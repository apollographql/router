//! GraphQL operation types for apollo-federation.
//!
//! ## Selection types
//! Each "conceptual" type consists of up to three actual types: a data type, an "element"
//! type, and a selection type.
//! - The data type records the data about the type. Things like a field name or fragment type
//! condition are in the data type. These types can be constructed and modified with plain rust.
//! - The element type contains the data type and maintains a key for the data. These types provide
//! APIs for modifications that keep the key up-to-date.
//! - The selection type contains the element type and, for composite fields, a subselection.
//!
//! For example, for fields, the data type is [`FieldData`], the element type is
//! [`Field`], and the selection type is [`FieldSelection`].

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::ops::Deref;
use std::sync::atomic;
use std::sync::Arc;
use std::sync::OnceLock;

use apollo_compiler::executable;
use apollo_compiler::executable::Name;
use apollo_compiler::name;
use apollo_compiler::validation::Valid;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;
use indexmap::IndexMap;
use indexmap::IndexSet;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::error::SingleFederationError::Internal;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::query_graph::graph_path::OpPathElement;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::FetchDataKeyRenamer;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::schema::definitions::is_composite_type;
use crate::schema::definitions::types_can_be_merged;
use crate::schema::definitions::AbstractType;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::ValidFederationSchema;

pub(crate) const TYPENAME_FIELD: Name = name!("__typename");

// Global storage for the counter used to uniquely identify selections
static NEXT_ID: atomic::AtomicUsize = atomic::AtomicUsize::new(1);

/// Opaque wrapper of the unique selection ID type.
///
/// Note that we shouldn't add `derive(Serialize, Deserialize)` to this without changing the types
/// to be something like UUIDs.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct SelectionId(usize);

impl SelectionId {
    pub(crate) fn new() -> Self {
        // atomically increment global counter
        Self(NEXT_ID.fetch_add(1, atomic::Ordering::AcqRel))
    }
}

/// Compare two input values, with two special cases for objects: assuming no duplicate keys,
/// and order-independence.
///
/// This comes from apollo-rs: https://github.com/apollographql/apollo-rs/blob/6825be88fe13cd0d67b83b0e4eb6e03c8ab2555e/crates/apollo-compiler/src/validation/selection.rs#L160-L188
/// Hopefully we can do this more easily in the future!
fn same_value(left: &executable::Value, right: &executable::Value) -> bool {
    use apollo_compiler::executable::Value;
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Enum(left), Value::Enum(right)) => left == right,
        (Value::Variable(left), Value::Variable(right)) => left == right,
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Float(left), Value::Float(right)) => left == right,
        (Value::Int(left), Value::Int(right)) => left == right,
        (Value::Boolean(left), Value::Boolean(right)) => left == right,
        (Value::List(left), Value::List(right)) => left
            .iter()
            .zip(right.iter())
            .all(|(left, right)| same_value(left, right)),
        (Value::Object(left), Value::Object(right)) if left.len() == right.len() => {
            left.iter().all(|(key, value)| {
                right
                    .iter()
                    .find(|(other_key, _)| key == other_key)
                    .is_some_and(|(_, other_value)| same_value(value, other_value))
            })
        }
        _ => false,
    }
}

/// Returns true if two argument lists are equivalent.
///
/// The arguments and values must be the same, independent of order.
fn same_arguments(
    left: &[Node<executable::Argument>],
    right: &[Node<executable::Argument>],
) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let right = right
        .iter()
        .map(|arg| (&arg.name, arg))
        .collect::<HashMap<_, _>>();

    left.iter().all(|arg| {
        right
            .get(&arg.name)
            .is_some_and(|right_arg| same_value(&arg.value, &right_arg.value))
    })
}

/// Returns true if two directive lists are equivalent.
fn same_directives(left: &executable::DirectiveList, right: &executable::DirectiveList) -> bool {
    if left.len() != right.len() {
        return false;
    }

    left.iter().all(|left_directive| {
        right.iter().any(|right_directive| {
            left_directive.name == right_directive.name
                && same_arguments(&left_directive.arguments, &right_directive.arguments)
        })
    })
}

/// An analogue of the apollo-compiler type `Operation` with these changes:
/// - Stores the schema that the operation is queried against.
/// - Swaps `operation_type` with `root_kind` (using the analogous apollo-federation type).
/// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
/// - Stores the fragments used by this operation (the executable document the operation was taken
///   from may contain other fragments that are not used by this operation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) root_kind: SchemaRootDefinitionKind,
    pub(crate) name: Option<Name>,
    pub(crate) variables: Arc<Vec<Node<executable::VariableDefinition>>>,
    pub(crate) directives: Arc<executable::DirectiveList>,
    pub(crate) selection_set: SelectionSet,
    pub(crate) named_fragments: NamedFragments,
}

pub(crate) struct NormalizedDefer {
    pub operation: Operation,
    pub has_defers: bool,
    pub assigned_defer_labels: HashSet<NodeStr>,
    pub defer_conditions: IndexMap<String, IndexSet<String>>,
}

impl Operation {
    /// Parse an operation from a source string.
    #[cfg(any(test, doc))]
    pub fn parse(
        schema: ValidFederationSchema,
        source_text: &str,
        source_name: &str,
        operation_name: Option<&str>,
    ) -> Result<Self, FederationError> {
        let document = apollo_compiler::ExecutableDocument::parse_and_validate(
            schema.schema(),
            source_text,
            source_name,
        )?;
        Operation::from_operation_document(schema, &document, operation_name)
    }

    pub fn from_operation_document(
        schema: ValidFederationSchema,
        document: &Valid<apollo_compiler::ExecutableDocument>,
        operation_name: Option<&str>,
    ) -> Result<Self, FederationError> {
        let operation = document.get_operation(operation_name).map_err(|_| {
            FederationError::internal(format!("No operation named {operation_name:?}"))
        })?;
        let named_fragments = NamedFragments::new(&document.fragments, &schema);
        let selection_set =
            SelectionSet::from_selection_set(&operation.selection_set, &named_fragments, &schema)?;
        Ok(Operation {
            schema,
            root_kind: operation.operation_type.into(),
            name: operation.name.clone(),
            variables: Arc::new(operation.variables.clone()),
            directives: Arc::new(operation.directives.clone()),
            selection_set,
            named_fragments,
        })
    }

    // PORT_NOTE(@goto-bus-stop): It might make sense for the returned data structure to *be* the
    // `DeferNormalizer` from the JS side
    pub(crate) fn with_normalized_defer(self) -> NormalizedDefer {
        if self.has_defer() {
            todo!("@defer not implemented");
        } else {
            NormalizedDefer {
                operation: self,
                has_defers: false,
                assigned_defer_labels: HashSet::new(),
                defer_conditions: IndexMap::new(),
            }
        }
    }

    fn has_defer(&self) -> bool {
        self.selection_set.has_defer()
            || self
                .named_fragments
                .fragments
                .values()
                .any(|f| f.has_defer())
    }

    pub(crate) fn without_defer(self) -> Self {
        if self.has_defer() {
            todo!("@defer not implemented");
        }

        self
    }
}

/// An analogue of the apollo-compiler type `SelectionSet` with these changes:
/// - For the type, stores the schema and the position in that schema instead of just the
///   `NamedType`.
/// - Stores selections in a map so they can be normalized efficiently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectionSet {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) type_position: CompositeTypeDefinitionPosition,
    pub(crate) selections: Arc<SelectionMap>,
}

pub(crate) mod normalized_selection_map {
    use std::borrow::Cow;
    use std::iter::Map;
    use std::ops::Deref;
    use std::sync::Arc;

    use apollo_compiler::ast::Name;
    use indexmap::IndexMap;

    use crate::error::FederationError;
    use crate::error::SingleFederationError::Internal;
    use crate::query_plan::operation::normalized_field_selection::FieldSelection;
    use crate::query_plan::operation::normalized_fragment_spread_selection::FragmentSpreadSelection;
    use crate::query_plan::operation::normalized_inline_fragment_selection::InlineFragmentSelection;
    use crate::query_plan::operation::HasSelectionKey;
    use crate::query_plan::operation::Selection;
    use crate::query_plan::operation::SelectionKey;
    use crate::query_plan::operation::SelectionSet;

    /// A "normalized" selection map is an optimized representation of a selection set which does
    /// not contain selections with the same selection "key". Selections that do have the same key
    /// are  merged during the normalization process. By storing a selection set as a map, we can
    /// efficiently merge/join multiple selection sets.
    ///
    /// Because the key depends strictly on the value, we expose the underlying map's API in a
    /// read-only capacity, while mutations use an API closer to `IndexSet`. We don't just use an
    /// `IndexSet` since key computation is expensive (it involves sorting). This type is in its own
    /// module to prevent code from accidentally mutating the underlying map outside the mutation
    /// API.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub(crate) struct SelectionMap(IndexMap<SelectionKey, Selection>);

    impl Deref for SelectionMap {
        type Target = IndexMap<SelectionKey, Selection>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl SelectionMap {
        pub(crate) fn new() -> Self {
            SelectionMap(IndexMap::new())
        }

        pub(crate) fn clear(&mut self) {
            self.0.clear();
        }

        pub(crate) fn insert(&mut self, value: Selection) -> Option<Selection> {
            self.0.insert(value.key(), value)
        }

        /// Insert a selection at a specific index.
        pub(crate) fn insert_at(&mut self, index: usize, value: Selection) -> Option<Selection> {
            self.0.shift_insert(index, value.key(), value)
        }

        /// Remove a selection from the map. Returns the selection and its numeric index.
        pub(crate) fn remove(&mut self, key: &SelectionKey) -> Option<(usize, Selection)> {
            // We specifically use shift_remove() instead of swap_remove() to maintain order.
            self.0
                .shift_remove_full(key)
                .map(|(index, _key, selection)| (index, selection))
        }

        pub(crate) fn retain(
            &mut self,
            mut predicate: impl FnMut(&SelectionKey, &Selection) -> bool,
        ) {
            self.0.retain(|k, v| predicate(k, v))
        }

        pub(crate) fn get_mut(&mut self, key: &SelectionKey) -> Option<SelectionValue> {
            self.0.get_mut(key).map(SelectionValue::new)
        }

        pub(crate) fn iter_mut(&mut self) -> IterMut {
            self.0.iter_mut().map(|(k, v)| (k, SelectionValue::new(v)))
        }

        pub(super) fn entry(&mut self, key: SelectionKey) -> Entry {
            match self.0.entry(key) {
                indexmap::map::Entry::Occupied(entry) => Entry::Occupied(OccupiedEntry(entry)),
                indexmap::map::Entry::Vacant(entry) => Entry::Vacant(VacantEntry(entry)),
            }
        }

        pub(crate) fn extend(&mut self, other: SelectionMap) {
            self.0.extend(other.0)
        }

        pub(crate) fn extend_ref(&mut self, other: &SelectionMap) {
            self.0
                .extend(other.iter().map(|(k, v)| (k.clone(), v.clone())))
        }

        /// Returns the selection set resulting from "recursively" filtering any selection
        /// that does not match the provided predicate.
        /// This method calls `predicate` on every selection of the selection set,
        /// not just top-level ones, and apply a "depth-first" strategy:
        /// when the predicate is called on a given selection it is guaranteed that
        /// filtering has happened on all the selections of its sub-selection.
        pub(crate) fn filter_recursive_depth_first(
            &self,
            predicate: &mut dyn FnMut(&Selection) -> Result<bool, FederationError>,
        ) -> Result<Cow<'_, Self>, FederationError> {
            fn recur_sub_selections<'sel>(
                selection: &'sel Selection,
                predicate: &mut dyn FnMut(&Selection) -> Result<bool, FederationError>,
            ) -> Result<Cow<'sel, Selection>, FederationError> {
                Ok(match selection {
                    Selection::Field(field) => {
                        if let Some(sub_selections) = &field.selection_set {
                            match sub_selections.filter_recursive_depth_first(predicate)? {
                                Cow::Borrowed(_) => Cow::Borrowed(selection),
                                Cow::Owned(new) => Cow::Owned(Selection::from_field(
                                    field.field.clone(),
                                    Some(new),
                                )),
                            }
                        } else {
                            Cow::Borrowed(selection)
                        }
                    }
                    Selection::InlineFragment(fragment) => match fragment
                        .selection_set
                        .filter_recursive_depth_first(predicate)?
                    {
                        Cow::Borrowed(_) => Cow::Borrowed(selection),
                        Cow::Owned(selection_set) => Cow::Owned(Selection::InlineFragment(
                            Arc::new(InlineFragmentSelection {
                                inline_fragment: fragment.inline_fragment.clone(),
                                selection_set,
                            }),
                        )),
                    },
                    Selection::FragmentSpread(_) => {
                        return Err(FederationError::internal("unexpected fragment spread"))
                    }
                })
            }
            let mut iter = self.0.iter();
            let mut enumerated = (&mut iter).enumerate();
            let mut new_map: IndexMap<_, _>;
            loop {
                let Some((index, (key, selection))) = enumerated.next() else {
                    return Ok(Cow::Borrowed(self));
                };
                let filtered = recur_sub_selections(selection, predicate)?;
                let keep = predicate(&filtered)?;
                if keep && matches!(filtered, Cow::Borrowed(_)) {
                    // Nothing changed so far, continue without cloning
                    continue;
                }

                // Clone the map so far
                new_map = self.0.as_slice()[..index]
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();

                if keep {
                    new_map.insert(key.clone(), filtered.into_owned());
                }
                break;
            }
            for (key, selection) in iter {
                let filtered = recur_sub_selections(selection, predicate)?;
                if predicate(&filtered)? {
                    new_map.insert(key.clone(), filtered.into_owned());
                }
            }
            Ok(Cow::Owned(Self(new_map)))
        }
    }

    impl<A> FromIterator<A> for SelectionMap
    where
        A: Into<Selection>,
    {
        fn from_iter<T: IntoIterator<Item = A>>(iter: T) -> Self {
            let mut map = Self::new();
            for selection in iter {
                map.insert(selection.into());
            }
            map
        }
    }

    type IterMut<'a> = Map<
        indexmap::map::IterMut<'a, SelectionKey, Selection>,
        fn((&'a SelectionKey, &'a mut Selection)) -> (&'a SelectionKey, SelectionValue<'a>),
    >;

    /// A mutable reference to a `Selection` value in a `SelectionMap`, which
    /// also disallows changing key-related data (to maintain the invariant that a value's key is
    /// the same as it's map entry's key).
    #[derive(Debug)]
    pub(crate) enum SelectionValue<'a> {
        Field(FieldSelectionValue<'a>),
        FragmentSpread(FragmentSpreadSelectionValue<'a>),
        InlineFragment(InlineFragmentSelectionValue<'a>),
    }

    impl<'a> SelectionValue<'a> {
        pub(crate) fn new(selection: &'a mut Selection) -> Self {
            match selection {
                Selection::Field(field_selection) => {
                    SelectionValue::Field(FieldSelectionValue::new(field_selection))
                }
                Selection::FragmentSpread(fragment_spread_selection) => {
                    SelectionValue::FragmentSpread(FragmentSpreadSelectionValue::new(
                        fragment_spread_selection,
                    ))
                }
                Selection::InlineFragment(inline_fragment_selection) => {
                    SelectionValue::InlineFragment(InlineFragmentSelectionValue::new(
                        inline_fragment_selection,
                    ))
                }
            }
        }
    }

    #[derive(Debug)]
    pub(crate) struct FieldSelectionValue<'a>(&'a mut Arc<FieldSelection>);

    impl<'a> FieldSelectionValue<'a> {
        pub(crate) fn new(field_selection: &'a mut Arc<FieldSelection>) -> Self {
            Self(field_selection)
        }

        pub(crate) fn get(&self) -> &Arc<FieldSelection> {
            self.0
        }

        pub(crate) fn get_sibling_typename_mut(&mut self) -> &mut Option<Name> {
            Arc::make_mut(self.0).field.sibling_typename_mut()
        }

        pub(crate) fn get_selection_set_mut(&mut self) -> &mut Option<SelectionSet> {
            &mut Arc::make_mut(self.0).selection_set
        }
    }

    #[derive(Debug)]
    pub(crate) struct FragmentSpreadSelectionValue<'a>(&'a mut Arc<FragmentSpreadSelection>);

    impl<'a> FragmentSpreadSelectionValue<'a> {
        pub(crate) fn new(fragment_spread_selection: &'a mut Arc<FragmentSpreadSelection>) -> Self {
            Self(fragment_spread_selection)
        }

        pub(crate) fn get(&self) -> &Arc<FragmentSpreadSelection> {
            self.0
        }
    }

    #[derive(Debug)]
    pub(crate) struct InlineFragmentSelectionValue<'a>(&'a mut Arc<InlineFragmentSelection>);

    impl<'a> InlineFragmentSelectionValue<'a> {
        pub(crate) fn new(inline_fragment_selection: &'a mut Arc<InlineFragmentSelection>) -> Self {
            Self(inline_fragment_selection)
        }

        pub(crate) fn get(&self) -> &Arc<InlineFragmentSelection> {
            self.0
        }

        pub(crate) fn get_selection_set_mut(&mut self) -> &mut SelectionSet {
            &mut Arc::make_mut(self.0).selection_set
        }
    }

    pub(crate) enum Entry<'a> {
        Occupied(OccupiedEntry<'a>),
        Vacant(VacantEntry<'a>),
    }

    impl<'a> Entry<'a> {
        pub fn or_insert(
            self,
            produce: impl FnOnce() -> Result<Selection, FederationError>,
        ) -> Result<SelectionValue<'a>, FederationError> {
            match self {
                Self::Occupied(entry) => Ok(entry.into_mut()),
                Self::Vacant(entry) => entry.insert(produce()?),
            }
        }
    }

    pub(crate) struct OccupiedEntry<'a>(indexmap::map::OccupiedEntry<'a, SelectionKey, Selection>);

    impl<'a> OccupiedEntry<'a> {
        pub(crate) fn get(&self) -> &Selection {
            self.0.get()
        }

        pub(crate) fn get_mut(&mut self) -> SelectionValue {
            SelectionValue::new(self.0.get_mut())
        }

        pub(crate) fn into_mut(self) -> SelectionValue<'a> {
            SelectionValue::new(self.0.into_mut())
        }

        pub(crate) fn key(&self) -> &SelectionKey {
            self.0.key()
        }

        pub(crate) fn remove(self) -> Selection {
            // We specifically use shift_remove() instead of swap_remove() to maintain order.
            self.0.shift_remove()
        }
    }

    pub(crate) struct VacantEntry<'a>(indexmap::map::VacantEntry<'a, SelectionKey, Selection>);

    impl<'a> VacantEntry<'a> {
        pub(crate) fn key(&self) -> &SelectionKey {
            self.0.key()
        }

        pub(crate) fn insert(
            self,
            value: Selection,
        ) -> Result<SelectionValue<'a>, FederationError> {
            if *self.key() != value.key() {
                return Err(Internal {
                    message: format!(
                        "Key mismatch when inserting selection {} into vacant entry ",
                        value
                    ),
                }
                .into());
            }
            Ok(SelectionValue::new(self.0.insert(value)))
        }
    }

    impl IntoIterator for SelectionMap {
        type Item = <IndexMap<SelectionKey, Selection> as IntoIterator>::Item;
        type IntoIter = <IndexMap<SelectionKey, Selection> as IntoIterator>::IntoIter;

        fn into_iter(self) -> Self::IntoIter {
            <IndexMap<SelectionKey, Selection> as IntoIterator>::into_iter(self.0)
        }
    }
}

pub(crate) use normalized_selection_map::FieldSelectionValue;
pub(crate) use normalized_selection_map::FragmentSpreadSelectionValue;
pub(crate) use normalized_selection_map::InlineFragmentSelectionValue;
pub(crate) use normalized_selection_map::SelectionMap;
pub(crate) use normalized_selection_map::SelectionValue;

/// A selection "key" (unrelated to the federation `@key` directive) is an identifier of a selection
/// (field, inline fragment, or fragment spread) that is used to determine whether two selections
/// can be merged.
///
/// In order to merge two selections they need to
/// * reference the same field/inline fragment
/// * specify the same directives
/// * directives have to be applied in the same order
/// * directive arguments order does not matter (they get automatically sorted by their names).
/// * selection cannot specify @defer directive
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SelectionKey {
    Field {
        /// The field alias (if specified) or field name in the resulting selection set.
        response_name: Name,
        /// directives applied on the field
        directives: Arc<executable::DirectiveList>,
    },
    FragmentSpread {
        /// The name of the fragment.
        fragment_name: Name,
        /// Directives applied on the fragment spread (does not contain @defer).
        directives: Arc<executable::DirectiveList>,
    },
    InlineFragment {
        /// The optional type condition of the fragment.
        type_condition: Option<Name>,
        /// Directives applied on the fragment spread (does not contain @defer).
        directives: Arc<executable::DirectiveList>,
    },
    Defer {
        /// Unique selection ID used to distinguish deferred fragment spreads that cannot be merged.
        deferred_id: SelectionId,
    },
}

impl SelectionKey {
    pub(crate) fn is_typename_field(&self) -> bool {
        matches!(self, SelectionKey::Field { response_name, .. } if *response_name == TYPENAME_FIELD)
    }
}

pub(crate) trait HasSelectionKey {
    fn key(&self) -> SelectionKey;
}

/// Options for the `.containment()` family of selection functions.
#[derive(Debug, Clone, Copy)]
pub struct ContainmentOptions {
    /// If the right-hand side has a __typename selection but the left-hand side does not,
    /// still consider the left-hand side to contain the right-hand side.
    pub ignore_missing_typename: bool,
}

// Currently Default *can* be derived, but if we add a new option
// here, that might no longer be true.
#[allow(clippy::derivable_impls)]
impl Default for ContainmentOptions {
    fn default() -> Self {
        Self {
            ignore_missing_typename: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    /// The left-hand selection does not fully contain right-hand selection.
    NotContained,
    /// The left-hand selection fully contains the right-hand selection, and more.
    StrictlyContained,
    /// Two selections are equal.
    Equal,
}
impl Containment {
    /// Returns true if the right-hand selection set is strictly contained or equal.
    pub fn is_contained(self) -> bool {
        matches!(self, Containment::StrictlyContained | Containment::Equal)
    }
}

/// An analogue of the apollo-compiler type `Selection` that stores our other selection analogues
/// instead of the apollo-compiler types.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::IsVariant)]
pub(crate) enum Selection {
    Field(Arc<FieldSelection>),
    FragmentSpread(Arc<FragmentSpreadSelection>),
    InlineFragment(Arc<InlineFragmentSelection>),
}

impl Selection {
    pub(crate) fn from_field(field: Field, sub_selections: Option<SelectionSet>) -> Self {
        Self::Field(Arc::new(field.with_subselection(sub_selections)))
    }

    pub(crate) fn from_inline_fragment(
        inline_fragment: InlineFragment,
        sub_selections: SelectionSet,
    ) -> Self {
        let inline_fragment_selection = InlineFragmentSelection {
            inline_fragment,
            selection_set: sub_selections,
        };
        Self::InlineFragment(Arc::new(inline_fragment_selection))
    }

    pub(crate) fn from_element(
        element: OpPathElement,
        sub_selections: Option<SelectionSet>,
    ) -> Result<Self, FederationError> {
        // PORT_NOTE: This is TODO item is copied from the JS `selectionOfElement` function.
        // TODO: validate that the subSelection is ok for the element
        match element {
            OpPathElement::Field(field) => Ok(Self::from_field(field, sub_selections)),
            OpPathElement::InlineFragment(inline_fragment) => {
                let Some(sub_selections) = sub_selections else {
                    return Err(FederationError::internal(
                        "unexpected inline fragment without sub-selections",
                    ));
                };
                Ok(Self::from_inline_fragment(inline_fragment, sub_selections))
            }
        }
    }

    pub(crate) fn schema(&self) -> &ValidFederationSchema {
        match self {
            Selection::Field(field_selection) => &field_selection.field.data().schema,
            Selection::FragmentSpread(fragment_spread_selection) => {
                &fragment_spread_selection.spread.data().schema
            }
            Selection::InlineFragment(inline_fragment_selection) => {
                &inline_fragment_selection.inline_fragment.data().schema
            }
        }
    }

    fn directives(&self) -> &Arc<executable::DirectiveList> {
        match self {
            Selection::Field(field_selection) => &field_selection.field.data().directives,
            Selection::FragmentSpread(fragment_spread_selection) => {
                &fragment_spread_selection.spread.data().directives
            }
            Selection::InlineFragment(inline_fragment_selection) => {
                &inline_fragment_selection.inline_fragment.data().directives
            }
        }
    }

    pub(crate) fn element(&self) -> Result<OpPathElement, FederationError> {
        match self {
            Selection::Field(field_selection) => {
                Ok(OpPathElement::Field(field_selection.field.clone()))
            }
            Selection::FragmentSpread(_) => Err(Internal {
                message: "Fragment spread does not have element".to_owned(),
            }
            .into()),
            Selection::InlineFragment(inline_fragment_selection) => Ok(
                OpPathElement::InlineFragment(inline_fragment_selection.inline_fragment.clone()),
            ),
        }
    }

    pub(crate) fn selection_set(&self) -> Result<Option<&SelectionSet>, FederationError> {
        match self {
            Selection::Field(field_selection) => Ok(field_selection.selection_set.as_ref()),
            Selection::FragmentSpread(_) => Err(Internal {
                message: "Fragment spread does not directly have a selection set".to_owned(),
            }
            .into()),
            Selection::InlineFragment(inline_fragment_selection) => {
                Ok(Some(&inline_fragment_selection.selection_set))
            }
        }
    }

    pub(crate) fn conditions(&self) -> Result<Conditions, FederationError> {
        let self_conditions = Conditions::from_directives(self.directives())?;
        if let Conditions::Boolean(false) = self_conditions {
            // Never included, so there is no point recursing.
            Ok(Conditions::Boolean(false))
        } else {
            match self {
                Selection::Field(_) => {
                    // The sub-selections of this field don't affect whether we should query this
                    // field, so we explicitly do not merge them in.
                    //
                    // PORT_NOTE: The JS codebase merges the sub-selections' conditions in with the
                    // field's conditions when field's selections are non-boolean. This is arguably
                    // a bug, so we've fixed it here.
                    Ok(self_conditions)
                }
                Selection::InlineFragment(inline) => {
                    Ok(self_conditions.merge(inline.selection_set.conditions()?))
                }
                Selection::FragmentSpread(_x) => Err(FederationError::internal(
                    "Unexpected fragment spread in Selection::conditions()",
                )),
            }
        }
    }

    pub(crate) fn collect_variables<'selection>(
        &'selection self,
        variables: &mut HashSet<&'selection Name>,
    ) -> Result<(), FederationError> {
        match self {
            Selection::Field(field) => field.collect_variables(variables),
            Selection::InlineFragment(inline_fragment) => {
                inline_fragment.collect_variables(variables)
            }
            Selection::FragmentSpread(_) => {
                Err(FederationError::internal("unexpected fragment spread"))
            }
        }
    }

    pub(crate) fn has_defer(&self) -> bool {
        match self {
            Selection::Field(field_selection) => field_selection.has_defer(),
            Selection::FragmentSpread(fragment_spread_selection) => {
                fragment_spread_selection.has_defer()
            }
            Selection::InlineFragment(inline_fragment_selection) => {
                inline_fragment_selection.has_defer()
            }
        }
    }

    fn collect_used_fragment_names(&self, aggregator: &mut HashMap<Name, i32>) {
        match self {
            Selection::Field(field_selection) => {
                if let Some(s) = field_selection.selection_set.clone() {
                    s.collect_used_fragment_names(aggregator)
                }
            }
            Selection::InlineFragment(inline) => {
                inline.selection_set.collect_used_fragment_names(aggregator);
            }
            Selection::FragmentSpread(fragment) => {
                let current_count = aggregator
                    .entry(fragment.spread.data().fragment_name.clone())
                    .or_default();
                *current_count += 1;
            }
        }
    }

    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Selection>, FederationError> {
        match self {
            Selection::Field(field) => {
                field.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
            Selection::FragmentSpread(spread) => {
                spread.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
            Selection::InlineFragment(inline) => {
                inline.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
        }
    }

    pub(crate) fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> bool {
        match self {
            Selection::Field(field) => field.can_add_to(parent_type, schema),
            Selection::FragmentSpread(_) => true,
            Selection::InlineFragment(inline) => inline.can_add_to(parent_type, schema),
        }
    }

    pub(crate) fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        option: NormalizeSelectionOption,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        match self {
            Selection::Field(field) => {
                field.normalize(parent_type, named_fragments, schema, option)
            }
            Selection::FragmentSpread(spread) => {
                spread.normalize(parent_type, named_fragments, schema)
            }
            Selection::InlineFragment(inline) => {
                inline.normalize(parent_type, named_fragments, schema, option)
            }
        }
    }

    pub(crate) fn with_updated_selection_set(
        &self,
        selection_set: Option<SelectionSet>,
    ) -> Result<Self, FederationError> {
        match self {
            Selection::Field(field) => Ok(Selection::from(
                field.with_updated_selection_set(selection_set),
            )),
            Selection::InlineFragment(inline_fragment) => {
                let Some(selection_set) = selection_set else {
                    return Err(FederationError::internal(
                        "updating inline fragment without a sub-selection set",
                    ));
                };
                Ok(inline_fragment
                    .with_updated_selection_set(selection_set)
                    .into())
            }
            Selection::FragmentSpread(_) => {
                Err(FederationError::internal("unexpected fragment spread"))
            }
        }
    }

    pub(crate) fn with_updated_selections<S: Into<Selection>>(
        &self,
        type_position: CompositeTypeDefinitionPosition,
        selections: impl IntoIterator<Item = S>,
    ) -> Result<Self, FederationError> {
        let new_sub_selection =
            SelectionSet::from_raw_selections(self.schema().clone(), type_position, selections);
        self.with_updated_selection_set(Some(new_sub_selection))
    }

    pub(crate) fn with_updated_directives(
        &self,
        directives: executable::DirectiveList,
    ) -> Result<Self, FederationError> {
        match self {
            Selection::Field(field) => Ok(Selection::Field(Arc::new(
                field.with_updated_directives(directives),
            ))),
            Selection::InlineFragment(inline_fragment) => Ok(Selection::InlineFragment(Arc::new(
                inline_fragment.with_updated_directives(directives),
            ))),
            Selection::FragmentSpread(_) => {
                Err(FederationError::internal("unexpected fragment spread"))
            }
        }
    }

    pub(crate) fn containment(
        &self,
        other: &Selection,
        options: ContainmentOptions,
    ) -> Containment {
        match (self, other) {
            (Selection::Field(self_field), Selection::Field(other_field)) => {
                self_field.containment(other_field, options)
            }
            (
                Selection::InlineFragment(self_fragment),
                Selection::InlineFragment(_) | Selection::FragmentSpread(_),
            ) => self_fragment.containment(other, options),
            (
                Selection::FragmentSpread(self_fragment),
                Selection::InlineFragment(_) | Selection::FragmentSpread(_),
            ) => self_fragment.containment(other, options),
            _ => Containment::NotContained,
        }
    }

    /// Returns true if this selection is a superset of the other selection.
    pub(crate) fn contains(&self, other: &Selection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }

    /// Apply the `mapper` to self.selection_set, if it exists, and return a new `Selection`.
    /// - Note: The returned selection may have no subselection set or an empty one if the mapper
    ///         returns so, which may make the returned selection invalid. It's caller's responsibility
    ///         to appropriately handle invalid return values.
    pub(crate) fn map_selection_set(
        &self,
        mapper: impl FnOnce(&SelectionSet) -> Result<Option<SelectionSet>, FederationError>,
    ) -> Result<Self, FederationError> {
        if let Some(selection_set) = self.selection_set()? {
            self.with_updated_selection_set(mapper(selection_set)?)
        } else {
            // selection has no (sub-)selection set.
            Ok(self.clone())
        }
    }
}

impl From<FieldSelection> for Selection {
    fn from(value: FieldSelection) -> Self {
        Self::Field(value.into())
    }
}

impl From<FragmentSpreadSelection> for Selection {
    fn from(value: FragmentSpreadSelection) -> Self {
        Self::FragmentSpread(value.into())
    }
}

impl From<InlineFragmentSelection> for Selection {
    fn from(value: InlineFragmentSelection) -> Self {
        Self::InlineFragment(value.into())
    }
}

impl HasSelectionKey for Selection {
    fn key(&self) -> SelectionKey {
        match self {
            Selection::Field(field_selection) => field_selection.key(),
            Selection::FragmentSpread(fragment_spread_selection) => fragment_spread_selection.key(),
            Selection::InlineFragment(inline_fragment_selection) => inline_fragment_selection.key(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, derive_more::From)]
pub(crate) enum SelectionOrSet {
    Selection(Selection),
    SelectionSet(SelectionSet),
}

/// An analogue of the apollo-compiler type `Fragment` with these changes:
/// - Stores the type condition explicitly, which means storing the schema and position (in
///   apollo-compiler, this is in the `SelectionSet`).
/// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fragment {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) name: Name,
    pub(crate) type_condition_position: CompositeTypeDefinitionPosition,
    pub(crate) directives: Arc<executable::DirectiveList>,
    pub(crate) selection_set: SelectionSet,
}

impl Fragment {
    fn from_fragment(
        fragment: &executable::Fragment,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<Self, FederationError> {
        Ok(Self {
            schema: schema.clone(),
            name: fragment.name.clone(),
            type_condition_position: schema
                .get_type(fragment.type_condition().clone())?
                .try_into()?,
            directives: Arc::new(fragment.directives.clone()),
            selection_set: SelectionSet::from_selection_set(
                &fragment.selection_set,
                named_fragments,
                schema,
            )?,
        })
    }

    // PORT NOTE: in JS code this is stored on the fragment
    pub(crate) fn fragment_usages(&self) -> HashMap<Name, i32> {
        let mut usages = HashMap::new();
        self.selection_set.collect_used_fragment_names(&mut usages);
        usages
    }

    // PORT NOTE: in JS code this is stored on the fragment
    pub(crate) fn collect_used_fragment_names(&self, aggregator: &mut HashMap<Name, i32>) {
        self.selection_set.collect_used_fragment_names(aggregator)
    }

    fn has_defer(&self) -> bool {
        self.selection_set.has_defer()
    }
}

mod normalized_field_selection {
    use std::collections::HashSet;
    use std::sync::Arc;

    use apollo_compiler::ast;
    use apollo_compiler::executable;
    use apollo_compiler::executable::Name;
    use apollo_compiler::Node;

    use crate::error::FederationError;
    use crate::query_graph::graph_path::OpPathElement;
    use crate::query_plan::operation::directives_with_sorted_arguments;
    use crate::query_plan::operation::HasSelectionKey;
    use crate::query_plan::operation::SelectionKey;
    use crate::query_plan::operation::SelectionSet;
    use crate::query_plan::FetchDataPathElement;
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::position::FieldDefinitionPosition;
    use crate::schema::position::TypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;

    /// An analogue of the apollo-compiler type `Field` with these changes:
    /// - Makes the selection set optional. This is because `SelectionSet` requires a type of
    ///   `CompositeTypeDefinitionPosition`, which won't exist for fields returning a non-composite type
    ///   (scalars and enums).
    /// - Stores the field data (other than the selection set) in `Field`, to facilitate
    ///   operation paths and graph paths.
    /// - For the field definition, stores the schema and the position in that schema instead of just
    ///   the `FieldDefinition` (which contains no references to the parent type or schema).
    /// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct FieldSelection {
        pub(crate) field: Field,
        pub(crate) selection_set: Option<SelectionSet>,
    }

    impl HasSelectionKey for FieldSelection {
        fn key(&self) -> SelectionKey {
            self.field.key()
        }
    }

    impl FieldSelection {
        pub(crate) fn with_updated_selection_set(
            &self,
            selection_set: Option<SelectionSet>,
        ) -> Self {
            Self {
                field: self.field.clone(),
                selection_set,
            }
        }

        pub(crate) fn with_updated_directives(
            &self,
            directives: executable::DirectiveList,
        ) -> Self {
            Self {
                field: self.field.with_updated_directives(directives),
                selection_set: self.selection_set.clone(),
            }
        }

        pub(crate) fn element(&self) -> OpPathElement {
            OpPathElement::Field(self.field.clone())
        }

        pub(crate) fn with_updated_alias(&self, alias: Name) -> Field {
            let mut data = self.field.data().clone();
            data.alias = Some(alias);
            Field::new(data)
        }

        pub(crate) fn collect_variables<'selection>(
            &'selection self,
            variables: &mut HashSet<&'selection Name>,
        ) -> Result<(), FederationError> {
            self.field.collect_variables(variables);
            if let Some(set) = &self.selection_set {
                set.collect_variables(variables)?
            }
            Ok(())
        }
    }

    /// The non-selection-set data of `FieldSelection`, used with operation paths and graph
    /// paths.
    #[derive(Clone, PartialEq, Eq, Hash)]
    pub(crate) struct Field {
        data: FieldData,
        key: SelectionKey,
    }

    impl std::fmt::Debug for Field {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.data.fmt(f)
        }
    }

    impl Field {
        pub(crate) fn new(data: FieldData) -> Self {
            Self {
                key: data.key(),
                data,
            }
        }

        /// Create a trivial field selection without any arguments or directives.
        pub(crate) fn from_position(
            schema: &ValidFederationSchema,
            field_position: FieldDefinitionPosition,
        ) -> Self {
            Self::new(FieldData::from_position(schema, field_position))
        }

        pub(crate) fn new_introspection_typename(
            schema: &ValidFederationSchema,
            parent_type: &CompositeTypeDefinitionPosition,
            alias: Option<Name>,
        ) -> Self {
            Self::new(FieldData {
                schema: schema.clone(),
                field_position: parent_type.introspection_typename_field(),
                alias,
                arguments: Default::default(),
                directives: Default::default(),
                sibling_typename: None,
            })
        }

        /// Turn this `Field` into a `FieldSelection` with the given sub-selection. If this is
        /// meant to be a leaf selection, use `None`.
        pub(crate) fn with_subselection(
            self,
            selection_set: Option<SelectionSet>,
        ) -> FieldSelection {
            FieldSelection {
                field: self,
                selection_set,
            }
        }

        pub(crate) fn schema(&self) -> &ValidFederationSchema {
            &self.data.schema
        }

        pub(crate) fn data(&self) -> &FieldData {
            &self.data
        }

        pub(crate) fn sibling_typename(&self) -> Option<&Name> {
            self.data.sibling_typename.as_ref()
        }

        pub(crate) fn sibling_typename_mut(&mut self) -> &mut Option<Name> {
            &mut self.data.sibling_typename
        }

        pub(crate) fn with_updated_directives(
            &self,
            directives: executable::DirectiveList,
        ) -> Field {
            let mut data = self.data.clone();
            data.directives = Arc::new(directives);
            Self::new(data)
        }

        pub(crate) fn as_path_element(&self) -> FetchDataPathElement {
            FetchDataPathElement::Key(self.data().response_name().into())
        }

        pub(crate) fn collect_variables<'selection>(
            &'selection self,
            variables: &mut HashSet<&'selection Name>,
        ) {
            for arg in self.data().arguments.iter() {
                collect_variables_from_argument(arg, variables)
            }
            for dir in self.data().directives.iter() {
                collect_variables_from_directive(dir, variables)
            }
        }
    }

    pub(crate) fn collect_variables_from_argument<'selection>(
        argument: &'selection executable::Argument,
        variables: &mut HashSet<&'selection Name>,
    ) {
        if let Some(v) = argument.value.as_variable() {
            variables.insert(v);
        }
    }

    pub(crate) fn collect_variables_from_directive<'selection>(
        directive: &'selection executable::Directive,
        variables: &mut HashSet<&'selection Name>,
    ) {
        for arg in directive.arguments.iter() {
            collect_variables_from_argument(arg, variables)
        }
    }

    impl HasSelectionKey for Field {
        fn key(&self) -> SelectionKey {
            self.key.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct FieldData {
        pub(crate) schema: ValidFederationSchema,
        pub(crate) field_position: FieldDefinitionPosition,
        pub(crate) alias: Option<Name>,
        pub(crate) arguments: Arc<Vec<Node<executable::Argument>>>,
        pub(crate) directives: Arc<executable::DirectiveList>,
        pub(crate) sibling_typename: Option<Name>,
    }

    impl FieldData {
        /// Create a trivial field selection without any arguments or directives.
        pub fn from_position(
            schema: &ValidFederationSchema,
            field_position: FieldDefinitionPosition,
        ) -> Self {
            Self {
                schema: schema.clone(),
                field_position,
                alias: None,
                arguments: Default::default(),
                directives: Default::default(),
                sibling_typename: None,
            }
        }

        pub(crate) fn name(&self) -> &Name {
            self.field_position.field_name()
        }

        pub(crate) fn response_name(&self) -> Name {
            self.alias.clone().unwrap_or_else(|| self.name().clone())
        }

        fn output_ast_type(&self) -> Result<&ast::Type, FederationError> {
            Ok(&self.field_position.get(self.schema.schema())?.ty)
        }

        pub(crate) fn output_base_type(&self) -> Result<TypeDefinitionPosition, FederationError> {
            let definition = self.field_position.get(self.schema.schema())?;
            self.schema
                .get_type(definition.ty.inner_named_type().clone())
        }

        pub(crate) fn is_leaf(&self) -> Result<bool, FederationError> {
            let base_type_position = self.output_base_type()?;
            Ok(matches!(
                base_type_position,
                TypeDefinitionPosition::Scalar(_) | TypeDefinitionPosition::Enum(_)
            ))
        }
    }

    impl HasSelectionKey for FieldData {
        fn key(&self) -> SelectionKey {
            SelectionKey::Field {
                response_name: self.response_name(),
                directives: Arc::new(directives_with_sorted_arguments(&self.directives)),
            }
        }
    }
}

pub(crate) use normalized_field_selection::Field;
pub(crate) use normalized_field_selection::FieldData;
pub(crate) use normalized_field_selection::FieldSelection;

mod normalized_fragment_spread_selection {
    use std::sync::Arc;

    use apollo_compiler::executable;
    use apollo_compiler::executable::Name;

    use crate::query_plan::operation::directives_with_sorted_arguments;
    use crate::query_plan::operation::is_deferred_selection;
    use crate::query_plan::operation::HasSelectionKey;
    use crate::query_plan::operation::SelectionId;
    use crate::query_plan::operation::SelectionKey;
    use crate::query_plan::operation::SelectionSet;
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct FragmentSpreadSelection {
        pub(crate) spread: FragmentSpread,
        pub(crate) selection_set: SelectionSet,
    }

    impl HasSelectionKey for FragmentSpreadSelection {
        fn key(&self) -> SelectionKey {
            self.spread.key()
        }
    }

    /// An analogue of the apollo-compiler type `FragmentSpread` with these changes:
    /// - Stores the schema (may be useful for directives).
    /// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
    #[derive(Clone, PartialEq, Eq)]
    pub(crate) struct FragmentSpread {
        data: FragmentSpreadData,
        key: SelectionKey,
    }

    impl std::fmt::Debug for FragmentSpread {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.data.fmt(f)
        }
    }

    impl FragmentSpread {
        pub(crate) fn new(data: FragmentSpreadData) -> Self {
            Self {
                key: data.key(),
                data,
            }
        }

        pub(crate) fn data(&self) -> &FragmentSpreadData {
            &self.data
        }
    }

    impl HasSelectionKey for FragmentSpread {
        fn key(&self) -> SelectionKey {
            self.key.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct FragmentSpreadData {
        pub(crate) schema: ValidFederationSchema,
        pub(crate) fragment_name: Name,
        pub(crate) type_condition_position: CompositeTypeDefinitionPosition,
        // directives applied on the fragment spread selection
        pub(crate) directives: Arc<executable::DirectiveList>,
        // directives applied within the fragment definition
        //
        // PORT_NOTE: The JS codebase combined the fragment spread's directives with the fragment
        // definition's directives. This was invalid GraphQL as those directives may not be applicable
        // on different locations. While we now keep track of those references, they are currently ignored.
        pub(crate) fragment_directives: Arc<executable::DirectiveList>,
        pub(crate) selection_id: SelectionId,
    }

    impl HasSelectionKey for FragmentSpreadData {
        fn key(&self) -> SelectionKey {
            if is_deferred_selection(&self.directives) {
                SelectionKey::Defer {
                    deferred_id: self.selection_id.clone(),
                }
            } else {
                SelectionKey::FragmentSpread {
                    fragment_name: self.fragment_name.clone(),
                    directives: Arc::new(directives_with_sorted_arguments(&self.directives)),
                }
            }
        }
    }
}

pub(crate) use normalized_fragment_spread_selection::FragmentSpread;
pub(crate) use normalized_fragment_spread_selection::FragmentSpreadData;
pub(crate) use normalized_fragment_spread_selection::FragmentSpreadSelection;

impl FragmentSpreadSelection {
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Selection>, FederationError> {
        // We preserve the parent type here, to make sure we don't lose context, but we actually don't
        // want to expand the spread as that would compromise the code that optimize subgraph fetches to re-use named
        // fragments.
        //
        // This is a little bit iffy, because the fragment may not apply at this parent type, but we
        // currently leave it to the caller to ensure this is not a mistake. But most of the
        // QP code works on selections with fully expanded fragments, so this code (and that of `can_add_to`
        // on come into play in the code for reusing fragments, and that code calls those methods
        // appropriately.
        if self.spread.data().schema == *schema
            && self.spread.data().type_condition_position == *parent_type
        {
            return Ok(Some(Selection::FragmentSpread(Arc::new(self.clone()))));
        }

        // If we're rebasing on a _different_ schema, then we *must* have fragments, since reusing
        // `self.fragments` would be incorrect. If we're on the same schema though, we're happy to default
        // to `self.fragments`.
        let rebase_on_same_schema = self.spread.data().schema == *schema;
        let Some(named_fragment) = named_fragments.get(&self.spread.data().fragment_name) else {
            // If we're rebasing on another schema (think a subgraph), then named fragments will have been rebased on that, and some
            // of them may not contain anything that is on that subgraph, in which case they will not have been included at all.
            // If so, then as long as we're not asked to error if we cannot rebase, then we're happy to skip that spread (since again,
            // it expands to nothing that applies on the schema).
            return if let RebaseErrorHandlingOption::ThrowError = error_handling {
                Err(FederationError::internal(format!(
                    "Cannot rebase {} fragment if it isn't part of the provided fragments",
                    self.spread.data().fragment_name
                )))
            } else {
                Ok(None)
            };
        };

        // Lastly, if we rebase on a different schema, it's possible the fragment type does not intersect the
        // parent type. For instance, the parent type could be some object type T while the fragment is an
        // interface I, and T may implement I in the supergraph, but not in a particular subgraph (of course,
        // if I doesn't exist at all in the subgraph, then we'll have exited above, but I may exist in the
        // subgraph, just not be implemented by T for some reason). In that case, we can't reuse the fragment
        // as its spread is essentially invalid in that position, so we have to replace it by the expansion
        // of that fragment, which we rebase on the parentType (which in turn, will remove anythings within
        // the fragment selection that needs removing, potentially everything).
        if !rebase_on_same_schema
            && !runtime_types_intersect(
                parent_type,
                &named_fragment.type_condition_position,
                schema,
            )
        {
            // Note that we've used the rebased `named_fragment` to check the type intersection because we needed to
            // compare runtime types "for the schema we're rebasing into". But now that we're deciding to not reuse
            // this rebased fragment, what we rebase is the selection set of the non-rebased fragment. And that's
            // important because the very logic we're hitting here may need to happen inside the rebase on the
            // fragment selection, but that logic would not be triggered if we used the rebased `named_fragment` since
            // `rebase_on_same_schema` would then be 'true'.
            let expanded_selection_set = self.selection_set.rebase_on(
                parent_type,
                named_fragments,
                schema,
                error_handling,
            )?;
            // In theory, we could return the selection set directly, but making `SelectionSet.rebase_on` sometimes
            // return a `SelectionSet` complicate things quite a bit. So instead, we encapsulate the selection set
            // in an "empty" inline fragment. This make for non-really-optimal selection sets in the (relatively
            // rare) case where this is triggered, but in practice this "inefficiency" is removed by future calls
            // to `normalize`.
            return if expanded_selection_set.selections.is_empty() {
                Ok(None)
            } else {
                Ok(Some(Selection::from_inline_fragment(
                    InlineFragment::new(InlineFragmentData {
                        schema: schema.clone(),
                        parent_type_position: parent_type.clone(),
                        type_condition_position: None,
                        directives: Default::default(),
                        selection_id: SelectionId::new(),
                    }),
                    expanded_selection_set,
                )))
            };
        }

        let spread = FragmentSpread::new(FragmentSpreadData::from_fragment(
            &named_fragment,
            &self.spread.data().directives,
        ));
        Ok(Some(Selection::FragmentSpread(Arc::new(
            FragmentSpreadSelection {
                spread,
                selection_set: named_fragment.selection_set.clone(),
            },
        ))))
    }

    pub(crate) fn has_defer(&self) -> bool {
        self.spread.data().directives.has("defer") || self.selection_set.has_defer()
    }

    /// Copies fragment spread selection and assigns it a new unique selection ID.
    pub(crate) fn with_unique_id(&self) -> Self {
        let mut data = self.spread.data().clone();
        data.selection_id = SelectionId::new();
        Self {
            spread: FragmentSpread::new(data),
            selection_set: self.selection_set.clone(),
        }
    }

    /// Normalize this fragment spread into a "normalized" spread representation with following
    /// modifications
    /// - Stores the schema (may be useful for directives).
    /// - Encloses list of directives in `Arc`s to facilitate cheaper cloning.
    /// - Stores unique selection ID (used for deferred fragments)
    pub(crate) fn from_fragment_spread(
        fragment_spread: &executable::FragmentSpread,
        fragment: &Node<Fragment>,
    ) -> Result<FragmentSpreadSelection, FederationError> {
        let spread_data = FragmentSpreadData::from_fragment(fragment, &fragment_spread.directives);
        Ok(FragmentSpreadSelection {
            spread: FragmentSpread::new(spread_data),
            selection_set: fragment.selection_set.clone(),
        })
    }

    pub(crate) fn from_fragment(
        fragment: &Node<Fragment>,
        directives: &executable::DirectiveList,
    ) -> Self {
        let spread_data = FragmentSpreadData::from_fragment(fragment, directives);
        Self {
            spread: FragmentSpread::new(spread_data),
            selection_set: fragment.selection_set.clone(),
        }
    }

    pub(crate) fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        let this_condition = self.spread.data().type_condition_position.clone();
        // This method assumes by contract that `parent_type` runtimes intersects `self.inline_fragment.data().parent_type_position`'s,
        // but `parent_type` runtimes may be a subset. So first check if the selection should not be discarded on that account (that
        // is, we should not keep the selection if its condition runtimes don't intersect at all with those of
        // `parent_type` as that would ultimately make an invalid selection set).
        if (self.spread.data().schema != *schema || this_condition != *parent_type)
            && !runtime_types_intersect(&this_condition, parent_type, schema)
        {
            return Ok(None);
        }

        // We must update the spread parent type if necessary since we're not going deeper,
        // or we'll be fundamentally losing context.
        if self.spread.data().schema != *schema {
            return Err(FederationError::internal(
                "Should not try to normalize using a type from another schema",
            ));
        }

        if let Some(rebased_fragment_spread) = self.rebase_on(
            parent_type,
            named_fragments,
            schema,
            RebaseErrorHandlingOption::ThrowError,
        )? {
            Ok(Some(SelectionOrSet::Selection(rebased_fragment_spread)))
        } else {
            unreachable!("We should always be able to either rebase the fragment spread OR throw an exception");
        }
    }

    pub(crate) fn containment(
        &self,
        other: &Selection,
        options: ContainmentOptions,
    ) -> Containment {
        match other {
            // Using keys here means that @defer fragments never compare equal.
            // This is a bit odd but it is consistent: the selection set data structure would not
            // even try to compare two @defer fragments, because their keys are different.
            Selection::FragmentSpread(other) if self.spread.key() == other.spread.key() => self
                .selection_set
                .containment(&other.selection_set, options),
            _ => Containment::NotContained,
        }
    }

    /// Returns true if this selection is a superset of the other selection.
    pub(crate) fn contains(&self, other: &Selection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl FragmentSpreadData {
    pub(crate) fn from_fragment(
        fragment: &Node<Fragment>,
        spread_directives: &executable::DirectiveList,
    ) -> FragmentSpreadData {
        FragmentSpreadData {
            schema: fragment.schema.clone(),
            fragment_name: fragment.name.clone(),
            type_condition_position: fragment.type_condition_position.clone(),
            directives: Arc::new(spread_directives.clone()),
            fragment_directives: fragment.directives.clone(),
            selection_id: SelectionId::new(),
        }
    }
}

mod normalized_inline_fragment_selection {
    use std::collections::HashSet;
    use std::sync::Arc;

    use apollo_compiler::executable;
    use apollo_compiler::executable::Name;

    use super::normalized_field_selection::collect_variables_from_directive;
    use crate::error::FederationError;
    use crate::link::graphql_definition::defer_directive_arguments;
    use crate::link::graphql_definition::DeferDirectiveArguments;
    use crate::query_plan::operation::directives_with_sorted_arguments;
    use crate::query_plan::operation::is_deferred_selection;
    use crate::query_plan::operation::runtime_types_intersect;
    use crate::query_plan::operation::HasSelectionKey;
    use crate::query_plan::operation::SelectionId;
    use crate::query_plan::operation::SelectionKey;
    use crate::query_plan::operation::SelectionSet;
    use crate::query_plan::FetchDataPathElement;
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;

    /// An analogue of the apollo-compiler type `InlineFragment` with these changes:
    /// - Stores the inline fragment data (other than the selection set) in `InlineFragment`,
    ///   to facilitate operation paths and graph paths.
    /// - For the type condition, stores the schema and the position in that schema instead of just
    ///   the `NamedType`.
    /// - Stores the parent type explicitly, which means storing the position (in apollo-compiler, this
    ///   is in the parent selection set).
    /// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct InlineFragmentSelection {
        pub(crate) inline_fragment: InlineFragment,
        pub(crate) selection_set: SelectionSet,
    }

    impl InlineFragmentSelection {
        pub(crate) fn with_updated_selection_set(&self, selection_set: SelectionSet) -> Self {
            Self {
                inline_fragment: self.inline_fragment.clone(),
                selection_set,
            }
        }

        pub(crate) fn with_updated_directives(
            &self,
            directives: executable::DirectiveList,
        ) -> Self {
            Self {
                inline_fragment: self.inline_fragment.with_updated_directives(directives),
                selection_set: self.selection_set.clone(),
            }
        }

        pub(crate) fn collect_variables<'selection>(
            &'selection self,
            variables: &mut HashSet<&'selection Name>,
        ) -> Result<(), FederationError> {
            self.inline_fragment.collect_variables(variables);
            self.selection_set.collect_variables(variables)
        }
    }

    impl HasSelectionKey for InlineFragmentSelection {
        fn key(&self) -> SelectionKey {
            self.inline_fragment.key()
        }
    }

    /// The non-selection-set data of `InlineFragmentSelection`, used with operation paths and
    /// graph paths.
    #[derive(Clone, PartialEq, Eq, Hash)]
    pub(crate) struct InlineFragment {
        data: InlineFragmentData,
        key: SelectionKey,
    }

    impl std::fmt::Debug for InlineFragment {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.data.fmt(f)
        }
    }

    impl InlineFragment {
        pub(crate) fn new(data: InlineFragmentData) -> Self {
            Self {
                key: data.key(),
                data,
            }
        }

        pub(crate) fn schema(&self) -> &ValidFederationSchema {
            &self.data.schema
        }

        pub(crate) fn data(&self) -> &InlineFragmentData {
            &self.data
        }

        pub(crate) fn with_updated_type_condition(
            &self,
            new: Option<CompositeTypeDefinitionPosition>,
        ) -> Self {
            let mut data = self.data().clone();
            data.type_condition_position = new;
            Self::new(data)
        }
        pub(crate) fn with_updated_directives(
            &self,
            directives: executable::DirectiveList,
        ) -> InlineFragment {
            let mut data = self.data().clone();
            data.directives = Arc::new(directives);
            Self::new(data)
        }

        pub(crate) fn as_path_element(&self) -> Option<FetchDataPathElement> {
            let condition = self.data().type_condition_position.clone()?;

            Some(FetchDataPathElement::TypenameEquals(
                condition.type_name().clone().into(),
            ))
        }

        pub(crate) fn collect_variables<'selection>(
            &'selection self,
            variables: &mut HashSet<&'selection Name>,
        ) {
            for dir in self.data.directives.iter() {
                collect_variables_from_directive(dir, variables)
            }
        }
    }

    impl HasSelectionKey for InlineFragment {
        fn key(&self) -> SelectionKey {
            self.key.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct InlineFragmentData {
        pub(crate) schema: ValidFederationSchema,
        pub(crate) parent_type_position: CompositeTypeDefinitionPosition,
        pub(crate) type_condition_position: Option<CompositeTypeDefinitionPosition>,
        pub(crate) directives: Arc<executable::DirectiveList>,
        pub(crate) selection_id: SelectionId,
    }

    impl InlineFragmentData {
        pub(crate) fn defer_directive_arguments(
            &self,
        ) -> Result<Option<DeferDirectiveArguments>, FederationError> {
            if let Some(directive) = self.directives.get("defer") {
                Ok(Some(defer_directive_arguments(directive)?))
            } else {
                Ok(None)
            }
        }

        pub(super) fn casted_type_if_add_to(
            &self,
            parent_type: &CompositeTypeDefinitionPosition,
            schema: &ValidFederationSchema,
        ) -> Option<CompositeTypeDefinitionPosition> {
            if &self.parent_type_position == parent_type && &self.schema == schema {
                return Some(self.casted_type());
            }
            match self.can_rebase_on(parent_type) {
                (false, _) => None,
                (true, None) => Some(parent_type.clone()),
                (true, Some(ty)) => Some(ty),
            }
        }

        pub(crate) fn casted_type(&self) -> CompositeTypeDefinitionPosition {
            self.type_condition_position
                .clone()
                .unwrap_or_else(|| self.parent_type_position.clone())
        }

        fn can_rebase_on(
            &self,
            parent_type: &CompositeTypeDefinitionPosition,
        ) -> (bool, Option<CompositeTypeDefinitionPosition>) {
            let Some(ty) = self.type_condition_position.as_ref() else {
                return (true, None);
            };
            match self
                .schema
                .get_type(ty.type_name().clone())
                .and_then(CompositeTypeDefinitionPosition::try_from)
            {
                Ok(ty) if runtime_types_intersect(parent_type, &ty, &self.schema) => {
                    (true, Some(ty))
                }
                _ => (false, None),
            }
        }
    }

    impl HasSelectionKey for InlineFragmentData {
        fn key(&self) -> SelectionKey {
            if is_deferred_selection(&self.directives) {
                SelectionKey::Defer {
                    deferred_id: self.selection_id.clone(),
                }
            } else {
                SelectionKey::InlineFragment {
                    type_condition: self
                        .type_condition_position
                        .as_ref()
                        .map(|pos| pos.type_name().clone()),
                    directives: Arc::new(directives_with_sorted_arguments(&self.directives)),
                }
            }
        }
    }
}

pub(crate) use normalized_inline_fragment_selection::InlineFragment;
pub(crate) use normalized_inline_fragment_selection::InlineFragmentData;
pub(crate) use normalized_inline_fragment_selection::InlineFragmentSelection;

use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;

/// A simple MultiMap implementation using IndexMap with Vec<V> as its value type.
/// - Preserves the insertion order of keys and values.
struct MultiIndexMap<K, V>(IndexMap<K, Vec<V>>);

impl<K, V> Deref for MultiIndexMap<K, V> {
    type Target = IndexMap<K, Vec<V>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K, V> MultiIndexMap<K, V>
where
    K: Eq + Hash,
{
    fn new() -> Self {
        Self(IndexMap::new())
    }

    fn insert(&mut self, key: K, value: V) {
        self.0.entry(key).or_default().push(value);
    }

    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iterable: I) {
        for (key, value) in iterable {
            self.insert(key, value);
        }
    }
}

/// the return type of `lazy_map` function's `mapper` closure argument
#[derive(derive_more::From)]
pub(crate) enum SelectionMapperReturn {
    None,
    Selection(Selection),
    SelectionList(Vec<Selection>),
}

impl FromIterator<Selection> for SelectionMapperReturn {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = Selection>,
    {
        Self::SelectionList(Vec::from_iter(iter))
    }
}

impl SelectionSet {
    pub(crate) fn empty(
        schema: ValidFederationSchema,
        type_position: CompositeTypeDefinitionPosition,
    ) -> Self {
        Self {
            schema,
            type_position,
            selections: Default::default(),
        }
    }

    // TODO: Ideally, this method returns a proper, recursive iterator. As is, there is a lot of
    // overhead due to indirection, both from over allocation and from v-table lookups.
    pub(crate) fn split_top_level_fields(self) -> Box<dyn Iterator<Item = SelectionSet>> {
        let parent_type = self.type_position.clone();
        let selections: IndexMap<SelectionKey, Selection> = (**self.selections).clone();
        Box::new(selections.into_values().flat_map(move |sel| {
            let digest: Box<dyn Iterator<Item = SelectionSet>> = if sel.is_field() {
                Box::new(std::iter::once(SelectionSet::from_selection(
                    parent_type.clone(),
                    sel.clone(),
                )))
            } else {
                let Some(ele) = sel.element().ok() else {
                    let digest: Box<dyn Iterator<Item = SelectionSet>> =
                        Box::new(std::iter::empty());
                    return digest;
                };
                Box::new(
                    sel.selection_set()
                        .ok()
                        .flatten()
                        .cloned()
                        .into_iter()
                        .flat_map(SelectionSet::split_top_level_fields)
                        .filter_map(move |set| {
                            let parent_type = ele.parent_type_position();
                            Selection::from_element(ele.clone(), Some(set))
                                .ok()
                                .map(|sel| SelectionSet::from_selection(parent_type, sel))
                        }),
                )
            };
            digest
        }))
    }

    /// PORT_NOTE: JS calls this `newCompositeTypeSelectionSet`
    pub(crate) fn for_composite_type(
        schema: ValidFederationSchema,
        type_position: CompositeTypeDefinitionPosition,
    ) -> Self {
        let typename_field = Field::new_introspection_typename(&schema, &type_position, None)
            .with_subselection(None);
        Self {
            schema,
            type_position,
            selections: Arc::new(std::iter::once(typename_field).collect()),
        }
    }

    /// Build a selection set from a single selection.
    pub(crate) fn from_selection(
        type_position: CompositeTypeDefinitionPosition,
        selection: Selection,
    ) -> Self {
        let schema = selection.schema().clone();
        let mut selection_map = SelectionMap::new();
        selection_map.insert(selection);
        Self {
            schema,
            type_position,
            selections: Arc::new(selection_map),
        }
    }

    /// Build a selection set from the given selections. This does **not** handle merging of
    /// selections with the same keys!
    pub(crate) fn from_raw_selections<S: Into<Selection>>(
        schema: ValidFederationSchema,
        type_position: CompositeTypeDefinitionPosition,
        selections: impl IntoIterator<Item = S>,
    ) -> Self {
        Self {
            schema,
            type_position,
            selections: Arc::new(selections.into_iter().collect()),
        }
    }

    #[cfg(any(doc, test))]
    pub fn parse(
        schema: ValidFederationSchema,
        type_position: CompositeTypeDefinitionPosition,
        source_text: &str,
    ) -> Result<Self, FederationError> {
        let selection_set = crate::schema::field_set::parse_field_set_without_normalization(
            schema.schema(),
            type_position.type_name().clone(),
            source_text,
        )?;
        let named_fragments = NamedFragments::new(&IndexMap::new(), &schema);
        SelectionSet::from_selection_set(&selection_set, &named_fragments, &schema)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.selections.is_empty()
    }

    pub(crate) fn contains_top_level_field(&self, field: &Field) -> Result<bool, FederationError> {
        if let Some(selection) = self.selections.get(&field.key()) {
            let Selection::Field(field_selection) = selection else {
                return Err(Internal {
                    message: format!(
                        "Field selection key for field \"{}\" references non-field selection",
                        field.data().field_position,
                    ),
                }
                .into());
            };
            Ok(field_selection.field == *field)
        } else {
            Ok(false)
        }
    }

    /// Normalize this selection set (merging selections with the same keys), with the following
    /// additional transformations:
    /// - Expand fragment spreads into inline fragments.
    /// - Remove `__schema` or `__type` introspection fields, as these shouldn't be handled by query
    ///   planning.
    /// - Hoist fragment spreads/inline fragments into their parents if they have no directives and
    ///   their parent type matches.
    ///
    /// Note this function asserts that the type of the selection set is a composite type (i.e. this
    /// isn't the empty selection set of some leaf field), and will return error if this is not the
    /// case.
    pub(crate) fn from_selection_set(
        selection_set: &executable::SelectionSet,
        fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<SelectionSet, FederationError> {
        let type_position: CompositeTypeDefinitionPosition =
            schema.get_type(selection_set.ty.clone())?.try_into()?;
        let mut normalized_selections = vec![];
        SelectionSet::normalize_selections(
            &selection_set.selections,
            &type_position,
            &mut normalized_selections,
            fragments,
            schema,
        )?;
        let mut merged = SelectionSet {
            schema: schema.clone(),
            type_position,
            selections: Arc::new(SelectionMap::new()),
        };
        merged.merge_selections_into(normalized_selections.iter())?;
        Ok(merged)
    }

    /// A helper function for normalizing a list of selections into a destination.
    fn normalize_selections(
        selections: &[executable::Selection],
        parent_type_position: &CompositeTypeDefinitionPosition,
        destination: &mut Vec<Selection>,
        fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<(), FederationError> {
        for selection in selections {
            match selection {
                executable::Selection::Field(field_selection) => {
                    let Some(normalized_field_selection) = FieldSelection::from_field(
                        field_selection,
                        parent_type_position,
                        fragments,
                        schema,
                    )?
                    else {
                        continue;
                    };
                    destination.push(Selection::from(normalized_field_selection));
                }
                executable::Selection::FragmentSpread(fragment_spread_selection) => {
                    let Some(fragment) = fragments.get(&fragment_spread_selection.fragment_name)
                    else {
                        return Err(Internal {
                            message: format!(
                                "Fragment spread referenced non-existent fragment \"{}\"",
                                fragment_spread_selection.fragment_name,
                            ),
                        }
                        .into());
                    };
                    // if we don't expand fragments, we need to normalize it
                    let normalized_fragment_spread = FragmentSpreadSelection::from_fragment_spread(
                        fragment_spread_selection,
                        &fragment,
                    )?;
                    destination.push(Selection::FragmentSpread(Arc::new(
                        normalized_fragment_spread,
                    )));
                }
                executable::Selection::InlineFragment(inline_fragment_selection) => {
                    let is_on_parent_type =
                        if let Some(type_condition) = &inline_fragment_selection.type_condition {
                            type_condition == parent_type_position.type_name()
                        } else {
                            true
                        };
                    // We can hoist/collapse inline fragments if their type condition is on the
                    // parent type (or they have no type condition) and they don't have any
                    // directives.
                    //
                    // PORT_NOTE: The JS codebase didn't hoist inline fragments, only fragment
                    // spreads (presumably because named fragments would commonly be on the same
                    // type as their fragment spread usages). It should be fine to also hoist inline
                    // fragments though if we notice they're similarly useless (and presumably later
                    // transformations in the JS codebase would take care of this).
                    if is_on_parent_type && inline_fragment_selection.directives.is_empty() {
                        SelectionSet::normalize_selections(
                            &inline_fragment_selection.selection_set.selections,
                            parent_type_position,
                            destination,
                            fragments,
                            schema,
                        )?;
                    } else {
                        let normalized_inline_fragment_selection =
                            InlineFragmentSelection::from_inline_fragment(
                                inline_fragment_selection,
                                parent_type_position,
                                fragments,
                                schema,
                            )?;
                        destination.push(Selection::InlineFragment(Arc::new(
                            normalized_inline_fragment_selection,
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Merges the given normalized selection sets into this one.
    pub(crate) fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op SelectionSet>,
    ) -> Result<(), FederationError> {
        let mut selections_to_merge = vec![];
        for other in others {
            if other.schema != self.schema {
                return Err(FederationError::internal(
                    "Cannot merge selection sets from different schemas",
                ));
            }
            if other.type_position != self.type_position {
                return Err(FederationError::internal(
                    format!(
                        "Cannot merge selection set for type \"{}\" into a selection set for type \"{}\"",
                        other.type_position,
                        self.type_position,
                    ),
                ));
            }
            selections_to_merge.extend(other.selections.values());
        }
        self.merge_selections_into(selections_to_merge.into_iter())
    }

    /// A helper function for merging the given selections into this one.
    fn merge_selections_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op Selection>,
    ) -> Result<(), FederationError> {
        let mut fields = IndexMap::new();
        let mut fragment_spreads = IndexMap::new();
        let mut inline_fragments = IndexMap::new();
        let target = Arc::make_mut(&mut self.selections);
        for other_selection in others {
            let other_key = other_selection.key();
            match target.entry(other_key.clone()) {
                normalized_selection_map::Entry::Occupied(existing) => match existing.get() {
                    Selection::Field(self_field_selection) => {
                        let Selection::Field(other_field_selection) = other_selection else {
                            return Err(Internal {
                                    message: format!(
                                        "Field selection key for field \"{}\" references non-field selection",
                                        self_field_selection.field.data().field_position,
                                    ),
                                }.into());
                        };
                        fields
                            .entry(other_key)
                            .or_insert_with(Vec::new)
                            .push(other_field_selection);
                    }
                    Selection::FragmentSpread(self_fragment_spread_selection) => {
                        let Selection::FragmentSpread(other_fragment_spread_selection) =
                            other_selection
                        else {
                            return Err(Internal {
                                    message: format!(
                                        "Fragment spread selection key for fragment \"{}\" references non-field selection",
                                        self_fragment_spread_selection.spread.data().fragment_name,
                                    ),
                                }.into());
                        };
                        fragment_spreads
                            .entry(other_key)
                            .or_insert_with(Vec::new)
                            .push(other_fragment_spread_selection);
                    }
                    Selection::InlineFragment(self_inline_fragment_selection) => {
                        let Selection::InlineFragment(other_inline_fragment_selection) =
                            other_selection
                        else {
                            return Err(Internal {
                                    message: format!(
                                        "Inline fragment selection key under parent type \"{}\" {}references non-field selection",
                                        self_inline_fragment_selection.inline_fragment.data().parent_type_position,
                                        self_inline_fragment_selection.inline_fragment.data().type_condition_position.clone()
                                            .map_or_else(
                                                String::new,
                                                |cond| format!("(type condition: {}) ", cond),
                                            ),
                                    ),
                                }.into());
                        };
                        inline_fragments
                            .entry(other_key)
                            .or_insert_with(Vec::new)
                            .push(other_inline_fragment_selection);
                    }
                },
                normalized_selection_map::Entry::Vacant(vacant) => {
                    vacant.insert(other_selection.clone())?;
                }
            }
        }

        for (key, self_selection) in target.iter_mut() {
            match self_selection {
                SelectionValue::Field(mut self_field_selection) => {
                    if let Some(other_field_selections) = fields.shift_remove(key) {
                        self_field_selection.merge_into(
                            other_field_selections.iter().map(|selection| &***selection),
                        )?;
                    }
                }
                SelectionValue::FragmentSpread(mut self_fragment_spread_selection) => {
                    if let Some(other_fragment_spread_selections) =
                        fragment_spreads.shift_remove(key)
                    {
                        self_fragment_spread_selection.merge_into(
                            other_fragment_spread_selections
                                .iter()
                                .map(|selection| &***selection),
                        )?;
                    }
                }
                SelectionValue::InlineFragment(mut self_inline_fragment_selection) => {
                    if let Some(other_inline_fragment_selections) =
                        inline_fragments.shift_remove(key)
                    {
                        self_inline_fragment_selection.merge_into(
                            other_inline_fragment_selections
                                .iter()
                                .map(|selection| &***selection),
                        )?;
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) fn expand_all_fragments(&self) -> Result<SelectionSet, FederationError> {
        let mut expanded_selections = vec![];
        SelectionSet::expand_selection_set(&mut expanded_selections, self)?;

        let mut expanded = SelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(SelectionMap::new()),
        };
        expanded.merge_selections_into(expanded_selections.iter())?;
        Ok(expanded)
    }

    fn expand_selection_set(
        destination: &mut Vec<Selection>,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        for (_, value) in selection_set.selections.iter() {
            match value {
                Selection::Field(field_selection) => {
                    let selections = match &field_selection.selection_set {
                        Some(s) => Some(s.expand_all_fragments()?),
                        None => None,
                    };
                    destination.push(Selection::from_field(
                        field_selection.field.clone(),
                        selections,
                    ))
                }
                Selection::FragmentSpread(spread_selection) => {
                    let fragment_spread_data = spread_selection.spread.data();
                    // We can hoist/collapse named fragments if their type condition is on the
                    // parent type and they don't have any directives.
                    if fragment_spread_data.type_condition_position == selection_set.type_position
                        && fragment_spread_data.directives.is_empty()
                    {
                        SelectionSet::expand_selection_set(
                            destination,
                            &spread_selection.selection_set,
                        )?;
                    } else {
                        // convert to inline fragment
                        let expanded = InlineFragmentSelection::from_fragment_spread_selection(
                            selection_set.type_position.clone(), // the parent type of this inline selection
                            spread_selection,
                        )?;
                        destination.push(Selection::InlineFragment(Arc::new(expanded)));
                    }
                }
                Selection::InlineFragment(inline_selection) => {
                    destination.push(Selection::from_inline_fragment(
                        inline_selection.inline_fragment.clone(),
                        inline_selection.selection_set.expand_all_fragments()?,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Modifies the provided selection set to optimize the handling of __typename selections for query planning.
    ///
    /// __typename information can always be provided by any subgraph declaring that type. While this data can be
    /// theoretically fetched from multiple sources, in practice it doesn't really matter which subgraph we use
    /// for the __typename and we should just get it from the same source as the one that was used to resolve
    /// other fields.
    ///
    /// In most cases, selecting __typename won't be a problem as query planning algorithm ignores "obviously"
    /// inefficient paths. Typically, querying the __typename of an entity is generally ok because when looking at
    /// a path, the query planning algorithm always favor getting a field "locally" if it can (which it always can
    /// for __typename) and ignore alternative that would jump subgraphs.
    ///
    /// When querying a __typename after a @shareable field, query planning algorithm would consider getting the
    /// __typename from EACH version of the @shareable field. This unnecessarily explodes the number of possible
    /// query plans with some useless options and results in degraded performance. Since the number of possible
    /// plans doubles for every field for which there is a choice, eliminating unnecessary choices improves query
    /// planning performance.
    ///
    /// It is unclear how to do this cleanly with the current planning algorithm, so this method is a workaround
    /// so we can efficiently generate query plans. In order to prevent the query planner from spending time
    /// exploring those useless __typename options, we "remove" the unnecessary __typename selections from the
    /// operation. Since we need to ensure that the __typename field will still need to be queried, we "tag"
    /// one of the "sibling" selections (using "attachement") to remember that __typename needs to be added
    /// back eventually. The core query planning algorithm will ignore that tag, and because __typename has been
    /// otherwise removed, we'll save any related work. As we build the final query plan, we'll check back for
    /// those "tags" and add back the __typename selections. As this only happen after the query planning
    /// algorithm has computed all choices, we achieve our goal of not considering useless choices due to
    /// __typename. Do note that if __typename is the "only" selection of some selection set, then we leave it
    /// untouched, and let the query planning algorithm treat it as any other field. We have no other choice in
    /// that case, and that's actually what we want.
    pub(crate) fn optimize_sibling_typenames(
        &mut self,
        interface_types_with_interface_objects: &IndexSet<InterfaceTypeDefinitionPosition>,
    ) -> Result<(), FederationError> {
        let is_interface_object =
            interface_types_with_interface_objects.contains(&InterfaceTypeDefinitionPosition {
                type_name: self.type_position.type_name().clone(),
            });
        let mut typename_field_key: Option<SelectionKey> = None;
        let mut sibling_field_key: Option<SelectionKey> = None;

        let mutable_selection_map = Arc::make_mut(&mut self.selections);
        for (key, entry) in mutable_selection_map.iter_mut() {
            match entry {
                SelectionValue::Field(mut field_selection) => {
                    if field_selection.get().field.data().name() == &TYPENAME_FIELD
                        && !is_interface_object
                        && typename_field_key.is_none()
                    {
                        typename_field_key = Some(key.clone());
                    } else if sibling_field_key.is_none() {
                        sibling_field_key = Some(key.clone());
                    }

                    if let Some(field_selection_set) = field_selection.get_selection_set_mut() {
                        field_selection_set
                            .optimize_sibling_typenames(interface_types_with_interface_objects)?;
                    }
                }
                SelectionValue::InlineFragment(mut inline_fragment) => {
                    inline_fragment
                        .get_selection_set_mut()
                        .optimize_sibling_typenames(interface_types_with_interface_objects)?;
                }
                SelectionValue::FragmentSpread(fragment_spread) => {
                    // at this point in time all fragment spreads should have been converted into inline fragments
                    return Err(FederationError::internal(
                        format!(
                            "Error while optimizing sibling typename information, selection set contains {} named fragment",
                            fragment_spread.get().spread.data().fragment_name
                        )
                    ));
                }
            }
        }

        if let (Some(typename_key), Some(sibling_field_key)) =
            (typename_field_key, sibling_field_key)
        {
            if let (
                Some((_, Selection::Field(typename_field))),
                Some(SelectionValue::Field(mut sibling_field)),
            ) = (
                mutable_selection_map.remove(&typename_key),
                mutable_selection_map.get_mut(&sibling_field_key),
            ) {
                *sibling_field.get_sibling_typename_mut() =
                    Some(typename_field.field.data().response_name());
            } else {
                unreachable!("typename and sibling fields must both exist at this point")
            }
        }
        Ok(())
    }

    pub(crate) fn without_empty_branches(&self) -> Result<Option<Cow<'_, Self>>, FederationError> {
        let filtered = self.filter_recursive_depth_first(&mut |sel| match sel {
            Selection::Field(field) => Ok(if let Some(set) = &field.selection_set {
                !set.is_empty()
            } else {
                true
            }),
            Selection::InlineFragment(inline) => Ok(!inline.selection_set.is_empty()),
            Selection::FragmentSpread(_) => {
                Err(FederationError::internal("unexpected fragment spread"))
            }
        })?;
        Ok(if filtered.selections.is_empty() {
            None
        } else {
            Some(filtered)
        })
    }

    pub(crate) fn filter_recursive_depth_first(
        &self,
        predicate: &mut dyn FnMut(&Selection) -> Result<bool, FederationError>,
    ) -> Result<Cow<'_, Self>, FederationError> {
        match self.selections.filter_recursive_depth_first(predicate)? {
            Cow::Borrowed(_) => Ok(Cow::Borrowed(self)),
            Cow::Owned(selections) => Ok(Cow::Owned(Self {
                schema: self.schema.clone(),
                type_position: self.type_position.clone(),
                selections: Arc::new(selections),
            })),
        }
    }

    pub(crate) fn conditions(&self) -> Result<Conditions, FederationError> {
        // If the conditions of all the selections within the set are the same,
        // then those are conditions of the whole set and we return it.
        // Otherwise, we just return `true`
        // (which essentially translate to "that selection always need to be queried").
        // Note that for the case where the set has only 1 selection,
        // then this just mean we return the condition of that one selection.
        // Also note that in theory we could be a tad more precise,
        // and when all the selections have variable conditions,
        // we could return the intersection of all of them,
        // but we don't bother for now as that has probably extremely rarely an impact in practice.
        let mut selections = self.selections.values();
        let Some(first_selection) = selections.next() else {
            // we shouldn't really get here for well-formed selection, so whether we return true or false doesn't matter
            // too much, but in principle, if there is no selection, we should be cool not including it.
            return Ok(Conditions::Boolean(false));
        };
        let conditions = first_selection.conditions()?;
        for selection in selections {
            if selection.conditions()? != conditions {
                return Ok(Conditions::Boolean(true));
            }
        }
        Ok(conditions)
    }

    /// Build a selection by merging all items in the given selections (slice).
    /// - Assumes all items in the slice have the same selection key.
    fn make_selection<'a>(
        schema: &ValidFederationSchema,
        parent_type: &CompositeTypeDefinitionPosition,
        selections: impl Iterator<Item = &'a Selection>,
        named_fragments: &NamedFragments,
    ) -> Result<Selection, FederationError> {
        let mut iter = selections;
        let Some(first) = iter.next() else {
            // PORT_NOTE: The TypeScript version asserts here.
            return Err(FederationError::internal(
                "Should not be called without any updates",
            ));
        };
        let Some(second) = iter.next() else {
            // Optimize for the simple case of a single selection, as we don't have to do anything
            // complex to merge the sub-selections.
            return first
                .rebase_on(
                    parent_type,
                    named_fragments,
                    schema,
                    RebaseErrorHandlingOption::ThrowError,
                )?
                .ok_or_else(|| FederationError::internal("Unable to rebase selection updates"));
        };

        let element = first.element()?.rebase_on(
            parent_type,
            schema,
            RebaseErrorHandlingOption::ThrowError,
        )?;
        let Some(element) = element else {
            return Err(FederationError::internal(
                "Unable to rebase selection updates",
            ));
        };
        let sub_selection_parent_type: Option<CompositeTypeDefinitionPosition> =
            element.sub_selection_type_position()?;

        let Some(ref sub_selection_parent_type) = sub_selection_parent_type else {
            // This is a leaf, so all updates should correspond ot the same field and we just use the first.
            return Selection::from_element(element, /*sub_selection*/ None);
        };

        // This case has a sub-selection. Merge all sub-selection updates.
        let mut sub_selection_updates: MultiIndexMap<SelectionKey, Selection> =
            MultiIndexMap::new();
        for selection in [first, second].into_iter().chain(iter) {
            if let Some(sub_selection_set) = selection.selection_set()? {
                sub_selection_updates.extend(
                    sub_selection_set
                        .selections
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone())),
                );
            }
        }
        let updated_sub_selection = Some(Self::make_selection_set(
            schema,
            sub_selection_parent_type,
            sub_selection_updates.values().map(|v| v.iter()),
            named_fragments,
        )?);
        Selection::from_element(element, updated_sub_selection)
    }

    /// Build a selection set by aggregating all items from the `selection_key_groups` iterator.
    /// - Assumes each item (slice) from the iterator has the same selection key within the slice.
    /// - Note that if the same selection key repeats in a later group, the previous group will be
    ///   ignored and replaced by the new group.
    pub(crate) fn make_selection_set<'a>(
        schema: &ValidFederationSchema,
        parent_type: &CompositeTypeDefinitionPosition,
        selection_key_groups: impl Iterator<Item = impl Iterator<Item = &'a Selection>>,
        named_fragments: &NamedFragments,
    ) -> Result<SelectionSet, FederationError> {
        let mut result = SelectionMap::new();
        for group in selection_key_groups {
            let selection = Self::make_selection(schema, parent_type, group, named_fragments)?;
            result.insert(selection);
        }
        Ok(SelectionSet {
            schema: schema.clone(),
            type_position: parent_type.clone(),
            selections: Arc::new(result),
        })
    }

    // PORT_NOTE: Some features of the TypeScript `lazyMap` were not ported:
    // - `parentType` (optional) parameter: This is only used in `SelectionSet.normalize` method,
    //   but its Rust version doesn't use `lazy_map`.
    // - `mapper` may return a `SelectionSet`.
    //   For simplicity, this case was not ported. It was used by `normalize` method in the TypeScript.
    //   But, the Rust version doesn't use `lazy_map`.
    // - `mapper` may return `PathBasedUpdate`.
    //   The `PathBasedUpdate` case is only used in `withFieldAliased` function in the TypeScript
    //   version, but its Rust version doesn't use `lazy_map`.
    // PORT_NOTE #2: Taking ownership of `self` in this method was considered. However, calling
    // `Arc::make_mut` on the `Arc` fields of `self` didn't seem better than cloning Arc instances.
    pub(crate) fn lazy_map(
        &self,
        named_fragments: &NamedFragments,
        mut mapper: impl FnMut(&Selection) -> Result<SelectionMapperReturn, FederationError>,
    ) -> Result<SelectionSet, FederationError> {
        let mut iter = self.selections.values();

        // Find the first object that is not identical after mapping
        let Some((index, (_, first_changed))) = iter
            .by_ref()
            .map(|sel| (sel, mapper(sel)))
            .enumerate()
            .find(|(_, (sel, updated))|
                !matches!(&updated, Ok(SelectionMapperReturn::Selection(updated)) if updated == *sel))
        else {
            // All selections are identical after mapping, so just clone `self`.
            return Ok(self.clone());
        };

        // The mapped selection could be an error, so we need to not forget about it.
        let first_changed = first_changed?;
        // Copy the first half of the selections until the `index`-th item, since they are not
        // changed.
        let mut updated_selections = MultiIndexMap::new();
        updated_selections.extend(
            self.selections
                .iter()
                .take(index)
                .map(|(k, v)| (k.clone(), v.clone())),
        );

        let mut update_new_selection = |selection| match selection {
            SelectionMapperReturn::None => {} // Removed; Skip it.
            SelectionMapperReturn::Selection(new_selection) => {
                updated_selections.insert(new_selection.key(), new_selection)
            }
            SelectionMapperReturn::SelectionList(new_selections) => {
                updated_selections.extend(new_selections.into_iter().map(|s| (s.key(), s)))
            }
        };

        // Now update the rest of the selections using the `mapper` function.
        update_new_selection(first_changed);
        for selection in iter {
            update_new_selection(mapper(selection)?)
        }

        Self::make_selection_set(
            &self.schema,
            &self.type_position,
            updated_selections.values().map(|v| v.iter()),
            named_fragments,
        )
    }

    pub(crate) fn add_back_typename_in_attachments(&self) -> Result<SelectionSet, FederationError> {
        self.lazy_map(/*named_fragments*/ &Default::default(), |selection| {
            let selection_element = selection.element()?;
            let updated = selection
                .map_selection_set(|ss| ss.add_back_typename_in_attachments().map(Some))?;
            let Some(sibling_typename) = selection_element.sibling_typename() else {
                // No sibling typename to add back
                return Ok(updated.into());
            };
            // We need to add the query __typename for the current type in the current group.
            // Note that the value of the sibling_typename is the alias or "" if there is no alias
            let alias = if sibling_typename.is_empty() {
                None
            } else {
                Some(sibling_typename.clone())
            };
            let field_element = Field::new_introspection_typename(
                &self.schema,
                &selection.element()?.parent_type_position(),
                alias,
            );
            let typename_selection =
                Selection::from_element(field_element.into(), /*subselection*/ None)?;
            Ok([typename_selection, updated].into_iter().collect())
        })
    }

    pub(crate) fn add_typename_field_for_abstract_types(
        &self,
        parent_type_if_abstract: Option<AbstractType>,
        fragments: &Option<&mut RebasedFragments>,
    ) -> Result<SelectionSet, FederationError> {
        let mut selection_map = SelectionMap::new();
        if let Some(parent) = parent_type_if_abstract {
            if !self.has_top_level_typename_field() {
                let typename_selection = Selection::from_field(
                    Field::new_introspection_typename(&self.schema, &parent.into(), None),
                    None,
                );
                selection_map.insert(typename_selection);
            }
        }
        for selection in self.selections.values() {
            selection_map.insert(if let Some(selection_set) = selection.selection_set()? {
                let type_if_abstract =
                    subselection_type_if_abstract(selection, &self.schema, fragments);
                let updated_selection_set = selection_set
                    .add_typename_field_for_abstract_types(type_if_abstract, fragments)?;

                if updated_selection_set == *selection_set {
                    selection.clone()
                } else {
                    selection.with_updated_selection_set(Some(updated_selection_set))?
                }
            } else {
                selection.clone()
            });
        }

        Ok(SelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(selection_map),
        })
    }

    fn has_top_level_typename_field(&self) -> bool {
        // Needs to be behind a OnceLock because `Arc::new` is non-const.
        // XXX(@goto-bus-stop): Note this does *not* count `__typename @include(if: true)`.
        // This seems wrong? But it's what JS does, too.
        static TYPENAME_KEY: OnceLock<SelectionKey> = OnceLock::new();
        let key = TYPENAME_KEY.get_or_init(|| SelectionKey::Field {
            response_name: TYPENAME_FIELD,
            directives: Arc::new(Default::default()),
        });

        self.selections.contains_key(key)
    }

    /// Inserts a `Selection` into the inner map. Should a selection with the same key already
    /// exist in the map, the existing selection and the given selection are merged, replacing the
    /// existing selection while keeping the same insertion index.
    fn add_selection(
        &mut self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        selection: Selection,
    ) -> Result<(), FederationError> {
        let selections = Arc::make_mut(&mut self.selections);

        let key = selection.key();
        match selections.remove(&key) {
            Some((index, existing_selection)) => {
                let to_merge = [existing_selection, selection];
                // `existing_selection` and `selection` both have the same selection key,
                // so the merged selection will also have the same selection key.
                let selection = SelectionSet::make_selection(
                    schema,
                    parent_type,
                    to_merge.iter(),
                    /*named_fragments*/ &Default::default(),
                )?;
                selections.insert_at(index, selection);
            }
            None => {
                selections.insert(selection);
            }
        }

        Ok(())
    }

    /// Adds a path, and optional some selections following that path, to this selection map.
    ///
    /// Today, it is possible here to add conflicting paths, such as:
    /// - `add_at_path("field1(arg: 1)")`
    /// - `add_at_path("field1(arg: 2)")`
    ///
    /// Users of this method should guarantee that this doesn't happen. Otherwise, converting this
    /// SelectionSet back to an ExecutableDocument will return a validation error.
    ///
    /// The final selections are optional. If `path` ends on a leaf field, then no followup
    /// selections would make sense.
    /// When final selections are provided, unecessary fragments will be automatically removed
    /// at the junction between the path and those final selections.
    ///
    /// For instance, suppose that we have:
    ///  - a `path` argument that is `a::b::c`,
    ///    where the type of the last field `c` is some object type `C`.
    ///  - a `selections` argument that is `{ ... on C { d } }`.
    ///
    /// Then the resulting built selection set will be: `{ a { b { c { d } } }`,
    /// and in particular the `... on C` fragment will be eliminated since it is unecesasry
    /// (since again, `c` is of type `C`).
    pub(crate) fn add_at_path(
        &mut self,
        path: &[Arc<OpPathElement>],
        selection_set: Option<&Arc<SelectionSet>>,
    ) -> Result<(), FederationError> {
        // PORT_NOTE: This method was ported from the JS class `SelectionSetUpdates`. Unlike the
        // JS code, this mutates the selection set map in-place.
        match path.split_first() {
            // If we have a sub-path, recurse.
            Some((ele, path @ &[_, ..])) => {
                let Some(sub_selection_type) = ele.sub_selection_type_position()? else {
                    return Err(FederationError::internal("unexpected error: add_at_path encountered a field that is not of a composite type".to_string()));
                };
                let mut selection = Arc::make_mut(&mut self.selections)
                    .entry(ele.key())
                    .or_insert(|| {
                        Selection::from_element(
                            OpPathElement::clone(ele),
                            // We immediately add a selection afterward to make this selection set
                            // valid.
                            Some(SelectionSet::empty(self.schema.clone(), sub_selection_type)),
                        )
                    })?;
                match &mut selection {
                    SelectionValue::Field(field) => match field.get_selection_set_mut() {
                        Some(sub_selection) => sub_selection.add_at_path(path, selection_set)?,
                        None => return Err(FederationError::internal("add_at_path encountered a field without a subselection which should never happen".to_string())),
                    },
                    SelectionValue::InlineFragment(fragment) => fragment
                        .get_selection_set_mut()
                        .add_at_path(path, selection_set)?,
                    SelectionValue::FragmentSpread(_fragment) => {
                        return Err(FederationError::internal("add_at_path encountered a named fragment spread which should never happen".to_string()));
                    }
                };
            }
            // If we have no sub-path, we can add the selection.
            Some((ele, &[])) => {
                // PORT_NOTE: The JS code waited until the final selection was being constructed to
                // turn the path and selection set into a selection. Because we are mutating things
                // in-place, we eagerly construct the selection.
                let element = OpPathElement::clone(ele);
                let selection = Selection::from_element(
                    element,
                    selection_set.map(|set| SelectionSet::clone(set)),
                )?;
                self.add_selection(&ele.parent_type_position(), ele.schema(), selection)?
            }
            // If we don't have any path, we merge in the given subselections at the root.
            None => {
                if let Some(sel) = selection_set {
                    let parent_type = &sel.type_position;
                    let schema = sel.schema.clone();
                    sel.selections
                        .values()
                        .cloned()
                        .try_for_each(|sel| self.add_selection(parent_type, &schema, sel))?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn collect_used_fragment_names(&self, aggregator: &mut HashMap<Name, i32>) {
        self.selections
            .iter()
            .for_each(|(_, s)| s.collect_used_fragment_names(aggregator));
    }

    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<SelectionSet, FederationError> {
        let rebased_results = self
            .selections
            .iter()
            .filter_map(|(_, selection)| {
                selection
                    .rebase_on(parent_type, named_fragments, schema, error_handling)
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SelectionSet::from_raw_selections(
            schema.clone(),
            parent_type.clone(),
            rebased_results,
        ))
    }

    /// Applies some normalization rules to this selection set in the context of the provided `parent_type`.
    ///
    /// Normalization mostly removes unnecessary/redundant inline fragments, so that for instance, with a schema:
    /// ```graphql
    /// type Query {
    ///   t1: T1
    ///   i: I
    /// }
    ///
    /// interface I {
    ///   id: ID!
    /// }
    ///
    /// type T1 implements I {
    ///   id: ID!
    ///   v1: Int
    /// }
    ///
    /// type T2 implements I {
    ///   id: ID!
    ///   v2: Int
    /// }
    /// ```
    /// We can perform following normalization
    /// ```graphql
    /// normalize({
    ///   t1 {
    ///     ... on I {
    ///       id
    ///     }
    ///   }
    ///   i {
    ///     ... on T1 {
    ///       ... on I {
    ///         ... on T1 {
    ///           v1
    ///         }
    ///         ... on T2 {
    ///           v2
    ///         }
    ///       }
    ///     }
    ///     ... on T2 {
    ///       ... on I {
    ///         id
    ///       }
    ///     }
    ///   }
    /// }) === {
    ///   t1 {
    ///     id
    ///   }
    ///   i {
    ///     ... on T1 {
    ///       v1
    ///     }
    ///     ... on T2 {
    ///       id
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// For this operation to be valid (to not throw), `parent_type` must be such that every field selection in
    /// this selection set is such that its type position intersects with passed `parent_type` (there is no limitation
    /// on the fragment selections, though any fragment selections whose condition do not intersects `parent_type`
    /// will be discarded). Note that `self.normalize(self.type_condition)` is always valid and useful, but it is
    /// also possible to pass a `parent_type` that is more "restrictive" than the selection current type position
    /// (as long as the top-level fields of this selection set can be rebased on that type).
    ///
    /// Passing the option `recursive == false` makes the normalization only apply at the top-level, removing
    /// any unnecessary top-level inline fragments, possibly multiple layers of them, but we never recurse
    /// inside the sub-selection of an selection that is not removed by the normalization.
    pub(crate) fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        option: NormalizeSelectionOption,
    ) -> Result<SelectionSet, FederationError> {
        let mut normalized_selection_map = SelectionMap::new();
        for (_, selection) in self.selections.iter() {
            if let Some(selection_or_set) =
                selection.normalize(parent_type, named_fragments, schema, option)?
            {
                match selection_or_set {
                    SelectionOrSet::Selection(normalized_selection) => {
                        normalized_selection_map.insert(normalized_selection);
                    }
                    SelectionOrSet::SelectionSet(normalized_set) => {
                        normalized_selection_map.extend_ref(&normalized_set.selections);
                    }
                }
            }
        }

        Ok(SelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(normalized_selection_map),
        })
    }

    pub(crate) fn can_rebase_on(&self, parent_type: &CompositeTypeDefinitionPosition) -> bool {
        self.selections
            .values()
            .all(|sel| sel.can_add_to(parent_type, &self.schema))
    }

    fn has_defer(&self) -> bool {
        self.selections.values().any(|s| s.has_defer())
    }

    pub(crate) fn add_aliases_for_non_merging_fields(
        &self,
    ) -> Result<(SelectionSet, Vec<Arc<FetchDataRewrite>>), FederationError> {
        let mut aliases = Vec::new();
        compute_aliases_for_non_merging_fields(
            vec![SelectionSetAtPath {
                path: Vec::new(),
                selections: Some(self.clone()),
            }],
            &mut aliases,
            &self.schema,
        )?;

        let updated = self.with_field_aliased(&aliases)?;
        let output_rewrites = aliases
            .into_iter()
            .map(
                |FieldToAlias {
                     mut path,
                     response_name,
                     alias,
                 }| {
                    path.push(FetchDataPathElement::Key(alias.into()));
                    Arc::new(FetchDataRewrite::KeyRenamer(FetchDataKeyRenamer {
                        path,
                        rename_key_to: response_name,
                    }))
                },
            )
            .collect::<Vec<_>>();

        Ok((updated, output_rewrites))
    }

    pub(crate) fn with_field_aliased(
        &self,
        aliases: &[FieldToAlias],
    ) -> Result<SelectionSet, FederationError> {
        if aliases.is_empty() {
            return Ok(self.clone());
        }

        let mut at_current_level: HashMap<FetchDataPathElement, &FieldToAlias> = HashMap::new();
        let mut remaining: Vec<&FieldToAlias> = Vec::new();

        for alias in aliases {
            if !alias.path.is_empty() {
                remaining.push(alias);
            } else {
                at_current_level.insert(
                    FetchDataPathElement::Key(alias.response_name.clone()),
                    alias,
                );
            }
        }

        let mut selection_map = SelectionMap::new();
        for selection in self.selections.values() {
            let path_element = selection.element()?.as_path_element();
            let subselection_aliases = remaining
                .iter()
                .filter_map(|alias| {
                    if alias.path.first() == path_element.as_ref() {
                        Some(FieldToAlias {
                            path: alias.path[1..].to_vec(),
                            response_name: alias.response_name.clone(),
                            alias: alias.alias.clone(),
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            let selection_set = selection.selection_set()?;
            let updated_selection_set = selection_set
                .map(|selection_set| selection_set.with_field_aliased(&subselection_aliases))
                .transpose()?;

            match selection {
                Selection::Field(field) => {
                    let alias = path_element.and_then(|elem| at_current_level.get(&elem));
                    if alias.is_none() && selection_set == updated_selection_set.as_ref() {
                        selection_map.insert(selection.clone());
                    } else {
                        let updated_field = match alias {
                            Some(alias) => field.with_updated_alias(alias.alias.clone()),
                            None => field.field.clone(),
                        };
                        selection_map
                            .insert(Selection::from_field(updated_field, updated_selection_set));
                    }
                }
                Selection::InlineFragment(_) => {
                    if selection_set == updated_selection_set.as_ref() {
                        selection_map.insert(selection.clone());
                    } else {
                        selection_map
                            .insert(selection.with_updated_selection_set(updated_selection_set)?);
                    }
                }
                Selection::FragmentSpread(_) => {
                    return Err(FederationError::internal("unexpected fragment spread"))
                }
            }
        }

        Ok(SelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(selection_map),
        })
    }

    pub(crate) fn fields_in_set(&self) -> Vec<CollectedFieldInSet> {
        let mut fields = Vec::new();

        for (_key, selection) in self.selections.iter() {
            match selection {
                Selection::Field(field) => fields.push(CollectedFieldInSet {
                    path: Vec::new(),
                    field: field.clone(),
                }),
                Selection::FragmentSpread(_fragment) => {
                    todo!()
                }
                Selection::InlineFragment(inline_fragment) => {
                    let condition = inline_fragment
                        .inline_fragment
                        .data()
                        .type_condition_position
                        .as_ref();
                    let header = match condition {
                        Some(cond) => vec![FetchDataPathElement::TypenameEquals(
                            cond.type_name().clone().into(),
                        )],
                        None => vec![],
                    };
                    for CollectedFieldInSet { path, field } in
                        inline_fragment.selection_set.fields_in_set().into_iter()
                    {
                        let mut new_path = header.clone();
                        new_path.extend(path);
                        fields.push(CollectedFieldInSet {
                            path: new_path,
                            field,
                        })
                    }
                }
            }
        }
        fields
    }

    pub(crate) fn used_variables(&self) -> Result<Vec<Name>, FederationError> {
        let mut variables = HashSet::new();
        self.collect_variables(&mut variables)?;
        let mut res: Vec<Name> = variables.into_iter().cloned().collect();
        res.sort();
        Ok(res)
    }

    pub(crate) fn collect_variables<'selection>(
        &'selection self,
        variables: &mut HashSet<&'selection Name>,
    ) -> Result<(), FederationError> {
        for selection in self.selections.values() {
            selection.collect_variables(variables)?
        }
        Ok(())
    }

    pub(crate) fn validate(
        &self,
        _variable_definitions: &[Node<executable::VariableDefinition>],
    ) -> Result<(), FederationError> {
        if self.selections.is_empty() {
            Err(FederationError::internal("Invalid empty selection set"))
        } else {
            for selection in self.selections.values() {
                if let Some(s) = selection.selection_set()? {
                    s.validate(_variable_definitions)?;
                }
            }

            Ok(())
        }
    }

    pub(crate) fn containment(&self, other: &Self, options: ContainmentOptions) -> Containment {
        if other.selections.len() > self.selections.len() {
            // If `other` has more selections but we're ignoring missing __typename, then in the case where
            // `other` has a __typename but `self` does not, then we need the length of `other` to be at
            // least 2 more than other of `self` to be able to conclude there is no contains.
            if !options.ignore_missing_typename
                || other.selections.len() > self.selections.len() + 1
                || self.has_top_level_typename_field()
                || !other.has_top_level_typename_field()
            {
                return Containment::NotContained;
            }
        }

        let mut is_equal = true;
        let mut did_ignore_typename = false;

        for (key, other_selection) in other.selections.iter() {
            if key.is_typename_field() && options.ignore_missing_typename {
                if !self.has_top_level_typename_field() {
                    did_ignore_typename = true;
                }
                continue;
            }

            let Some(self_selection) = self.selections.get(key) else {
                return Containment::NotContained;
            };

            match self_selection.containment(other_selection, options) {
                Containment::NotContained => return Containment::NotContained,
                Containment::StrictlyContained if is_equal => is_equal = false,
                Containment::StrictlyContained | Containment::Equal => {}
            }
        }

        let expected_len = if did_ignore_typename {
            self.selections.len() + 1
        } else {
            self.selections.len()
        };

        if is_equal && other.selections.len() == expected_len {
            Containment::Equal
        } else {
            Containment::StrictlyContained
        }
    }

    /// Returns true if this selection is a superset of the other selection.
    pub(crate) fn contains(&self, other: &Self) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl IntoIterator for SelectionSet {
    type Item = <IndexMap<SelectionKey, Selection> as IntoIterator>::Item;
    type IntoIter = <IndexMap<SelectionKey, Selection> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        Arc::unwrap_or_clone(self.selections).into_iter()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SelectionSetAtPath {
    path: Vec<FetchDataPathElement>,
    selections: Option<SelectionSet>,
}

pub(crate) struct FieldToAlias {
    path: Vec<FetchDataPathElement>,
    response_name: NodeStr,
    alias: Name,
}

pub(crate) struct SeenResponseName {
    field_name: Name,
    field_type: executable::Type,
    selections: Option<Vec<SelectionSetAtPath>>,
}

pub(crate) struct CollectedFieldInSet {
    path: Vec<FetchDataPathElement>,
    field: Arc<FieldSelection>,
}

impl CollectedFieldInSet {
    pub(crate) fn field(&self) -> &Arc<FieldSelection> {
        &self.field
    }
}

struct FieldInPath {
    path: Vec<FetchDataPathElement>,
    field: Arc<FieldSelection>,
}

fn compute_aliases_for_non_merging_fields(
    selections: Vec<SelectionSetAtPath>,
    alias_collector: &mut Vec<FieldToAlias>,
    schema: &ValidFederationSchema,
) -> Result<(), FederationError> {
    let mut seen_response_names: HashMap<Name, SeenResponseName> = HashMap::new();

    fn rebased_fields_in_set(s: &SelectionSetAtPath) -> impl Iterator<Item = FieldInPath> + '_ {
        s.selections.iter().flat_map(|s2| {
            s2.fields_in_set()
                .into_iter()
                .map(|CollectedFieldInSet { path, field }| {
                    let mut new_path = s.path.clone();
                    new_path.extend(path);
                    FieldInPath {
                        path: new_path,
                        field,
                    }
                })
        })
    }

    for FieldInPath { mut path, field } in selections.iter().flat_map(rebased_fields_in_set) {
        let field_name = field.field.data().name();
        let response_name = field.field.data().response_name();
        let field_type = &field.field.data().field_position.get(schema.schema())?.ty;

        match seen_response_names.get(&response_name) {
            Some(previous) => {
                if &previous.field_name == field_name
                    && types_can_be_merged(&previous.field_type, field_type, schema.schema())?
                {
                    // If the type is non-composite, then we're all set. But if it is composite, we need to record the sub-selection to that response name
                    // as we need to "recurse" on the merged of both the previous and this new field.
                    if is_composite_type(field_type.inner_named_type(), schema.schema())? {
                        match &previous.selections {
                            None => {
                                return Err(SingleFederationError::Internal {
                                    message: format!(
                                        "Should have added selections for `'{:?}\'",
                                        previous.field_type
                                    ),
                                }
                                .into());
                            }
                            Some(s) => {
                                let mut selections = s.clone();
                                let mut p = path.clone();
                                p.push(FetchDataPathElement::Key(response_name.clone().into()));
                                selections.push(SelectionSetAtPath {
                                    path: p,
                                    selections: field.selection_set.clone(),
                                });
                                seen_response_names.insert(
                                    response_name,
                                    SeenResponseName {
                                        field_name: previous.field_name.clone(),
                                        field_type: previous.field_type.clone(),
                                        selections: Some(selections),
                                    },
                                )
                            }
                        };
                    }
                } else {
                    // We need to alias the new occurence.
                    let alias = gen_alias_name(&response_name, &seen_response_names);

                    // Given how we generate aliases, it's is very unlikely that the generated alias will conflict with any of the other response name
                    // at the level, but it's theoretically possible. By adding the alias to the seen names, we ensure that in the remote change that
                    // this ever happen, we'll avoid the conflict by giving another alias to the followup occurence.
                    let selections = match field.selection_set.as_ref() {
                        Some(s) => {
                            let mut p = path.clone();
                            p.push(FetchDataPathElement::Key(alias.clone().into()));
                            Some(vec![SelectionSetAtPath {
                                path: p,
                                selections: Some(s.clone()),
                            }])
                        }
                        None => None,
                    };

                    seen_response_names.insert(
                        alias.clone(),
                        SeenResponseName {
                            field_name: field_name.clone(),
                            field_type: field_type.clone(),
                            selections,
                        },
                    );

                    // Lastly, we record that the added alias need to be rewritten back to the proper response name post query.

                    alias_collector.push(FieldToAlias {
                        path,
                        response_name: response_name.into(),
                        alias,
                    })
                }
            }
            None => {
                let selections: Option<Vec<SelectionSetAtPath>> = match field.selection_set.as_ref()
                {
                    Some(s) => {
                        path.push(FetchDataPathElement::Key(response_name.clone().into()));
                        Some(vec![SelectionSetAtPath {
                            path,
                            selections: Some(s.clone()),
                        }])
                    }
                    None => None,
                };
                seen_response_names.insert(
                    response_name,
                    SeenResponseName {
                        field_name: field_name.clone(),
                        field_type: field_type.clone(),
                        selections,
                    },
                );
            }
        }
    }

    for selections in seen_response_names.into_values() {
        if let Some(selections) = selections.selections {
            compute_aliases_for_non_merging_fields(selections, alias_collector, schema)?;
        }
    }

    Ok(())
}

fn gen_alias_name(base_name: &Name, unavailable_names: &HashMap<Name, SeenResponseName>) -> Name {
    let mut counter = 0usize;
    loop {
        if let Ok(name) = Name::try_from(NodeStr::new(&format!("{base_name}__alias_{counter}"))) {
            if !unavailable_names.contains_key(&name) {
                return name;
            }
        }
        counter += 1;
    }
}

pub(crate) fn subselection_type_if_abstract(
    selection: &Selection,
    schema: &ValidFederationSchema,
    fragments: &Option<&mut RebasedFragments>,
) -> Option<AbstractType> {
    match selection {
        Selection::Field(field) => {
            match schema
                .get_type(field.field.data().field_position.type_name().clone())
                .ok()?
            {
                crate::schema::position::TypeDefinitionPosition::Interface(i) => {
                    Some(AbstractType::Interface(i))
                }
                crate::schema::position::TypeDefinitionPosition::Union(u) => {
                    Some(AbstractType::Union(u))
                }
                _ => None,
            }
        }
        Selection::FragmentSpread(fragment_spread) => {
            let fragment = fragments
                .as_ref()
                .and_then(|r| {
                    r.original_fragments
                        .get(&fragment_spread.spread.data().fragment_name)
                })
                .ok_or(crate::error::SingleFederationError::InvalidGraphQL {
                    message: "missing fragment".to_string(),
                })
                //FIXME: return error
                .ok()?;
            match fragment.type_condition_position.clone() {
                CompositeTypeDefinitionPosition::Interface(i) => Some(AbstractType::Interface(i)),
                CompositeTypeDefinitionPosition::Union(u) => Some(AbstractType::Union(u)),
                CompositeTypeDefinitionPosition::Object(_) => None,
            }
        }
        Selection::InlineFragment(inline_fragment) => {
            match inline_fragment
                .inline_fragment
                .data()
                .type_condition_position
                .clone()?
            {
                CompositeTypeDefinitionPosition::Interface(i) => Some(AbstractType::Interface(i)),
                CompositeTypeDefinitionPosition::Union(u) => Some(AbstractType::Union(u)),
                CompositeTypeDefinitionPosition::Object(_) => None,
            }
        }
    }
}

impl FieldSelection {
    /// Normalize this field selection (merging selections with the same keys), with the following
    /// additional transformations:
    /// - Expand fragment spreads into inline fragments.
    /// - Remove `__schema` or `__type` introspection fields, as these shouldn't be handled by query
    ///   planning.
    /// - Hoist fragment spreads/inline fragments into their parents if they have no directives and
    ///   their parent type matches.
    pub(crate) fn from_field(
        field: &executable::Field,
        parent_type_position: &CompositeTypeDefinitionPosition,
        fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<Option<FieldSelection>, FederationError> {
        // Skip __schema/__type introspection fields as router takes care of those, and they do not
        // need to be query planned.
        if field.name == "__schema" || field.name == "__type" {
            return Ok(None);
        }
        let field_position = parent_type_position.field(field.name.clone())?;
        // We might be able to validate that the returned `FieldDefinition` matches that within
        // the given `field`, but on the off-chance there's a mutation somewhere in between
        // Operation creation and the creation of the ValidFederationSchema, it's safer to just
        // confirm it exists in this schema.
        field_position.get(schema.schema())?;
        let field_composite_type_result: Result<CompositeTypeDefinitionPosition, FederationError> =
            schema.get_type(field.selection_set.ty.clone())?.try_into();

        Ok(Some(FieldSelection {
            field: Field::new(FieldData {
                schema: schema.clone(),
                field_position,
                alias: field.alias.clone(),
                arguments: Arc::new(field.arguments.clone()),
                directives: Arc::new(field.directives.clone()),
                sibling_typename: None,
            }),
            selection_set: if field_composite_type_result.is_ok() {
                Some(SelectionSet::from_selection_set(
                    &field.selection_set,
                    fragments,
                    schema,
                )?)
            } else {
                None
            },
        }))
    }

    pub(crate) fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        option: NormalizeSelectionOption,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        if let Some(selection_set) = &self.selection_set {
            let mut normalized_selection: SelectionSet =
                if NormalizeSelectionOption::NormalizeRecursively == option {
                    let field = self.field.data().field_position.get(schema.schema())?;
                    let field_composite_type_position: CompositeTypeDefinitionPosition = schema
                        .get_type(field.ty.inner_named_type().clone())?
                        .try_into()?;
                    selection_set.normalize(
                        &field_composite_type_position,
                        named_fragments,
                        schema,
                        option,
                    )?
                } else {
                    selection_set.clone()
                };

            let mut selection = self.clone();
            if normalized_selection.is_empty() {
                // In rare cases, it's possible that everything in the sub-selection was trimmed away and so the
                // sub-selection is empty. Which suggest something may be wrong with this part of the query
                // intent, but the query was valid while keeping an empty sub-selection isn't. So in that
                // case, we just add some "non-included" __typename field just to keep the query valid.
                let directives =
                    executable::DirectiveList(vec![Node::new(executable::Directive {
                        name: name!("include"),
                        arguments: vec![Node::new(executable::Argument {
                            name: name!("if"),
                            value: Node::new(executable::Value::Boolean(false)),
                        })],
                    })]);
                let non_included_typename = Selection::from_field(
                    Field::new(FieldData {
                        schema: schema.clone(),
                        field_position: parent_type.introspection_typename_field(),
                        alias: None,
                        arguments: Arc::new(vec![]),
                        directives: Arc::new(directives),
                        sibling_typename: None,
                    }),
                    None,
                );
                let mut typename_selection = SelectionMap::new();
                typename_selection.insert(non_included_typename);

                normalized_selection.selections = Arc::new(typename_selection);
                selection.selection_set = Some(normalized_selection);
            } else {
                selection.selection_set = Some(normalized_selection);
            }
            Ok(Some(SelectionOrSet::Selection(Selection::from(selection))))
        } else {
            // JS PORT NOTE: In JS implementation field selection stores field definition information,
            // in RS version we only store the field position reference so we don't need to update the
            // underlying elements
            Ok(Some(SelectionOrSet::Selection(Selection::from(
                self.clone(),
            ))))
        }
    }

    /// Returns a field selection "equivalent" to the one represented by this object, but such that its parent type
    /// is the one provided as argument.
    ///
    /// Obviously, this operation will only succeed if this selection (both the field itself and its subselections)
    /// make sense from the provided parent type. If this is not the case, this method will throw.
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Selection>, FederationError> {
        if &self.field.data().schema == schema
            && &self.field.data().field_position.parent() == parent_type
        {
            // we are rebasing field on the same parent within the same schema - we can just return self
            return Ok(Some(Selection::from(self.clone())));
        }

        let Some(rebased) = self.field.rebase_on(parent_type, schema, error_handling)? else {
            // rebasing failed but we are ignoring errors
            return Ok(None);
        };

        let Some(selection_set) = &self.selection_set else {
            // leaf field
            return Ok(Some(Selection::from_field(rebased, None)));
        };

        let rebased_type_name = rebased
            .data()
            .field_position
            .get(schema.schema())?
            .ty
            .inner_named_type();
        let rebased_base_type: CompositeTypeDefinitionPosition =
            schema.get_type(rebased_type_name.clone())?.try_into()?;

        let selection_set_type = &selection_set.type_position;
        if self.field.data().schema == rebased.data().schema
            && &rebased_base_type == selection_set_type
        {
            // we are rebasing within the same schema and the same base type
            return Ok(Some(Selection::from_field(
                rebased.clone(),
                self.selection_set.clone(),
            )));
        }

        let rebased_selection_set =
            selection_set.rebase_on(&rebased_base_type, named_fragments, schema, error_handling)?;
        if rebased_selection_set.selections.is_empty() {
            // empty selection set
            Ok(None)
        } else {
            Ok(Some(Selection::from_field(
                rebased.clone(),
                Some(rebased_selection_set),
            )))
        }
    }

    fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> bool {
        if &self.field.data().schema == schema
            && parent_type == &self.field.data().field_position.parent()
        {
            return true;
        }

        let Some(ty) = self.field.type_if_added_to(parent_type, schema) else {
            return false;
        };

        if let Some(set) = &self.selection_set {
            if set.type_position != ty {
                return set
                    .selections
                    .values()
                    .all(|sel| sel.can_add_to(parent_type, schema));
            }
        }
        true
    }

    pub(crate) fn has_defer(&self) -> bool {
        self.field.has_defer() || self.selection_set.as_ref().is_some_and(|s| s.has_defer())
    }

    pub(crate) fn containment(
        &self,
        other: &FieldSelection,
        options: ContainmentOptions,
    ) -> Containment {
        let self_field = self.field.data();
        let other_field = other.field.data();
        if self_field.name() != other_field.name()
            || self_field.alias != other_field.alias
            || !same_arguments(&self_field.arguments, &other_field.arguments)
            || !same_directives(&self_field.directives, &other_field.directives)
        {
            return Containment::NotContained;
        }

        match (&self.selection_set, &other.selection_set) {
            (None, None) => Containment::Equal,
            (Some(self_selection), Some(other_selection)) => {
                self_selection.containment(other_selection, options)
            }
            (None, Some(_)) | (Some(_), None) => {
                debug_assert!(false, "field selections have the same element, so if one does not have a subselection, neither should the other one");
                Containment::NotContained
            }
        }
    }

    /// Returns true if this selection is a superset of the other selection.
    pub(crate) fn contains(&self, other: &FieldSelection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl<'a> FieldSelectionValue<'a> {
    /// Merges the given normalized field selections into this one (this method assumes the keys
    /// already match).
    pub(crate) fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op FieldSelection>,
    ) -> Result<(), FederationError> {
        let self_field = &self.get().field;
        let mut selection_sets = vec![];
        for other in others {
            let other_field = &other.field;
            if other_field.data().schema != self_field.data().schema {
                return Err(Internal {
                    message: "Cannot merge field selections from different schemas".to_owned(),
                }
                .into());
            }
            if other_field.data().field_position != self_field.data().field_position {
                return Err(Internal {
                        message: format!(
                            "Cannot merge field selection for field \"{}\" into a field selection for field \"{}\"",
                            other_field.data().field_position,
                            self_field.data().field_position,
                        ),
                    }.into());
            }
            if self.get().selection_set.is_some() {
                let Some(other_selection_set) = &other.selection_set else {
                    return Err(Internal {
                        message: format!(
                            "Field \"{}\" has composite type but not a selection set",
                            other_field.data().field_position,
                        ),
                    }
                    .into());
                };
                selection_sets.push(other_selection_set);
            } else if other.selection_set.is_some() {
                return Err(Internal {
                    message: format!(
                        "Field \"{}\" has non-composite type but also has a selection set",
                        other_field.data().field_position,
                    ),
                }
                .into());
            }
        }
        if let Some(self_selection_set) = self.get_selection_set_mut() {
            self_selection_set.merge_into(selection_sets.into_iter())?;
        }
        Ok(())
    }
}

impl Field {
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Field>, FederationError> {
        let field_parent = self.data().field_position.parent();
        if self.data().schema == *schema && field_parent == *parent_type {
            // pointing to the same parent -> return self
            return Ok(Some(self.clone()));
        }

        if self.data().name() == &TYPENAME_FIELD {
            // TODO interface object info should be precomputed in QP constructor
            return if schema
                .possible_runtime_types(parent_type.clone())?
                .iter()
                .any(|t| is_interface_object(t, schema))
            {
                if let RebaseErrorHandlingOption::ThrowError = error_handling {
                    Err(FederationError::internal(
                        format!("Cannot add selection of field \"{}\" to selection set of parent type \"{}\" that is potentially an interface object type at runtime",
                                self.data().field_position,
                                parent_type
                        )))
                } else {
                    Ok(None)
                }
            } else {
                let mut updated_field_data = self.data().clone();
                updated_field_data.schema = schema.clone();
                updated_field_data.field_position = parent_type.introspection_typename_field();
                Ok(Some(Field::new(updated_field_data)))
            };
        }

        let field_from_parent = parent_type.field(self.data().name().clone())?;
        return if field_from_parent.get(schema.schema()).is_ok()
            && self.can_rebase_on(parent_type, schema)
        {
            let mut updated_field_data = self.data().clone();
            updated_field_data.schema = schema.clone();
            updated_field_data.field_position = field_from_parent;
            Ok(Some(Field::new(updated_field_data)))
        } else if let RebaseErrorHandlingOption::IgnoreError = error_handling {
            Ok(None)
        } else {
            Err(FederationError::internal(format!(
                "Cannot add selection of field \"{}\" to selection set of parent type \"{}\"",
                self.data().field_position,
                parent_type
            )))
        };
    }

    /// Verifies whether given field can be rebase on following parent type.
    ///
    /// There are 2 valid cases we want to allow:
    /// 1. either `parent_type` and `field_parent_type` are the same underlying type (same name) but from different underlying schema. Typically,
    ///  happens when we're building subgraph queries but using selections from the original query which is against the supergraph API schema.
    /// 2. or they are not the same underlying type, but the field parent type is from an interface (or an interface object, which is the same
    ///  here), in which case we may be rebasing an interface field on one of the implementation type, which is ok. Note that we don't verify
    ///  that `parent_type` is indeed an implementation of `field_parent_type` because it's possible that this implementation relationship exists
    ///  in the supergraph, but not in any of the subgraph schema involved here. So we just let it be. Not that `rebase_on` will complain anyway
    ///  if the field name simply does not exist in `parent_type`.
    fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> bool {
        let field_parent_type = self.data().field_position.parent();
        // case 1
        if field_parent_type.type_name() == parent_type.type_name() {
            return true;
        }
        // case 2
        let is_interface_object_type =
            match TryInto::<ObjectTypeDefinitionPosition>::try_into(field_parent_type.clone()) {
                Ok(ref o) => is_interface_object(o, schema),
                Err(_) => false,
            };
        field_parent_type.is_interface_type() || is_interface_object_type
    }

    pub(crate) fn has_defer(&self) -> bool {
        // @defer cannot be on field at the moment
        false
    }

    pub(crate) fn type_if_added_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Option<CompositeTypeDefinitionPosition> {
        let data = self.data();
        if data.field_position.parent() == *parent_type && data.schema == *schema {
            let base_ty_name = data
                .field_position
                .get(schema.schema())
                .ok()?
                .ty
                .inner_named_type();
            return schema
                .get_type(base_ty_name.clone())
                .and_then(CompositeTypeDefinitionPosition::try_from)
                .ok();
        }
        if data.name() == &TYPENAME_FIELD {
            let type_name = parent_type
                .introspection_typename_field()
                .get(schema.schema())
                .ok()?
                .ty
                .inner_named_type();
            return schema.try_get_type(type_name.clone())?.try_into().ok();
        }
        if self.can_rebase_on(parent_type, schema) {
            let type_name = parent_type
                .field(data.field_position.field_name().clone())
                .ok()?
                .get(schema.schema())
                .ok()?
                .ty
                .inner_named_type();
            schema.try_get_type(type_name.clone())?.try_into().ok()
        } else {
            None
        }
    }

    pub(crate) fn parent_type_position(&self) -> CompositeTypeDefinitionPosition {
        self.data().field_position.parent()
    }

    pub(crate) fn types_can_be_merged(&self, other: &Self) -> Result<bool, FederationError> {
        let self_definition = self.data().field_position.get(self.schema().schema())?;
        let other_definition = other.data().field_position.get(self.schema().schema())?;
        types_can_be_merged(
            &self_definition.ty,
            &other_definition.ty,
            self.schema().schema(),
        )
    }
}

impl<'a> FragmentSpreadSelectionValue<'a> {
    /// Merges the given normalized fragment spread selections into this one (this method assumes
    /// the keys already match).
    pub(crate) fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op FragmentSpreadSelection>,
    ) -> Result<(), FederationError> {
        let self_fragment_spread = &self.get().spread;
        for other in others {
            let other_fragment_spread = &other.spread;
            if other_fragment_spread.data().schema != self_fragment_spread.data().schema {
                return Err(Internal {
                    message: "Cannot merge fragment spread from different schemas".to_owned(),
                }
                .into());
            }
            // Nothing to do since the fragment spread is already part of the selection set.
            // Fragment spreads are uniquely identified by fragment name and applied directives.
            // Since there is already an entry for the same fragment spread, there is no point
            // in attempting to merge its sub-selections, as the underlying entry should be
            // exactly the same as the currently processed one.
        }
        Ok(())
    }
}

impl InlineFragmentSelection {
    /// Copies inline fragment selection and assigns it a new unique selection ID.
    pub(crate) fn with_unique_id(&self) -> Self {
        let mut data = self.inline_fragment.data().clone();
        data.selection_id = SelectionId::new();
        Self {
            inline_fragment: InlineFragment::new(data),
            selection_set: self.selection_set.clone(),
        }
    }

    /// Normalize this inline fragment selection (merging selections with the same keys), with the
    /// following additional transformations:
    /// - Expand fragment spreads into inline fragments.
    /// - Remove `__schema` or `__type` introspection fields, as these shouldn't be handled by query
    ///   planning.
    /// - Hoist fragment spreads/inline fragments into their parents if they have no directives and
    ///   their parent type matches.
    pub(crate) fn from_inline_fragment(
        inline_fragment: &executable::InlineFragment,
        parent_type_position: &CompositeTypeDefinitionPosition,
        fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<InlineFragmentSelection, FederationError> {
        let type_condition_position: Option<CompositeTypeDefinitionPosition> =
            if let Some(type_condition) = &inline_fragment.type_condition {
                Some(schema.get_type(type_condition.clone())?.try_into()?)
            } else {
                None
            };
        Ok(InlineFragmentSelection {
            inline_fragment: InlineFragment::new(InlineFragmentData {
                schema: schema.clone(),
                parent_type_position: parent_type_position.clone(),
                type_condition_position,
                directives: Arc::new(inline_fragment.directives.clone()),
                selection_id: SelectionId::new(),
            }),
            selection_set: SelectionSet::from_selection_set(
                &inline_fragment.selection_set,
                fragments,
                schema,
            )?,
        })
    }

    pub(crate) fn from_fragment_spread_selection(
        parent_type_position: CompositeTypeDefinitionPosition,
        fragment_spread_selection: &Arc<FragmentSpreadSelection>,
    ) -> Result<InlineFragmentSelection, FederationError> {
        let fragment_spread_data = fragment_spread_selection.spread.data();
        Ok(InlineFragmentSelection {
            inline_fragment: InlineFragment::new(InlineFragmentData {
                schema: fragment_spread_data.schema.clone(),
                parent_type_position,
                type_condition_position: Some(fragment_spread_data.type_condition_position.clone()),
                directives: fragment_spread_data.directives.clone(),
                selection_id: SelectionId::new(),
            }),
            selection_set: fragment_spread_selection
                .selection_set
                .expand_all_fragments()?,
        })
    }

    /// Construct a new InlineFragmentSelection out of a selection set.
    /// - The new type condition will be the same as the selection set's type.
    pub(crate) fn from_selection_set(
        parent_type_position: CompositeTypeDefinitionPosition,
        selection_set: SelectionSet,
        directives: Arc<executable::DirectiveList>,
    ) -> Self {
        let inline_fragment_data = InlineFragmentData {
            schema: selection_set.schema.clone(),
            parent_type_position,
            type_condition_position: selection_set.type_position.clone().into(),
            directives,
            selection_id: SelectionId::new(),
        };
        Self {
            inline_fragment: InlineFragment::new(inline_fragment_data),
            selection_set,
        }
    }

    pub(crate) fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        option: NormalizeSelectionOption,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        let this_condition = self.inline_fragment.data().type_condition_position.clone();
        // This method assumes by contract that `parent_type` runtimes intersects `self.inline_fragment.data().parent_type_position`'s,
        // but `parent_type` runtimes may be a subset. So first check if the selection should not be discarded on that account (that
        // is, we should not keep the selection if its condition runtimes don't intersect at all with those of
        // `parent_type` as that would ultimately make an invalid selection set).
        if let Some(ref type_condition) = this_condition {
            if (self.inline_fragment.data().schema != *schema
                || self.inline_fragment.data().parent_type_position != *parent_type)
                && !runtime_types_intersect(type_condition, parent_type, schema)
            {
                return Ok(None);
            }
        }

        // We know the condition is "valid", but it may not be useful. That said, if the condition has directives,
        // we preserve the fragment no matter what.
        if self.inline_fragment.data().directives.is_empty() {
            // There is a number of cases where a fragment is not useful:
            // 1. if there is no type condition (remember it also has no directives).
            // 2. if it's the same type as the current type: it's not restricting types further.
            // 3. if the current type is an object more generally: because in that case the condition
            //   cannot be restricting things further (it's typically a less precise interface/union).
            let useless_fragment = match this_condition {
                None => true,
                Some(ref c) => self.inline_fragment.data().schema == *schema && c == parent_type,
            };
            if useless_fragment || parent_type.is_object_type() {
                let normalized_selection_set =
                    self.selection_set
                        .normalize(parent_type, named_fragments, schema, option)?;
                return if normalized_selection_set.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(SelectionOrSet::SelectionSet(normalized_selection_set)))
                };
            }
        }

        // We preserve the current fragment, so we only recurse within the sub-selection if we're asked to be recursive.
        // (note that even if we're not recursive, we may still have some "lifting" to do)
        let normalized_selection_set = if NormalizeSelectionOption::NormalizeRecursively == option {
            let normalized =
                self.selection_set
                    .normalize(parent_type, named_fragments, schema, option)?;
            // It could be that nothing was satisfiable.
            if normalized.is_empty() {
                if self.inline_fragment.data().directives.is_empty() {
                    return Ok(None);
                } else if let Some(rebased_fragment) = self.inline_fragment.rebase_on(
                    parent_type,
                    schema,
                    RebaseErrorHandlingOption::ThrowError,
                )? {
                    // We should be able to rebase, or there is a bug, so error if that is the case.
                    // If we rebased successfully then we add "non-included" __typename field selection
                    // just to keep the query valid.
                    let directives =
                        executable::DirectiveList(vec![Node::new(executable::Directive {
                            name: name!("include"),
                            arguments: vec![Node::new(executable::Argument {
                                name: name!("if"),
                                value: Node::new(executable::Value::Boolean(false)),
                            })],
                        })]);
                    let parent_typename_field = if let Some(condition) = this_condition {
                        condition.introspection_typename_field()
                    } else {
                        parent_type.introspection_typename_field()
                    };
                    let typename_field_selection = Selection::from_field(
                        Field::new(FieldData {
                            schema: schema.clone(),
                            field_position: parent_typename_field,
                            alias: None,
                            arguments: Arc::new(vec![]),
                            directives: Arc::new(directives),
                            sibling_typename: None,
                        }),
                        None,
                    );

                    return Ok(Some(SelectionOrSet::Selection(
                        Selection::from_inline_fragment(
                            rebased_fragment,
                            SelectionSet::from_selection(
                                parent_type.clone(),
                                typename_field_selection,
                            ),
                        ),
                    )));
                }
            }
            normalized
        } else {
            self.selection_set.clone()
        };

        // Second, we check if some of the sub-selection fragments can be "lifted" outside of this fragment. This can happen if:
        // 1. the current fragment is an abstract type,
        // 2. the sub-fragment is an object type,
        // 3. the sub-fragment type is a valid runtime of the current type.
        if self.inline_fragment.data().directives.is_empty()
            && this_condition.is_some_and(|c| c.is_abstract_type())
        {
            let mut liftable_selections = SelectionMap::new();
            for (_, selection) in normalized_selection_set.selections.iter() {
                match selection {
                    Selection::FragmentSpread(spread_selection) => {
                        let type_condition = spread_selection
                            .spread
                            .data()
                            .type_condition_position
                            .clone();
                        if type_condition.is_object_type()
                            && runtime_types_intersect(parent_type, &type_condition, schema)
                        {
                            liftable_selections
                                .insert(Selection::FragmentSpread(spread_selection.clone()));
                        }
                    }
                    Selection::InlineFragment(inline_fragment_selection) => {
                        if let Some(type_condition) = inline_fragment_selection
                            .inline_fragment
                            .data()
                            .type_condition_position
                            .clone()
                        {
                            if type_condition.is_object_type()
                                && runtime_types_intersect(parent_type, &type_condition, schema)
                            {
                                liftable_selections.insert(Selection::InlineFragment(
                                    inline_fragment_selection.clone(),
                                ));
                            }
                        };
                    }
                    _ => continue,
                }
            }

            // If we can lift all selections, then that just mean we can get rid of the current fragment altogether
            if liftable_selections.len() == normalized_selection_set.selections.len() {
                return Ok(Some(SelectionOrSet::SelectionSet(normalized_selection_set)));
            }

            // Otherwise, if there are "liftable" selections, we must return a set comprised of those lifted selection,
            // and the current fragment _without_ those lifted selections.
            if liftable_selections.len() > 0 {
                let mut mutable_selections = self.selection_set.selections.clone();
                let final_fragment_selections = Arc::make_mut(&mut mutable_selections);
                final_fragment_selections.retain(|k, _| !liftable_selections.contains_key(k));
                let final_inline_fragment = Selection::from_inline_fragment(
                    self.inline_fragment.clone(),
                    SelectionSet {
                        schema: schema.clone(),
                        type_position: parent_type.clone(),
                        selections: Arc::new(final_fragment_selections.clone()),
                    },
                );

                let mut final_selection_map = SelectionMap::new();
                final_selection_map.insert(final_inline_fragment);
                final_selection_map.extend(liftable_selections);
                let final_selections = SelectionSet {
                    schema: schema.clone(),
                    type_position: parent_type.clone(),
                    selections: final_selection_map.into(),
                };
                return Ok(Some(SelectionOrSet::SelectionSet(final_selections)));
            }
        }

        if self.inline_fragment.data().schema == *schema
            && self.inline_fragment.data().parent_type_position == *parent_type
            && self.selection_set == normalized_selection_set
        {
            // normalization did not change the fragment
            Ok(Some(SelectionOrSet::Selection(Selection::InlineFragment(
                Arc::new(self.clone()),
            ))))
        } else if let Some(rebased) = self.inline_fragment.rebase_on(
            parent_type,
            schema,
            RebaseErrorHandlingOption::ThrowError,
        )? {
            Ok(Some(SelectionOrSet::Selection(Selection::InlineFragment(
                Arc::new(InlineFragmentSelection {
                    inline_fragment: rebased,
                    selection_set: normalized_selection_set,
                }),
            ))))
        } else {
            unreachable!("We should always be able to either rebase the inline fragment OR throw an exception");
        }
    }

    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Selection>, FederationError> {
        if &self.inline_fragment.data().schema == schema
            && self.inline_fragment.data().parent_type_position == *parent_type
        {
            // we are rebasing inline fragment on the same parent within the same schema - we can just return self
            return Ok(Some(Selection::from(self.clone())));
        }

        let Some(rebased_fragment) =
            self.inline_fragment
                .rebase_on(parent_type, schema, error_handling)?
        else {
            // rebasing failed but we are ignoring errors
            return Ok(None);
        };

        let rebased_casted_type = rebased_fragment
            .data()
            .type_condition_position
            .clone()
            .unwrap_or(rebased_fragment.data().parent_type_position.clone());
        if &self.inline_fragment.data().schema == schema && rebased_casted_type == *parent_type {
            // we are within the same schema - selection set does not have to be rebased
            Ok(Some(Selection::from_inline_fragment(
                rebased_fragment,
                self.selection_set.clone(),
            )))
        } else {
            let rebased_selection_set = self.selection_set.rebase_on(
                &rebased_casted_type,
                named_fragments,
                schema,
                error_handling,
            )?;
            if rebased_selection_set.selections.is_empty() {
                // empty selection set
                Ok(None)
            } else {
                Ok(Some(Selection::from_inline_fragment(
                    rebased_fragment,
                    rebased_selection_set,
                )))
            }
        }
    }

    pub(crate) fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> bool {
        if &self.inline_fragment.data().parent_type_position == parent_type
            && self.inline_fragment.data().schema == *schema
        {
            return true;
        }
        let Some(ty) = self
            .inline_fragment
            .data()
            .casted_type_if_add_to(parent_type, schema)
        else {
            return false;
        };
        if self.selection_set.type_position != ty {
            for sel in self.selection_set.selections.values() {
                if !sel.can_add_to(&ty, schema) {
                    return false;
                }
            }
            true
        } else {
            true
        }
    }

    pub(crate) fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        parent_schema: &ValidFederationSchema,
    ) -> bool {
        self.inline_fragment
            .can_rebase_on(parent_type, parent_schema)
            .0
    }

    pub(crate) fn casted_type(&self) -> &CompositeTypeDefinitionPosition {
        let data = self.inline_fragment.data();
        data.type_condition_position
            .as_ref()
            .unwrap_or(&data.parent_type_position)
    }

    pub(crate) fn has_defer(&self) -> bool {
        self.inline_fragment.data().directives.has("defer")
            || self
                .selection_set
                .selections
                .values()
                .any(|s| s.has_defer())
    }

    pub(crate) fn containment(
        &self,
        other: &Selection,
        options: ContainmentOptions,
    ) -> Containment {
        match other {
            // Using keys here means that @defer fragments never compare equal.
            // This is a bit odd but it is consistent: the selection set data structure would not
            // even try to compare two @defer fragments, because their keys are different.
            Selection::InlineFragment(other)
                if self.inline_fragment.key() == other.inline_fragment.key() =>
            {
                self.selection_set
                    .containment(&other.selection_set, options)
            }
            _ => Containment::NotContained,
        }
    }

    /// Returns true if this selection is a superset of the other selection.
    pub(crate) fn contains(&self, other: &Selection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl<'a> InlineFragmentSelectionValue<'a> {
    /// Merges the given normalized inline fragment selections into this one (this method assumes
    /// the keys already match).
    pub(crate) fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op InlineFragmentSelection>,
    ) -> Result<(), FederationError> {
        let self_inline_fragment = &self.get().inline_fragment;
        let mut selection_sets = vec![];
        for other in others {
            let other_inline_fragment = &other.inline_fragment;
            if other_inline_fragment.data().schema != self_inline_fragment.data().schema {
                return Err(Internal {
                    message: "Cannot merge inline fragment from different schemas".to_owned(),
                }
                .into());
            }
            if other_inline_fragment.data().parent_type_position
                != self_inline_fragment.data().parent_type_position
            {
                return Err(Internal {
                    message: format!(
                        "Cannot merge inline fragment of parent type \"{}\" into an inline fragment of parent type \"{}\"",
                        other_inline_fragment.data().parent_type_position,
                        self_inline_fragment.data().parent_type_position,
                    ),
                }.into());
            }
            selection_sets.push(&other.selection_set);
        }
        self.get_selection_set_mut()
            .merge_into(selection_sets.into_iter())?;
        Ok(())
    }
}

impl InlineFragment {
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<InlineFragment>, FederationError> {
        if &self.data().parent_type_position == parent_type {
            return Ok(Some(self.clone()));
        }

        let type_condition = self.data().type_condition_position.clone();
        // This usually imply that the fragment is not from the same subgraph than the selection. So we need
        // to update the source type of the fragment, but also "rebase" the condition to the selection set
        // schema.
        let (can_rebase, rebased_condition) = self.can_rebase_on(parent_type, schema);
        if !can_rebase {
            if let RebaseErrorHandlingOption::ThrowError = error_handling {
                let printable_type_condition = self
                    .data()
                    .type_condition_position
                    .clone()
                    .map_or_else(|| "".to_string(), |t| t.to_string());
                let printable_runtimes = type_condition.map_or_else(
                    || "undefined".to_string(),
                    |t| print_possible_runtimes(&t, schema),
                );
                let printable_parent_runtimes = print_possible_runtimes(parent_type, schema);
                Err(FederationError::internal(
                    format!("Cannot add fragment of condition \"{}\" (runtimes: [{}]) to parent type \"{}\" (runtimes: [{})",
                            printable_type_condition,
                            printable_runtimes,
                            parent_type,
                            printable_parent_runtimes,
                    ),
                ))
            } else {
                Ok(None)
            }
        } else {
            let mut rebased_fragment_data = self.data().clone();
            rebased_fragment_data.type_condition_position = rebased_condition;
            Ok(Some(InlineFragment::new(rebased_fragment_data)))
        }
    }

    pub(crate) fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        parent_schema: &ValidFederationSchema,
    ) -> (bool, Option<CompositeTypeDefinitionPosition>) {
        if self.data().type_condition_position.is_none() {
            // can_rebase = true, condition = undefined
            return (true, None);
        }

        if let Some(Ok(rebased_condition)) = self
            .data()
            .type_condition_position
            .clone()
            .and_then(|condition_position| {
                parent_schema.try_get_type(condition_position.type_name().clone())
            })
            .map(|rebased_condition_position| {
                CompositeTypeDefinitionPosition::try_from(rebased_condition_position)
            })
        {
            // chained if let chains are not yet supported
            // see https://github.com/rust-lang/rust/issues/53667
            if runtime_types_intersect(parent_type, &rebased_condition, parent_schema) {
                // can_rebase = true, condition = rebased_condition
                (true, Some(rebased_condition))
            } else {
                (false, None)
            }
        } else {
            // can_rebase = false, condition = undefined
            (false, None)
        }
    }
}

pub(crate) fn merge_selection_sets(
    mut selection_sets: Vec<SelectionSet>,
) -> Result<SelectionSet, FederationError> {
    let Some((first, remainder)) = selection_sets.split_first_mut() else {
        return Err(Internal {
            message: "".to_owned(),
        }
        .into());
    };
    first.merge_into(remainder.iter())?;

    // Take ownership of the first element and discard the rest;
    // we can unwrap because `split_first_mut()` guarantees at least one element will be yielded
    Ok(selection_sets.into_iter().next().unwrap())
}

/// Options for handling rebasing errors.
#[derive(Clone, Copy)]
pub(crate) enum RebaseErrorHandlingOption {
    IgnoreError,
    ThrowError,
}

/// Options for normalizing the selection sets
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NormalizeSelectionOption {
    NormalizeRecursively,
    NormalizeSingleSelection,
}

/// This uses internal copy-on-write optimization to make `Clone` cheap.
/// However a cloned `NamedFragments` still behaves like a deep copy:
/// unlike in JS where we can have multiple references to a mutable map,
/// here modifying a cloned map will leave the original unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct NamedFragments {
    fragments: Arc<IndexMap<Name, Node<Fragment>>>,
}

impl NamedFragments {
    pub(crate) fn new(
        fragments: &IndexMap<Name, Node<executable::Fragment>>,
        schema: &ValidFederationSchema,
    ) -> NamedFragments {
        // JS PORT - In order to normalize Fragments we need to process them in dependency order.
        //
        // In JS implementation mapInDependencyOrder method was called when rebasing/filtering/expanding selection sets.
        // Since resulting `IndexMap` of `NormalizedFragments` will be already sorted, we only need to map it once
        // when creating the `NamedFragments`.
        NamedFragments::initialize_in_dependency_order(fragments, schema)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.fragments.len() == 0
    }

    pub(crate) fn size(&self) -> usize {
        self.fragments.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Node<Fragment>> {
        self.fragments.values()
    }

    pub(crate) fn iter_rev(&self) -> impl Iterator<Item = &Node<Fragment>> {
        self.fragments.values().rev()
    }

    pub(crate) fn iter_mut(&mut self) -> indexmap::map::IterMut<'_, Name, Node<Fragment>> {
        Arc::make_mut(&mut self.fragments).iter_mut()
    }

    // Calls `retain` on the underlying `IndexMap`.
    pub(crate) fn retain(&mut self, mut predicate: impl FnMut(&Name, &Node<Fragment>) -> bool) {
        Arc::make_mut(&mut self.fragments).retain(|name, fragment| predicate(name, fragment));
    }

    fn insert(&mut self, fragment: Fragment) {
        Arc::make_mut(&mut self.fragments).insert(fragment.name.clone(), Node::new(fragment));
    }

    fn try_insert(&mut self, fragment: Fragment) -> Result<(), FederationError> {
        match Arc::make_mut(&mut self.fragments).entry(fragment.name.clone()) {
            indexmap::map::Entry::Occupied(_) => {
                Err(FederationError::internal("Duplicate fragment name"))
            }
            indexmap::map::Entry::Vacant(entry) => {
                let _ = entry.insert(Node::new(fragment));
                Ok(())
            }
        }
    }

    pub(crate) fn get(&self, name: &Name) -> Option<Node<Fragment>> {
        self.fragments.get(name).cloned()
    }

    pub(crate) fn contains(&self, name: &Name) -> bool {
        self.fragments.contains_key(name)
    }

    /**
     * Collect the usages of fragments that are used within the selection of other fragments.
     */
    pub(crate) fn collect_used_fragment_names(&self, aggregator: &mut HashMap<Name, i32>) {
        for fragment in self.fragments.values() {
            fragment
                .selection_set
                .collect_used_fragment_names(aggregator);
        }
    }

    /// JS PORT NOTE: In JS implementation this method was named mapInDependencyOrder and accepted a lambda to
    /// apply transformation on the fragments. It was called when rebasing/filtering/expanding selection sets.
    /// JS PORT NOTE: In JS implementation this method was potentially returning `undefined`. In order to simplify the code
    /// we will always return `NamedFragments` even if they are empty.
    ///
    /// We normalize passed in fragments in their dependency order, i.e. if a fragment A uses another fragment B, then we will
    /// normalize B _before_ attempting to normalize A. Normalized fragments have access to previously normalized fragments.
    fn initialize_in_dependency_order(
        fragments: &IndexMap<Name, Node<executable::Fragment>>,
        schema: &ValidFederationSchema,
    ) -> NamedFragments {
        struct FragmentDependencies {
            fragment: Node<executable::Fragment>,
            depends_on: Vec<Name>,
        }

        // Note: We use IndexMap to stabilize the ordering of the result, which influences
        //       the outcome of `map_to_expanded_selection_sets`.
        let mut fragments_map: IndexMap<Name, FragmentDependencies> = IndexMap::new();
        for fragment in fragments.values() {
            let mut fragment_usages: HashMap<Name, i32> = HashMap::new();
            NamedFragments::collect_fragment_usages(&fragment.selection_set, &mut fragment_usages);
            let usages: Vec<Name> = fragment_usages.keys().cloned().collect::<Vec<Name>>();
            fragments_map.insert(
                fragment.name.clone(),
                FragmentDependencies {
                    fragment: fragment.clone(),
                    depends_on: usages,
                },
            );
        }

        let mut removed_fragments: HashSet<Name> = HashSet::new();
        let mut mapped_fragments = NamedFragments::default();
        while !fragments_map.is_empty() {
            // Note that graphQL specifies that named fragments cannot have cycles (https://spec.graphql.org/draft/#sec-Fragment-spreads-must-not-form-cycles)
            // and so we're guaranteed that on every iteration, at least one element of the map is removed (so the `while` loop will terminate).
            fragments_map.retain(|name, info| {
                let can_remove = info
                    .depends_on
                    .iter()
                    .all(|n| mapped_fragments.contains(n) || removed_fragments.contains(n));
                if can_remove {
                    if let Ok(normalized) =
                        Fragment::from_fragment(&info.fragment, &mapped_fragments, schema)
                    {
                        // TODO this actually throws in JS code -> should we also throw?
                        // JS code has methods for
                        // * add and throw exception if entry already there
                        // * add_if_not_exists
                        // Rust HashMap exposes insert (that overwrites) and try_insert (that throws)
                        mapped_fragments.insert(normalized);
                    } else {
                        removed_fragments.insert(name.clone());
                    }
                }
                // keep only the elements that cannot be removed
                !can_remove
            });
        }
        mapped_fragments
    }

    // JS PORT - we need to calculate those for both executable::SelectionSet and SelectionSet
    fn collect_fragment_usages(
        selection_set: &executable::SelectionSet,
        aggregator: &mut HashMap<Name, i32>,
    ) {
        selection_set.selections.iter().for_each(|s| match s {
            executable::Selection::Field(f) => {
                NamedFragments::collect_fragment_usages(&f.selection_set, aggregator);
            }
            executable::Selection::InlineFragment(i) => {
                NamedFragments::collect_fragment_usages(&i.selection_set, aggregator);
            }
            executable::Selection::FragmentSpread(f) => {
                let current_count = aggregator.entry(f.fragment_name.clone()).or_default();
                *current_count += 1;
            }
        })
    }

    /// When we rebase named fragments on a subgraph schema, only a subset of what the fragment handles may belong
    /// to that particular subgraph. And there are a few sub-cases where that subset is such that we basically need or
    /// want to consider to ignore the fragment for that subgraph, and that is when:
    /// 1. the subset that apply is actually empty. The fragment wouldn't be valid in this case anyway.
    /// 2. the subset is a single leaf field: in that case, using the one field directly is just shorter than using
    ///   the fragment, so we consider the fragment don't really apply to that subgraph. Technically, using the
    ///   fragment could still be of value if the fragment name is a lot smaller than the one field name, but it's
    ///   enough of a niche case that we ignore it. Note in particular that one sub-case of this rule that is likely
    ///   to be common is when the subset ends up being just `__typename`: this would basically mean the fragment
    ///   don't really apply to the subgraph, and that this will ensure this is the case.
    pub(crate) fn is_selection_set_worth_using(selection_set: &SelectionSet) -> bool {
        if selection_set.selections.len() == 0 {
            return false;
        }
        if selection_set.selections.len() == 1 {
            // true if NOT field selection OR non-leaf field
            return if let Some((_, Selection::Field(field_selection))) =
                selection_set.selections.first()
            {
                field_selection.selection_set.is_some()
            } else {
                true
            };
        }
        true
    }

    pub(crate) fn rebase_on(
        &self,
        schema: &ValidFederationSchema,
    ) -> Result<NamedFragments, FederationError> {
        let mut rebased_fragments = NamedFragments::default();
        for fragment in self.fragments.values() {
            if let Ok(rebased_type) = schema
                .get_type(fragment.type_condition_position.type_name().clone())
                .and_then(CompositeTypeDefinitionPosition::try_from)
            {
                if let Ok(mut rebased_selection) = fragment.selection_set.rebase_on(
                    &rebased_type,
                    &rebased_fragments,
                    schema,
                    RebaseErrorHandlingOption::IgnoreError,
                ) {
                    // Rebasing can leave some inefficiencies in some case (particularly when a spread has to be "expanded", see `FragmentSpreadSelection.rebaseOn`),
                    // so we do a top-level normalization to keep things clean.
                    rebased_selection = rebased_selection.normalize(
                        &rebased_type,
                        &rebased_fragments,
                        schema,
                        NormalizeSelectionOption::NormalizeRecursively,
                    )?;
                    if NamedFragments::is_selection_set_worth_using(&rebased_selection) {
                        let fragment = Fragment {
                            schema: schema.clone(),
                            name: fragment.name.clone(),
                            type_condition_position: rebased_type.clone(),
                            directives: fragment.directives.clone(),
                            selection_set: rebased_selection,
                        };
                        rebased_fragments.insert(fragment);
                    }
                }
            }
        }
        Ok(rebased_fragments)
    }

    /// - Expands all nested fragments
    /// - Applies the provided `mapper` to each selection set of the expanded fragments.
    /// - Finally, re-fragments the nested fragments.
    fn map_to_expanded_selection_sets(
        &self,
        mut mapper: impl FnMut(&SelectionSet) -> Result<SelectionSet, FederationError>,
    ) -> Result<NamedFragments, FederationError> {
        let mut result = NamedFragments::default();
        // Note: `self.fragments` has insertion order topologically sorted.
        for fragment in self.fragments.values() {
            let expanded_selection_set = fragment.selection_set.expand_all_fragments()?.normalize(
                &fragment.type_condition_position,
                &Default::default(),
                &fragment.schema,
                NormalizeSelectionOption::NormalizeRecursively,
            )?;
            let mut mapped_selection_set = mapper(&expanded_selection_set)?;
            mapped_selection_set.optimize_at_root(&result)?;
            let updated = Fragment {
                selection_set: mapped_selection_set,
                schema: fragment.schema.clone(),
                name: fragment.name.clone(),
                type_condition_position: fragment.type_condition_position.clone(),
                directives: fragment.directives.clone(),
            };
            result.insert(updated);
        }
        Ok(result)
    }

    pub(crate) fn add_typename_field_for_abstract_types_in_named_fragments(
        &self,
    ) -> Result<Self, FederationError> {
        // This method is a bit tricky due to potentially nested fragments. More precisely, suppose that
        // we have:
        //   fragment MyFragment on T {
        //     a {
        //       b {
        //         ...InnerB
        //       }
        //     }
        //   }
        //
        //   fragment InnerB on B {
        //     __typename
        //     x
        //     y
        //   }
        // then if we were to "naively" add `__typename`, the first fragment would end up being:
        //   fragment MyFragment on T {
        //     a {
        //       __typename
        //       b {
        //         __typename
        //         ...InnerX
        //       }
        //     }
        //   }
        // but that's not ideal because the inner-most `__typename` is already within `InnerX`. And that
        // gets in the way to re-adding fragments (the `SelectionSet.optimize` method) because if we start
        // with:
        //   {
        //     a {
        //       __typename
        //       b {
        //         __typename
        //         x
        //         y
        //       }
        //     }
        //   }
        // and add `InnerB` first, we get:
        //   {
        //     a {
        //       __typename
        //       b {
        //         ...InnerB
        //       }
        //     }
        //   }
        // and it becomes tricky to recognize the "updated-with-typename" version of `MyFragment` now (we "seem"
        // to miss a `__typename`).
        //
        // Anyway, to avoid this issue, what we do is that for every fragment, we:
        //  1. expand any nested fragments in its selection.
        //  2. add `__typename` where we should in that expanded selection.
        //  3. re-optimize all fragments (using the "updated-with-typename" versions).
        // which is what `mapToExpandedSelectionSets` gives us.

        if self.is_empty() {
            // PORT_NOTE: This was an assertion failure in JS version. But, it's actually ok to
            // return unchanged if empty.
            return Ok(self.clone());
        }
        let updated = self.map_to_expanded_selection_sets(|ss| {
            ss.add_typename_field_for_abstract_types(
                /*parent_type_if_abstract*/ None, /*fragments*/ &None,
            )
        })?;
        // PORT_NOTE: The JS version asserts if `updated` is empty or not. But, we really want to
        // check the `updated` has the same set of fragments. To avoid performance hit, only the
        // size is checked here.
        if updated.size() != self.size() {
            return Err(FederationError::internal(
                "Unexpected change in the number of fragments",
            ));
        }
        Ok(updated)
    }
}

#[derive(Clone)]
pub(crate) struct RebasedFragments {
    pub(crate) original_fragments: NamedFragments,
    // JS PORT NOTE: In JS implementation values were optional
    /// Map key: subgraph name
    rebased_fragments: Arc<HashMap<NodeStr, NamedFragments>>,
}

impl RebasedFragments {
    pub(crate) fn new(fragments: NamedFragments) -> Self {
        Self {
            original_fragments: fragments,
            rebased_fragments: Arc::new(HashMap::new()),
        }
    }

    pub(crate) fn for_subgraph(
        &mut self,
        subgraph_name: impl Into<NodeStr>,
        subgraph_schema: &ValidFederationSchema,
    ) -> &NamedFragments {
        Arc::make_mut(&mut self.rebased_fragments)
            .entry(subgraph_name.into())
            .or_insert_with(|| {
                self.original_fragments
                    .rebase_on(subgraph_schema)
                    .unwrap_or_default()
            })
    }
}

impl TryFrom<&Operation> for executable::Operation {
    type Error = FederationError;

    fn try_from(normalized_operation: &Operation) -> Result<Self, Self::Error> {
        let operation_type: executable::OperationType = normalized_operation.root_kind.into();
        Ok(Self {
            operation_type,
            name: normalized_operation.name.clone(),
            variables: normalized_operation.variables.deref().clone(),
            directives: normalized_operation.directives.deref().clone(),
            selection_set: (&normalized_operation.selection_set).try_into()?,
        })
    }
}

impl TryFrom<&Fragment> for executable::Fragment {
    type Error = FederationError;

    fn try_from(normalized_fragment: &Fragment) -> Result<Self, Self::Error> {
        Ok(Self {
            name: normalized_fragment.name.clone(),
            directives: normalized_fragment.directives.deref().clone(),
            selection_set: (&normalized_fragment.selection_set).try_into()?,
        })
    }
}

impl TryFrom<&SelectionSet> for executable::SelectionSet {
    type Error = FederationError;

    fn try_from(val: &SelectionSet) -> Result<Self, Self::Error> {
        let mut flattened = vec![];
        for normalized_selection in val.selections.values() {
            let selection: executable::Selection = normalized_selection.try_into()?;
            if let executable::Selection::Field(field) = &selection {
                if field.name == *INTROSPECTION_TYPENAME_FIELD_NAME && field.alias.is_none() {
                    // Move unaliased __typename to the start of the selection set.
                    // This looks nicer, and matches existing tests.
                    // PORT_NOTE: JS does this in `selectionsInPrintOrder`
                    flattened.insert(0, selection);
                    continue;
                }
            }
            flattened.push(selection);
        }
        Ok(Self {
            ty: val.type_position.type_name().clone(),
            selections: flattened,
        })
    }
}

impl TryFrom<&Selection> for executable::Selection {
    type Error = FederationError;

    fn try_from(val: &Selection) -> Result<Self, Self::Error> {
        Ok(match val {
            Selection::Field(normalized_field_selection) => executable::Selection::Field(
                Node::new(normalized_field_selection.deref().try_into()?),
            ),
            Selection::FragmentSpread(normalized_fragment_spread_selection) => {
                executable::Selection::FragmentSpread(Node::new(
                    normalized_fragment_spread_selection.deref().into(),
                ))
            }
            Selection::InlineFragment(normalized_inline_fragment_selection) => {
                executable::Selection::InlineFragment(Node::new(
                    normalized_inline_fragment_selection.deref().try_into()?,
                ))
            }
        })
    }
}

impl TryFrom<&Field> for executable::Field {
    type Error = FederationError;

    fn try_from(normalized_field: &Field) -> Result<Self, Self::Error> {
        let definition = normalized_field
            .data()
            .field_position
            .get(normalized_field.data().schema.schema())?
            .node
            .to_owned();
        let selection_set = executable::SelectionSet {
            ty: definition.ty.inner_named_type().clone(),
            selections: vec![],
        };
        Ok(Self {
            definition,
            alias: normalized_field.data().alias.to_owned(),
            name: normalized_field.data().name().to_owned(),
            arguments: normalized_field.data().arguments.deref().to_owned(),
            directives: normalized_field.data().directives.deref().to_owned(),
            selection_set,
        })
    }
}

impl TryFrom<&FieldSelection> for executable::Field {
    type Error = FederationError;

    fn try_from(val: &FieldSelection) -> Result<Self, Self::Error> {
        let mut field = Self::try_from(&val.field)?;
        if let Some(selection_set) = &val.selection_set {
            field.selection_set = selection_set.try_into()?;
        }
        Ok(field)
    }
}

impl TryFrom<&InlineFragment> for executable::InlineFragment {
    type Error = FederationError;

    fn try_from(normalized_inline_fragment: &InlineFragment) -> Result<Self, Self::Error> {
        let type_condition = normalized_inline_fragment
            .data()
            .type_condition_position
            .as_ref()
            .map(|pos| pos.type_name().clone());
        let ty = type_condition.clone().unwrap_or_else(|| {
            normalized_inline_fragment
                .data()
                .parent_type_position
                .type_name()
                .clone()
        });
        Ok(Self {
            type_condition,
            directives: normalized_inline_fragment
                .data()
                .directives
                .deref()
                .to_owned(),
            selection_set: executable::SelectionSet {
                ty,
                selections: Vec::new(),
            },
        })
    }
}

impl TryFrom<&InlineFragmentSelection> for executable::InlineFragment {
    type Error = FederationError;

    fn try_from(val: &InlineFragmentSelection) -> Result<Self, Self::Error> {
        Ok(Self {
            selection_set: (&val.selection_set).try_into()?,
            ..Self::try_from(&val.inline_fragment)?
        })
    }
}

impl From<&FragmentSpreadSelection> for executable::FragmentSpread {
    fn from(val: &FragmentSpreadSelection) -> Self {
        let normalized_fragment_spread = &val.spread;
        Self {
            fragment_name: normalized_fragment_spread.data().fragment_name.to_owned(),
            directives: normalized_fragment_spread
                .data()
                .directives
                .deref()
                .to_owned(),
        }
    }
}

impl TryFrom<Operation> for Valid<executable::ExecutableDocument> {
    type Error = FederationError;

    fn try_from(value: Operation) -> Result<Self, Self::Error> {
        let operation = executable::Operation::try_from(&value)?;
        let fragments = value
            .named_fragments
            .fragments
            .iter()
            .map(|(name, fragment)| {
                Ok((
                    name.clone(),
                    Node::new(executable::Fragment::try_from(&**fragment)?),
                ))
            })
            .collect::<Result<IndexMap<_, _>, FederationError>>()?;

        let mut document = executable::ExecutableDocument::new();
        document.fragments = fragments;
        document.insert_operation(operation);
        Ok(document.validate(value.schema.schema())?)
    }
}

impl Display for Operation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let operation: executable::Operation = match self.try_into() {
            Ok(operation) => operation,
            Err(_) => return Err(std::fmt::Error),
        };
        for fragment_def in self.named_fragments.iter() {
            fragment_def.fmt(f)?;
            f.write_str("\n\n")?;
        }
        operation.serialize().fmt(f)
    }
}

impl Display for Fragment {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let fragment: executable::Fragment = match self.try_into() {
            Ok(fragment) => fragment,
            Err(_) => return Err(std::fmt::Error),
        };
        fragment.serialize().fmt(f)
    }
}

impl Display for SelectionSet {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let selection_set: executable::SelectionSet = match self.try_into() {
            Ok(selection_set) => selection_set,
            Err(_) => return Err(std::fmt::Error),
        };
        selection_set.serialize().no_indent().fmt(f)
    }
}

impl Display for Selection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let selection: executable::Selection = match self.try_into() {
            Ok(selection) => selection,
            Err(_) => return Err(std::fmt::Error),
        };
        selection.serialize().no_indent().fmt(f)
    }
}

impl Display for FieldSelection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let field: executable::Field = match self.try_into() {
            Ok(field) => field,
            Err(_) => return Err(std::fmt::Error),
        };
        field.serialize().no_indent().fmt(f)
    }
}

impl Display for InlineFragmentSelection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let inline_fragment: executable::InlineFragment = match self.try_into() {
            Ok(inline_fragment) => inline_fragment,
            Err(_) => return Err(std::fmt::Error),
        };
        inline_fragment.serialize().no_indent().fmt(f)
    }
}

impl Display for FragmentSpreadSelection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let fragment_spread: executable::FragmentSpread = self.into();
        fragment_spread.serialize().no_indent().fmt(f)
    }
}

impl Display for Field {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // We create a selection with an empty selection set here, relying on `apollo-rs` to skip
        // serializing it when empty. Note we're implicitly relying on the lack of type-checking
        // in both `FieldSelection` and `Field` display logic (specifically, we rely on
        // them not checking whether it is valid for the selection set to be empty).
        self.clone().with_subselection(None).fmt(f)
    }
}

impl Display for InlineFragment {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // We can't use the same trick we did with `Field`'s display logic, since
        // selection sets are non-optional for inline fragment selections.
        let data = self.data();
        if let Some(type_name) = &data.type_condition_position {
            f.write_str("... on ")?;
            f.write_str(type_name.type_name())?;
        } else {
            f.write_str("...")?;
        }
        data.directives.serialize().no_indent().fmt(f)
    }
}

fn directives_with_sorted_arguments(
    directives: &executable::DirectiveList,
) -> executable::DirectiveList {
    let mut directives = directives.clone();
    for directive in &mut directives {
        directive
            .make_mut()
            .arguments
            .sort_by(|a1, a2| a1.name.cmp(&a2.name))
    }
    directives
}

fn is_deferred_selection(directives: &executable::DirectiveList) -> bool {
    directives.has("defer")
}

/// Normalizes the selection set of the specified operation.
///
/// This method applies the following transformations:
/// - Merge selections with the same normalization "key".
/// - Expand fragment spreads into inline fragments.
/// - Remove `__schema` or `__type` introspection fields at all levels, as these shouldn't be
///   handled by query planning.
/// - Hoist fragment spreads/inline fragments into their parents if they have no directives and
///   their parent type matches.
pub(crate) fn normalize_operation(
    operation: &executable::Operation,
    named_fragments: NamedFragments,
    schema: &ValidFederationSchema,
    interface_types_with_interface_objects: &IndexSet<InterfaceTypeDefinitionPosition>,
) -> Result<Operation, FederationError> {
    let mut normalized_selection_set =
        SelectionSet::from_selection_set(&operation.selection_set, &named_fragments, schema)?;
    normalized_selection_set = normalized_selection_set.expand_all_fragments()?;
    normalized_selection_set.optimize_sibling_typenames(interface_types_with_interface_objects)?;

    let normalized_operation = Operation {
        schema: schema.clone(),
        root_kind: operation.operation_type.into(),
        name: operation.name.clone(),
        variables: Arc::new(operation.variables.clone()),
        directives: Arc::new(operation.directives.clone()),
        selection_set: normalized_selection_set,
        named_fragments,
    };
    Ok(normalized_operation)
}

// TODO remove once it is available in schema metadata
fn is_interface_object(obj: &ObjectTypeDefinitionPosition, schema: &ValidFederationSchema) -> bool {
    if let Ok(intf_obj_directive) = get_federation_spec_definition_from_subgraph(schema)
        .and_then(|spec| spec.interface_object_directive(schema))
    {
        obj.try_get(schema.schema())
            .is_some_and(|o| o.directives.has(&intf_obj_directive.name))
    } else {
        false
    }
}

fn runtime_types_intersect(
    type1: &CompositeTypeDefinitionPosition,
    type2: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> bool {
    if type1 == type2 {
        return true;
    }

    if let (Ok(runtimes_1), Ok(runtimes_2)) = (
        schema.possible_runtime_types(type1.clone()),
        schema.possible_runtime_types(type2.clone()),
    ) {
        return runtimes_1.intersection(&runtimes_2).next().is_some();
    }

    false
}

fn print_possible_runtimes(
    composite_type: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> String {
    schema
        .possible_runtime_types(composite_type.clone())
        .map_or_else(
            |_| "undefined".to_string(),
            |runtimes| {
                runtimes
                    .iter()
                    .map(|r| r.type_name.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            },
        )
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::ExecutableDocument;
    use indexmap::IndexSet;

    use super::normalize_operation;
    use super::Containment;
    use super::ContainmentOptions;
    use super::Name;
    use super::NamedFragments;
    use super::Operation;
    use super::Selection;
    use super::SelectionKey;
    use super::SelectionSet;
    use crate::query_graph::graph_path::OpPathElement;
    use crate::schema::position::InterfaceTypeDefinitionPosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use crate::subgraph::Subgraph;

    fn parse_schema_and_operation(
        schema_and_operation: &str,
    ) -> (ValidFederationSchema, ExecutableDocument) {
        let (schema, executable_document) =
            apollo_compiler::parse_mixed_validate(schema_and_operation, "document.graphql")
                .unwrap();
        let executable_document = executable_document.into_inner();
        let schema = ValidFederationSchema::new(schema).unwrap();
        (schema, executable_document)
    }

    fn parse_subgraph(name: &str, schema: &str) -> ValidFederationSchema {
        let parsed_schema =
            Subgraph::parse_and_expand(name, &format!("https://{name}"), schema).unwrap();
        ValidFederationSchema::new(parsed_schema.schema).unwrap()
    }

    #[test]
    fn expands_named_fragments() {
        let operation_with_named_fragment = r#"
query NamedFragmentQuery {
  foo {
    id
    ...Bar
  }
}

fragment Bar on Foo {
  bar
  baz
}

type Query {
  foo: Foo
}

type Foo {
  id: ID!
  bar: String!
  baz: Int
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_with_named_fragment);
        if let Some(operation) = executable_document
            .named_operations
            .get_mut("NamedFragmentQuery")
        {
            let mut normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            normalized_operation.named_fragments = Default::default();
            insta::assert_snapshot!(normalized_operation, @r###"
                query NamedFragmentQuery {
                  foo {
                    id
                    bar
                    baz
                  }
                }
            "###);
        }
    }

    #[test]
    fn expands_and_deduplicates_fragments() {
        let operation_with_named_fragment = r#"
query NestedFragmentQuery {
  foo {
    ...FirstFragment
    ...SecondFragment
  }
}

fragment FirstFragment on Foo {
  id
  bar
  baz
}

fragment SecondFragment on Foo {
  id
  bar
}

type Query {
  foo: Foo
}

type Foo {
  id: ID!
  bar: String!
  baz: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_with_named_fragment);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let mut normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            normalized_operation.named_fragments = Default::default();
            insta::assert_snapshot!(normalized_operation, @r###"
              query NestedFragmentQuery {
                foo {
                  id
                  bar
                  baz
                }
              }
            "###);
        }
    }

    #[test]
    fn can_remove_introspection_selections() {
        let operation_with_introspection = r#"
query TestIntrospectionQuery {
  __schema {
    types {
      name
    }
  }
}

type Query {
  foo: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_with_introspection);
        if let Some(operation) = executable_document
            .named_operations
            .get_mut("TestIntrospectionQuery")
        {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            assert!(normalized_operation.selection_set.selections.is_empty());
        }
    }

    #[test]
    fn merge_same_fields_without_directives() {
        let operation_string = r#"
query Test {
  t {
    v1
  }
  t {
    v2
 }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_string);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t {
    v1
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn merge_same_fields_with_same_directive() {
        let operation_with_directives = r#"
query Test($skipIf: Boolean!) {
  t @skip(if: $skipIf) {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_with_directives);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test($skipIf: Boolean!) {
  t @skip(if: $skipIf) {
    v1
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn merge_same_fields_with_same_directive_but_different_arg_order() {
        let operation_with_directives_different_arg_order = r#"
query Test($skipIf: Boolean!) {
  t @customSkip(if: $skipIf, label: "foo") {
    v1
  }
  t @customSkip(label: "foo", if: $skipIf) {
    v2
  }
}

directive @customSkip(if: Boolean!, label: String!) on FIELD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_with_directives_different_arg_order);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test($skipIf: Boolean!) {
  t @customSkip(if: $skipIf, label: "foo") {
    v1
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn do_not_merge_when_only_one_field_specifies_directive() {
        let operation_one_field_with_directives = r#"
query Test($skipIf: Boolean!) {
  t {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_one_field_with_directives);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test($skipIf: Boolean!) {
  t {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn do_not_merge_when_fields_have_different_directives() {
        let operation_different_directives = r#"
query Test($skip1: Boolean!, $skip2: Boolean!) {
  t @skip(if: $skip1) {
    v1
  }
  t @skip(if: $skip2) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_different_directives);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test($skip1: Boolean!, $skip2: Boolean!) {
  t @skip(if: $skip1) {
    v1
  }
  t @skip(if: $skip2) {
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn do_not_merge_fields_with_defer_directive() {
        let operation_defer_fields = r#"
query Test {
  t {
    ... @defer {
      v1
    }
  }
  t {
    ... @defer {
      v2
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_defer_fields);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t {
    ... @defer {
      v1
    }
    ... @defer {
      v2
    }
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn merge_nested_field_selections() {
        let nested_operation = r#"
query Test {
  t {
    t1
    ... @defer {
      v {
        v1
      }
    }
  }
  t {
    t1
    t2
    ... @defer {
      v {
        v2
      }
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  t1: Int
  t2: String
  v: V
}

type V {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(nested_operation);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t {
    t1
    ... @defer {
      v {
        v1
      }
    }
    t2
    ... @defer {
      v {
        v2
      }
    }
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    //
    // inline fragments
    //

    #[test]
    fn merge_same_fragment_without_directives() {
        let operation_with_fragments = r#"
query Test {
  t {
    ... on T {
      v1
    }
    ... on T {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_with_fragments);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t {
    v1
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn merge_same_fragments_with_same_directives() {
        let operation_fragments_with_directives = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T @skip(if: $skipIf) {
      v1
    }
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_fragments_with_directives);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test($skipIf: Boolean!) {
  t {
    ... on T @skip(if: $skipIf) {
      v1
      v2
    }
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn merge_same_fragments_with_same_directive_but_different_arg_order() {
        let operation_fragments_with_directives_args_order = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T @customSkip(if: $skipIf, label: "foo") {
      v1
    }
    ... on T @customSkip(label: "foo", if: $skipIf) {
      v2
    }
  }
}

directive @customSkip(if: Boolean!, label: String!) on FIELD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_fragments_with_directives_args_order);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test($skipIf: Boolean!) {
  t {
    ... on T @customSkip(if: $skipIf, label: "foo") {
      v1
      v2
    }
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn do_not_merge_when_only_one_fragment_specifies_directive() {
        let operation_one_fragment_with_directive = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T {
      v1
    }
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_one_fragment_with_directive);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test($skipIf: Boolean!) {
  t {
    v1
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn do_not_merge_when_fragments_have_different_directives() {
        let operation_fragments_with_different_directive = r#"
query Test($skip1: Boolean!, $skip2: Boolean!) {
  t {
    ... on T @skip(if: $skip1) {
      v1
    }
    ... on T @skip(if: $skip2) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_fragments_with_different_directive);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test($skip1: Boolean!, $skip2: Boolean!) {
  t {
    ... on T @skip(if: $skip1) {
      v1
    }
    ... on T @skip(if: $skip2) {
      v2
    }
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn do_not_merge_fragments_with_defer_directive() {
        let operation_fragments_with_defer = r#"
query Test {
  t {
    ... on T @defer {
      v1
    }
    ... on T @defer {
      v2
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_fragments_with_defer);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t {
    ... on T @defer {
      v1
    }
    ... on T @defer {
      v2
    }
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn merge_nested_fragments() {
        let operation_nested_fragments = r#"
query Test {
  t {
    ... on T {
      t1
    }
    ... on T {
      v {
        v1
      }
    }
  }
  t {
    ... on T {
      t1
      t2
    }
    ... on T {
      v {
        v2
      }
    }
  }
}

type Query {
  t: T
}

type T {
  t1: Int
  t2: String
  v: V
}

type V {
  v1: Int
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_nested_fragments);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t {
    t1
    v {
      v1
      v2
    }
    t2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    #[test]
    fn removes_sibling_typename() {
        let operation_with_typename = r#"
query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}

type Query {
  foo: Foo
}

type Foo {
  v1: ID!
  v2: String
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_with_typename);
        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query TestQuery {
  foo {
    v1
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn keeps_typename_if_no_other_selection() {
        let operation_with_single_typename = r#"
query TestQuery {
  foo {
    __typename
  }
}

type Query {
  foo: Foo
}

type Foo {
  v1: ID!
  v2: String
}
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_with_single_typename);
        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query TestQuery {
  foo {
    __typename
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn keeps_typename_for_interface_object() {
        let operation_with_intf_object_typename = r#"
query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}

directive @interfaceObject on OBJECT
directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

type Query {
  foo: Foo
}

type Foo @interfaceObject @key(fields: "id") {
  v1: ID!
  v2: String
}

scalar FieldSet
"#;
        let (schema, mut executable_document) =
            parse_schema_and_operation(operation_with_intf_object_typename);
        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let mut interface_objects: IndexSet<InterfaceTypeDefinitionPosition> = IndexSet::new();
            interface_objects.insert(InterfaceTypeDefinitionPosition {
                type_name: name!("Foo"),
            });

            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &interface_objects,
            )
            .unwrap();
            let expected = r#"query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        }
    }

    //
    // REBASE TESTS
    //
    #[cfg(test)]
    mod rebase_tests {
        use apollo_compiler::name;
        use indexmap::IndexSet;

        use crate::query_plan::operation::normalize_operation;
        use crate::query_plan::operation::tests::parse_schema_and_operation;
        use crate::query_plan::operation::tests::parse_subgraph;
        use crate::query_plan::operation::NamedFragments;
        use crate::schema::position::InterfaceTypeDefinitionPosition;

        #[test]
        fn skips_unknown_fragment_fields() {
            let operation_fragments = r#"
query TestQuery {
  t {
    ...FragOnT
  }
}

fragment FragOnT on T {
  v0
  v1
  v2
  u1 {
    v3
    v4
    v5
  }
  u2 {
    v4
    v5
  }
}

type Query {
  t: T
}

type T {
  v0: Int
  v1: Int
  v2: Int
  u1: U
  u2: U
}

type U {
  v3: Int
  v4: Int
  v5: Int
}
"#;
            let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
            assert!(
                !executable_document.fragments.is_empty(),
                "operation should have some fragments"
            );

            if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
                let normalized_operation = normalize_operation(
                    operation,
                    NamedFragments::new(&executable_document.fragments, &schema),
                    &schema,
                    &IndexSet::new(),
                )
                .unwrap();

                let subgraph_schema = r#"type Query {
  _: Int
}

type T {
  v1: Int
  u1: U
}

type U {
  v3: Int
  v5: Int
}"#;
                let subgraph = parse_subgraph("A", subgraph_schema);
                let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
                assert!(rebased_fragments.is_ok());
                let rebased_fragments = rebased_fragments.unwrap();
                assert!(!rebased_fragments.is_empty());
                assert!(rebased_fragments.contains(&name!("FragOnT")));
                let rebased_fragment = rebased_fragments.fragments.get("FragOnT").unwrap();

                insta::assert_snapshot!(rebased_fragment, @r###"
                    fragment FragOnT on T {
                      v1
                      u1 {
                        v3
                        v5
                      }
                    }
                "###);
            }
        }

        #[test]
        fn skips_unknown_fragment_on_condition() {
            let operation_fragments = r#"
query TestQuery {
  t {
    ...FragOnT
  }
  u {
    ...FragOnU
  }
}

fragment FragOnT on T {
  x
  y
}

fragment FragOnU on U {
  x
  y
}

type Query {
  t: T
  u: U
}

type T {
  x: Int
  y: Int
}

type U {
  x: Int
  y: Int
}
"#;
            let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
            assert!(
                !executable_document.fragments.is_empty(),
                "operation should have some fragments"
            );
            assert_eq!(2, executable_document.fragments.len());

            if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
                let normalized_operation = normalize_operation(
                    operation,
                    NamedFragments::new(&executable_document.fragments, &schema),
                    &schema,
                    &IndexSet::new(),
                )
                .unwrap();

                let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
  y: Int
}"#;
                let subgraph = parse_subgraph("A", subgraph_schema);
                let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
                assert!(rebased_fragments.is_ok());
                let rebased_fragments = rebased_fragments.unwrap();
                assert!(!rebased_fragments.is_empty());
                assert!(rebased_fragments.contains(&name!("FragOnT")));
                assert!(!rebased_fragments.contains(&name!("FragOnU")));
                let rebased_fragment = rebased_fragments.fragments.get("FragOnT").unwrap();

                let expected = r#"fragment FragOnT on T {
  x
  y
}"#;
                let actual = rebased_fragment.to_string();
                assert_eq!(actual, expected);
            }
        }

        #[test]
        fn skips_unknown_type_within_fragment() {
            let operation_fragments = r#"
query TestQuery {
  i {
    ...FragOnI
  }
}

fragment FragOnI on I {
  id
  otherId
  ... on T1 {
    x
  }
  ... on T2 {
    y
  }
}

type Query {
  i: I
}

interface I {
  id: ID!
  otherId: ID!
}

type T1 implements I {
  id: ID!
  otherId: ID!
  x: Int
}

type T2 implements I {
  id: ID!
  otherId: ID!
  y: Int
}
"#;
            let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
            assert!(
                !executable_document.fragments.is_empty(),
                "operation should have some fragments"
            );

            if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
                let normalized_operation = normalize_operation(
                    operation,
                    NamedFragments::new(&executable_document.fragments, &schema),
                    &schema,
                    &IndexSet::new(),
                )
                .unwrap();

                let subgraph_schema = r#"type Query {
  i: I
}

interface I {
  id: ID!
}

type T2 implements I {
  id: ID!
  y: Int
}
"#;
                let subgraph = parse_subgraph("A", subgraph_schema);
                let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
                assert!(rebased_fragments.is_ok());
                let rebased_fragments = rebased_fragments.unwrap();
                assert!(!rebased_fragments.is_empty());
                assert!(rebased_fragments.contains(&name!("FragOnI")));
                let rebased_fragment = rebased_fragments.fragments.get("FragOnI").unwrap();

                let expected = r#"fragment FragOnI on I {
  id
  ... on T2 {
    y
  }
}"#;
                let actual = rebased_fragment.to_string();
                assert_eq!(actual, expected);
            }
        }

        #[test]
        fn skips_typename_on_possible_interface_objects_within_fragment() {
            let operation_fragments = r#"
query TestQuery {
  i {
    ...FragOnI
  }
}

fragment FragOnI on I {
  __typename
  id
  x
}

type Query {
  i: I
}

interface I {
  id: ID!
  x: String!
}

type T implements I {
  id: ID!
  x: String!
}
"#;

            let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
            assert!(
                !executable_document.fragments.is_empty(),
                "operation should have some fragments"
            );

            if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
                let mut interface_objects: IndexSet<InterfaceTypeDefinitionPosition> =
                    IndexSet::new();
                interface_objects.insert(InterfaceTypeDefinitionPosition {
                    type_name: name!("I"),
                });
                let normalized_operation = normalize_operation(
                    operation,
                    NamedFragments::new(&executable_document.fragments, &schema),
                    &schema,
                    &interface_objects,
                )
                .unwrap();

                let subgraph_schema = r#"extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.5", import: [{ name: "@interfaceObject" }, { name: "@key" }])

directive @link(url: String, as: String, import: [link__Import]) repeatable on SCHEMA

directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

directive @interfaceObject on OBJECT

type Query {
  i: I
}

type I @interfaceObject @key(fields: "id") {
  id: ID!
  x: String!
}

scalar link__Import

scalar federation__FieldSet
"#;
                let subgraph = parse_subgraph("A", subgraph_schema);
                let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
                assert!(rebased_fragments.is_ok());
                let rebased_fragments = rebased_fragments.unwrap();
                assert!(!rebased_fragments.is_empty());
                assert!(rebased_fragments.contains(&name!("FragOnI")));
                let rebased_fragment = rebased_fragments.fragments.get("FragOnI").unwrap();

                let expected = r#"fragment FragOnI on I {
  id
  x
}"#;
                let actual = rebased_fragment.to_string();
                assert_eq!(actual, expected);
            }
        }

        #[test]
        fn skips_fragments_with_trivial_selections() {
            let operation_fragments = r#"
query TestQuery {
  t {
    ...F1
    ...F2
    ...F3
  }
}

fragment F1 on T {
  a
  b
}

fragment F2 on T {
  __typename
  a
  b
}

fragment F3 on T {
  __typename
  a
  b
  c
  d
}

type Query {
  t: T
}

type T {
  a: Int
  b: Int
  c: Int
  d: Int
}
"#;
            let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
            assert!(
                !executable_document.fragments.is_empty(),
                "operation should have some fragments"
            );

            if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
                let normalized_operation = normalize_operation(
                    operation,
                    NamedFragments::new(&executable_document.fragments, &schema),
                    &schema,
                    &IndexSet::new(),
                )
                .unwrap();

                let subgraph_schema = r#"type Query {
  t: T
}

type T {
  c: Int
  d: Int
}
"#;
                let subgraph = parse_subgraph("A", subgraph_schema);
                let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
                assert!(rebased_fragments.is_ok());
                let rebased_fragments = rebased_fragments.unwrap();
                // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
                assert_eq!(1, rebased_fragments.size());
                assert!(rebased_fragments.contains(&name!("F3")));
                let rebased_fragment = rebased_fragments.fragments.get("F3").unwrap();

                let expected = r#"fragment F3 on T {
  __typename
  c
  d
}"#;
                let actual = rebased_fragment.to_string();
                assert_eq!(actual, expected);
            }
        }

        #[test]
        fn handles_skipped_fragments_within_fragments() {
            let operation_fragments = r#"
query TestQuery {
  ...TheQuery
}

fragment TheQuery on Query {
  t {
    x
    ... GetU
  }
}

fragment GetU on T {
  u {
    y
    z
  }
}

type Query {
  t: T
}

type T {
  x: Int
  u: U
}

type U {
  y: Int
  z: Int
}
"#;
            let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
            assert!(
                !executable_document.fragments.is_empty(),
                "operation should have some fragments"
            );

            if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
                let normalized_operation = normalize_operation(
                    operation,
                    NamedFragments::new(&executable_document.fragments, &schema),
                    &schema,
                    &IndexSet::new(),
                )
                .unwrap();

                let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
}"#;
                let subgraph = parse_subgraph("A", subgraph_schema);
                let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
                assert!(rebased_fragments.is_ok());
                let rebased_fragments = rebased_fragments.unwrap();
                // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
                assert_eq!(1, rebased_fragments.size());
                assert!(rebased_fragments.contains(&name!("TheQuery")));
                let rebased_fragment = rebased_fragments.fragments.get("TheQuery").unwrap();

                let expected = r#"fragment TheQuery on Query {
  t {
    x
  }
}"#;
                let actual = rebased_fragment.to_string();
                assert_eq!(actual, expected);
            }
        }

        #[test]
        fn handles_subtypes_within_subgraphs() {
            let operation_fragments = r#"
query TestQuery {
  ...TQuery
}

fragment TQuery on Query {
  t {
    x
    y
    ... on T {
      z
    }
  }
}

type Query {
  t: I
}

interface I {
  x: Int
  y: Int
}

type T implements I {
  x: Int
  y: Int
  z: Int
}
"#;
            let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
            assert!(
                !executable_document.fragments.is_empty(),
                "operation should have some fragments"
            );

            if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
                let normalized_operation = normalize_operation(
                    operation,
                    NamedFragments::new(&executable_document.fragments, &schema),
                    &schema,
                    &IndexSet::new(),
                )
                .unwrap();

                let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
  y: Int
  z: Int
}
"#;

                let subgraph = parse_subgraph("A", subgraph_schema);
                let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
                assert!(rebased_fragments.is_ok());
                let rebased_fragments = rebased_fragments.unwrap();
                // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
                assert_eq!(1, rebased_fragments.size());
                assert!(rebased_fragments.contains(&name!("TQuery")));
                let rebased_fragment = rebased_fragments.fragments.get("TQuery").unwrap();

                let expected = r#"fragment TQuery on Query {
  t {
    x
    y
    z
  }
}"#;
                let actual = rebased_fragment.to_string();
                assert_eq!(actual, expected);
            }
        }
    }

    fn containment_custom(left: &str, right: &str, ignore_missing_typename: bool) -> Containment {
        let schema = apollo_compiler::Schema::parse_and_validate(
            r#"
        directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

        interface Intf {
            intfField: Int
        }
        type HasA implements Intf {
            a: Boolean
            intfField: Int
        }
        type Nested {
            a: Int
            b: Int
            c: Int
        }
        input Input {
            recur: Input
            f: Boolean
            g: Boolean
            h: Boolean
        }
        type Query {
            a: Int
            b: Int
            c: Int
            object: Nested
            intf: Intf
            arg(a: Int, b: Int, c: Int, d: Input): Int
        }
        "#,
            "schema.graphql",
        )
        .unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();
        let left = Operation::parse(schema.clone(), left, "left.graphql", None).unwrap();
        let right = Operation::parse(schema.clone(), right, "right.graphql", None).unwrap();

        left.selection_set.containment(
            &right.selection_set,
            ContainmentOptions {
                ignore_missing_typename,
            },
        )
    }

    fn containment(left: &str, right: &str) -> Containment {
        containment_custom(left, right, false)
    }

    #[test]
    fn selection_set_contains() {
        assert_eq!(containment("{ a }", "{ a }"), Containment::Equal);
        assert_eq!(containment("{ a b }", "{ b a }"), Containment::Equal);
        assert_eq!(
            containment("{ arg(a: 1) }", "{ arg(a: 2) }"),
            Containment::NotContained
        );
        assert_eq!(
            containment("{ arg(a: 1) }", "{ arg(b: 1) }"),
            Containment::NotContained
        );
        assert_eq!(
            containment("{ arg(a: 1) }", "{ arg(a: 1) }"),
            Containment::Equal
        );
        assert_eq!(
            containment("{ arg(a: 1, b: 1) }", "{ arg(b: 1 a: 1) }"),
            Containment::Equal
        );
        assert_eq!(
            containment("{ arg(a: 1) }", "{ arg(a: 1) }"),
            Containment::Equal
        );
        assert_eq!(
            containment(
                "{ arg(d: { f: true, g: true }) }",
                "{ arg(d: { f: true }) }"
            ),
            Containment::NotContained
        );
        assert_eq!(
            containment(
                "{ arg(d: { recur: { f: true } g: true h: false }) }",
                "{ arg(d: { h: false recur: {f: true} g: true }) }"
            ),
            Containment::Equal
        );
        assert_eq!(
            containment("{ arg @skip(if: true) }", "{ arg @skip(if: true) }"),
            Containment::Equal
        );
        assert_eq!(
            containment("{ arg @skip(if: true) }", "{ arg @skip(if: false) }"),
            Containment::NotContained
        );
        assert_eq!(
            containment("{ ... @defer { arg } }", "{ ... @defer { arg } }"),
            Containment::NotContained,
            "@defer selections never contain each other"
        );
        assert_eq!(
            containment("{ a b c }", "{ b a }"),
            Containment::StrictlyContained
        );
        assert_eq!(
            containment("{ a b }", "{ b c a }"),
            Containment::NotContained
        );
        assert_eq!(containment("{ a }", "{ b }"), Containment::NotContained);
        assert_eq!(
            containment("{ object { a } }", "{ object { b a } }"),
            Containment::NotContained
        );

        assert_eq!(
            containment("{ ... { a } }", "{ ... { a } }"),
            Containment::Equal
        );
        assert_eq!(
            containment(
                "{ intf { ... on HasA { a } } }",
                "{ intf { ... on HasA { a } } }",
            ),
            Containment::Equal
        );
        // These select the same things, but containment also counts fragment namedness
        assert_eq!(
            containment(
                "{ intf { ... on HasA { a } } }",
                "{ intf { ...named } } fragment named on HasA { a }",
            ),
            Containment::NotContained
        );
        assert_eq!(
            containment(
                "{ intf { ...named } } fragment named on HasA { a intfField }",
                "{ intf { ...named } } fragment named on HasA { a }",
            ),
            Containment::StrictlyContained
        );
        assert_eq!(
            containment(
                "{ intf { ...named } } fragment named on HasA { a }",
                "{ intf { ...named } } fragment named on HasA { a intfField }",
            ),
            Containment::NotContained
        );
    }

    #[test]
    fn selection_set_contains_missing_typename() {
        assert_eq!(
            containment_custom("{ a }", "{ a __typename }", true),
            Containment::Equal
        );
        assert_eq!(
            containment_custom("{ a b }", "{ b a __typename }", true),
            Containment::Equal
        );
        assert_eq!(
            containment_custom("{ a b }", "{ b __typename }", true),
            Containment::StrictlyContained
        );
        assert_eq!(
            containment_custom("{ object { a b } }", "{ object { b __typename } }", true),
            Containment::StrictlyContained
        );
        assert_eq!(
            containment_custom(
                "{ intf { intfField __typename } }",
                "{ intf { intfField } }",
                true
            ),
            Containment::StrictlyContained,
        );
        assert_eq!(
            containment_custom(
                "{ intf { intfField __typename } }",
                "{ intf { intfField __typename } }",
                true
            ),
            Containment::Equal,
        );
    }

    /// This regression-tests an assumption from
    /// https://github.com/apollographql/federation-next/pull/290#discussion_r1587200664
    #[test]
    fn converting_operation_types() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            r#"
        interface Intf {
            intfField: Int
        }
        type HasA implements Intf {
            a: Boolean
            intfField: Int
        }
        type Nested {
            a: Int
            b: Int
            c: Int
        }
        type Query {
            a: Int
            b: Int
            c: Int
            object: Nested
            intf: Intf
        }
        "#,
            "schema.graphql",
        )
        .unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();
        insta::assert_snapshot!(Operation::parse(
            schema.clone(),
            r#"
        {
            intf {
                ... on HasA { a }
                ... frag
            }
        }
        fragment frag on HasA { intfField }
        "#,
            "operation.graphql",
            None,
        )
        .unwrap(), @r###"
        fragment frag on HasA {
          intfField
        }

        {
          intf {
            ... on HasA {
              a
            }
            ...frag
          }
        }
        "###);
    }

    fn contains_field(ss: &SelectionSet, field_name: Name) -> bool {
        ss.selections.contains_key(&SelectionKey::Field {
            response_name: field_name,
            directives: Default::default(),
        })
    }

    fn is_named_field(sk: &SelectionKey, name: Name) -> bool {
        matches!(sk,
            SelectionKey::Field { response_name, directives: _ }
                if *response_name == name)
    }

    fn get_value_at_path<'a>(ss: &'a SelectionSet, path: &[Name]) -> Option<&'a Selection> {
        let Some((first, rest)) = path.split_first() else {
            // Error: empty path
            return None;
        };
        let result = ss.selections.get(&SelectionKey::Field {
            response_name: (*first).clone(),
            directives: Default::default(),
        });
        let Some(value) = result else {
            // Error: No matching field found.
            return None;
        };
        if rest.is_empty() {
            // Base case => We are done.
            Some(value)
        } else {
            // Recursive case
            match value.selection_set().unwrap() {
                None => None, // Error: Sub-selection expected, but not found.
                Some(ss) => get_value_at_path(ss, rest),
            }
        }
    }

    #[cfg(test)]
    mod make_selection_tests {
        use super::super::*;
        use super::*;

        const SAMPLE_OPERATION_DOC: &str = r#"
        type Query {
            foo: Foo!
        }

        type Foo {
            a: Int!
            b: Int!
            c: Int!
        }

        query TestQuery {
            foo {
                a
                b
                c
            }
        }
        "#;

        // Tests if `make_selection`'s subselection ordering is preserved.
        #[test]
        fn test_make_selection_order() {
            let (schema, executable_document) = parse_schema_and_operation(SAMPLE_OPERATION_DOC);
            let normalized_operation = normalize_operation(
                executable_document.get_operation(None).unwrap(),
                Default::default(),
                &schema,
                &Default::default(),
            )
            .unwrap();

            let foo = get_value_at_path(&normalized_operation.selection_set, &[name!("foo")])
                .expect("foo should exist");
            assert_eq!(foo.to_string(), "foo { a b c }");

            // Create a new foo with a different selection order using `make_selection`.
            let clone_selection_at_path = |base: &Selection, path: &[Name]| {
                let base_selection_set = base.selection_set().unwrap().unwrap();
                let selection =
                    get_value_at_path(base_selection_set, path).expect("path should exist");
                let subselections = SelectionSet::from_selection(
                    base_selection_set.type_position.clone(),
                    selection.clone(),
                );
                Selection::from_element(base.element().unwrap(), Some(subselections)).unwrap()
            };

            let foo_with_a = clone_selection_at_path(foo, &[name!("a")]);
            let foo_with_b = clone_selection_at_path(foo, &[name!("b")]);
            let foo_with_c = clone_selection_at_path(foo, &[name!("c")]);
            let new_selection = SelectionSet::make_selection(
                &schema,
                &foo.element().unwrap().parent_type_position(),
                [foo_with_c, foo_with_b, foo_with_a].iter(),
                /*named_fragments*/ &Default::default(),
            )
            .unwrap();
            // Make sure the ordering of c, b and a is preserved.
            assert_eq!(new_selection.to_string(), "foo { c b a }");
        }
    }

    #[cfg(test)]
    mod lazy_map_tests {
        use super::super::*;
        use super::*;

        // recursive filter implementation using `lazy_map`
        fn filter_rec(
            ss: &SelectionSet,
            pred: &impl Fn(&Selection) -> bool,
        ) -> Result<SelectionSet, FederationError> {
            ss.lazy_map(/*named_fragments*/ &Default::default(), |s| {
                if !pred(s) {
                    return Ok(SelectionMapperReturn::None);
                }
                match s.selection_set()? {
                    // Base case: leaf field
                    None => Ok(s.clone().into()),

                    // Recursive case: non-leaf field
                    Some(inner_ss) => {
                        let updated_ss = filter_rec(inner_ss, pred).map(Some)?;
                        // see if `updated_ss` is an non-empty selection set.
                        if matches!(updated_ss, Some(ref sub_ss) if !sub_ss.is_empty()) {
                            s.with_updated_selection_set(updated_ss).map(|ss| ss.into())
                        } else {
                            Ok(SelectionMapperReturn::None)
                        }
                    }
                }
            })
        }

        const SAMPLE_OPERATION_DOC: &str = r#"
        type Query {
            foo: Foo!
            some_int: Int!
            foo2: Foo!
        }

        type Foo {
            id: ID!
            bar: String!
            baz: Int
        }

        query TestQuery {
            foo {
                id
                bar
            },
            some_int
            foo2 {
                bar
            }
        }
        "#;

        // Tests `lazy_map` via `filter_rec` function.
        #[test]
        fn test_lazy_map() {
            let (schema, executable_document) = parse_schema_and_operation(SAMPLE_OPERATION_DOC);
            let normalized_operation = normalize_operation(
                executable_document.get_operation(None).unwrap(),
                Default::default(),
                &schema,
                &Default::default(),
            )
            .unwrap();

            let selection_set = normalized_operation.selection_set;

            // Select none
            let select_none = filter_rec(&selection_set, &|_| false).unwrap();
            assert!(select_none.is_empty());

            // Select all
            let select_all = filter_rec(&selection_set, &|_| true).unwrap();
            assert!(select_all == selection_set);

            // Remove `foo`
            let remove_foo =
                filter_rec(&selection_set, &|s| !is_named_field(&s.key(), name!("foo"))).unwrap();
            assert!(contains_field(&remove_foo, name!("some_int")));
            assert!(contains_field(&remove_foo, name!("foo2")));
            assert!(!contains_field(&remove_foo, name!("foo")));

            // Remove `bar`
            let remove_bar =
                filter_rec(&selection_set, &|s| !is_named_field(&s.key(), name!("bar"))).unwrap();
            // "foo2" should be removed, since it has no sub-selections left.
            assert!(!contains_field(&remove_bar, name!("foo2")));
        }

        fn add_typename_if(
            ss: &SelectionSet,
            pred: &impl Fn(&Selection) -> bool,
        ) -> Result<SelectionSet, FederationError> {
            ss.lazy_map(/*named_fragments*/ &Default::default(), |s| {
                let to_add_typename = pred(s);
                let updated = s.map_selection_set(|ss| add_typename_if(ss, pred).map(Some))?;
                if !to_add_typename {
                    return Ok(updated.into());
                }

                let parent_type_pos = s.element()?.parent_type_position();
                // "__typename" field
                let field_element =
                    Field::new_introspection_typename(s.schema(), &parent_type_pos, None);
                let typename_selection =
                    Selection::from_element(field_element.into(), /*subselection*/ None)?;
                // return `updated` and `typename_selection`
                Ok([updated, typename_selection].into_iter().collect())
            })
        }

        // Tests `lazy_map` via `add_typename_if` function.
        #[test]
        fn test_lazy_map2() {
            let (schema, executable_document) = parse_schema_and_operation(SAMPLE_OPERATION_DOC);
            let normalized_operation = normalize_operation(
                executable_document.get_operation(None).unwrap(),
                Default::default(),
                &schema,
                &Default::default(),
            )
            .unwrap();

            let selection_set = normalized_operation.selection_set;

            // Add __typename next to any "id" field.
            let result =
                add_typename_if(&selection_set, &|s| is_named_field(&s.key(), name!("id")))
                    .unwrap();

            // The top level won't have __typename, since it doesn't have "id".
            assert!(!contains_field(&result, name!("__typename")));

            // Check if "foo" has "__typename".
            get_value_at_path(&result, &[name!("foo"), name!("__typename")])
                .expect("foo.__typename should exist");
        }
    }

    fn field_element(
        schema: &ValidFederationSchema,
        object: apollo_compiler::schema::Name,
        field: apollo_compiler::schema::Name,
    ) -> OpPathElement {
        OpPathElement::Field(super::Field::new(super::FieldData {
            schema: schema.clone(),
            field_position: ObjectTypeDefinitionPosition::new(object)
                .field(field)
                .into(),
            alias: None,
            arguments: Default::default(),
            directives: Default::default(),
            sibling_typename: None,
        }))
    }

    const ADD_AT_PATH_TEST_SCHEMA: &str = r#"
        type A { b: B }
        type B { c: C }
        type C implements X {
            d: Int
            e(arg: Int): Int
        }
        type D implements X {
            d: Int
            e: Boolean
        }

        interface X {
            d: Int
        }
        type Query {
            a: A
            something: Boolean!
            scalar: String
            withArg(arg: Int): X
        }
    "#;

    #[test]
    fn add_at_path_merge_scalar_fields() {
        let schema =
            apollo_compiler::Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql")
                .unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();

        let mut selection_set = SelectionSet::empty(
            schema.clone(),
            ObjectTypeDefinitionPosition::new(name!("Query")).into(),
        );

        selection_set
            .add_at_path(
                &[field_element(&schema, name!("Query"), name!("scalar")).into()],
                None,
            )
            .unwrap();

        selection_set
            .add_at_path(
                &[field_element(&schema, name!("Query"), name!("scalar")).into()],
                None,
            )
            .unwrap();

        insta::assert_snapshot!(selection_set, @r#"{ scalar }"#);
    }

    #[test]
    fn add_at_path_merge_subselections() {
        let schema =
            apollo_compiler::Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql")
                .unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();

        let mut selection_set = SelectionSet::empty(
            schema.clone(),
            ObjectTypeDefinitionPosition::new(name!("Query")).into(),
        );

        let path_to_c = [
            field_element(&schema, name!("Query"), name!("a")).into(),
            field_element(&schema, name!("A"), name!("b")).into(),
            field_element(&schema, name!("B"), name!("c")).into(),
        ];

        selection_set
            .add_at_path(
                &path_to_c,
                Some(
                    &SelectionSet::parse(
                        schema.clone(),
                        ObjectTypeDefinitionPosition::new(name!("C")).into(),
                        "d",
                    )
                    .unwrap()
                    .into(),
                ),
            )
            .unwrap();
        selection_set
            .add_at_path(
                &path_to_c,
                Some(
                    &SelectionSet::parse(
                        schema.clone(),
                        ObjectTypeDefinitionPosition::new(name!("C")).into(),
                        "e(arg: 1)",
                    )
                    .unwrap()
                    .into(),
                ),
            )
            .unwrap();

        insta::assert_snapshot!(selection_set, @r#"{ a { b { c { d e(arg: 1) } } } }"#);
    }

    // TODO: `.add_at_path` should collapse unnecessary fragments
    #[test]
    #[ignore]
    fn add_at_path_collapses_unnecessary_fragments() {
        let schema =
            apollo_compiler::Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql")
                .unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();

        let mut selection_set = SelectionSet::empty(
            schema.clone(),
            ObjectTypeDefinitionPosition::new(name!("Query")).into(),
        );
        selection_set
            .add_at_path(
                &[
                    field_element(&schema, name!("Query"), name!("a")).into(),
                    field_element(&schema, name!("A"), name!("b")).into(),
                    field_element(&schema, name!("B"), name!("c")).into(),
                ],
                Some(
                    &SelectionSet::parse(
                        schema.clone(),
                        InterfaceTypeDefinitionPosition::new(name!("X")).into(),
                        "... on C { d }",
                    )
                    .unwrap()
                    .into(),
                ),
            )
            .unwrap();

        insta::assert_snapshot!(selection_set, @r#"{ a { b { c { d } } } }"#);
    }
}
