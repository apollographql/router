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
use crate::query_graph::graph_path::OpPathElement;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::FetchDataKeyRenamer;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::schema::definitions::is_composite_type;
use crate::schema::definitions::types_can_be_merged;
use crate::schema::definitions::AbstractType;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::ValidFederationSchema;

mod contains;
mod optimize;
mod rebase;
#[cfg(test)]
mod tests;

pub use contains::*;
pub use rebase::*;

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

    /// Removes the @defer directive from all selections without removing that selection.
    pub(crate) fn without_defer(mut self) -> Self {
        if self.has_defer() {
            self.selection_set.without_defer();
        }
        debug_assert!(!self.has_defer());
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

mod selection_map {
    use std::borrow::Cow;
    use std::iter::Map;
    use std::ops::Deref;
    use std::sync::Arc;

    use apollo_compiler::ast::Name;
    use apollo_compiler::executable;
    use indexmap::IndexMap;

    use crate::error::FederationError;
    use crate::error::SingleFederationError::Internal;
    use crate::operation::field_selection::FieldSelection;
    use crate::operation::fragment_spread_selection::FragmentSpreadSelection;
    use crate::operation::inline_fragment_selection::InlineFragmentSelection;
    use crate::operation::HasSelectionKey;
    use crate::operation::Selection;
    use crate::operation::SelectionKey;
    use crate::operation::SelectionSet;

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
                            Arc::new(InlineFragmentSelection::new(
                                fragment.inline_fragment.clone(),
                                selection_set,
                            )),
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

        pub(super) fn get_directives_mut(&mut self) -> &mut Arc<executable::DirectiveList> {
            match self {
                Self::Field(field) => field.get_directives_mut(),
                Self::FragmentSpread(spread) => spread.get_directives_mut(),
                Self::InlineFragment(inline) => inline.get_directives_mut(),
            }
        }

        pub(super) fn get_selection_set_mut(&mut self) -> Option<&mut SelectionSet> {
            match self {
                Self::Field(field) => field.get_selection_set_mut().as_mut(),
                Self::FragmentSpread(spread) => Some(spread.get_selection_set_mut()),
                Self::InlineFragment(inline) => Some(inline.get_selection_set_mut()),
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

        pub(super) fn get_directives_mut(&mut self) -> &mut Arc<executable::DirectiveList> {
            Arc::make_mut(self.0).field.directives_mut()
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

        pub(super) fn get_directives_mut(&mut self) -> &mut Arc<executable::DirectiveList> {
            Arc::make_mut(self.0).spread.directives_mut()
        }

        pub(crate) fn get_selection_set_mut(&mut self) -> &mut SelectionSet {
            &mut Arc::make_mut(self.0).selection_set
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

        pub(super) fn get_directives_mut(&mut self) -> &mut Arc<executable::DirectiveList> {
            Arc::make_mut(self.0).inline_fragment.directives_mut()
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

pub(crate) use selection_map::FieldSelectionValue;
pub(crate) use selection_map::FragmentSpreadSelectionValue;
pub(crate) use selection_map::InlineFragmentSelectionValue;
pub(crate) use selection_map::SelectionMap;
pub(crate) use selection_map::SelectionValue;

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

/// An analogue of the apollo-compiler type `Selection` that stores our other selection analogues
/// instead of the apollo-compiler types.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::IsVariant)]
pub(crate) enum Selection {
    Field(Arc<FieldSelection>),
    FragmentSpread(Arc<FragmentSpreadSelection>),
    InlineFragment(Arc<InlineFragmentSelection>),
}

/// Element enum that is more general than OpPathElement.
/// - Used for operation optimization.
#[derive(Debug, Clone, derive_more::From)]
pub(crate) enum OperationElement {
    Field(Field),
    FragmentSpread(FragmentSpread),
    InlineFragment(InlineFragment),
}

impl Selection {
    pub(crate) fn from_field(field: Field, sub_selections: Option<SelectionSet>) -> Self {
        Self::Field(Arc::new(field.with_subselection(sub_selections)))
    }

    /// Build a selection from an OpPathElement and a sub-selection set.
    pub(crate) fn from_element(
        element: OpPathElement,
        sub_selections: Option<SelectionSet>,
    ) -> Result<Selection, FederationError> {
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
                Ok(InlineFragmentSelection::new(inline_fragment, sub_selections).into())
            }
        }
    }

    /// Build a selection from an OperationElement and a sub-selection set.
    /// - `named_fragments`: Named fragment definitions that are rebased for the element's schema.
    pub(crate) fn from_operation_element(
        element: OperationElement,
        sub_selections: Option<SelectionSet>,
        named_fragments: &NamedFragments,
    ) -> Result<Selection, FederationError> {
        match element {
            OperationElement::Field(field) => Ok(Self::from_field(field, sub_selections)),
            OperationElement::FragmentSpread(fragment_spread) => {
                if sub_selections.is_some() {
                    return Err(FederationError::internal(
                        "unexpected fragment spread with sub-selections",
                    ));
                }
                Ok(FragmentSpreadSelection::new(fragment_spread, named_fragments)?.into())
            }
            OperationElement::InlineFragment(inline_fragment) => {
                let Some(sub_selections) = sub_selections else {
                    return Err(FederationError::internal(
                        "unexpected inline fragment without sub-selections",
                    ));
                };
                Ok(InlineFragmentSelection::new(inline_fragment, sub_selections).into())
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

    pub(crate) fn operation_element(&self) -> Result<OperationElement, FederationError> {
        match self {
            Selection::Field(field_selection) => {
                Ok(OperationElement::Field(field_selection.field.clone()))
            }
            Selection::FragmentSpread(fragment_spread_selection) => Ok(
                OperationElement::FragmentSpread(fragment_spread_selection.spread.clone()),
            ),
            Selection::InlineFragment(inline_fragment_selection) => Ok(
                OperationElement::InlineFragment(inline_fragment_selection.inline_fragment.clone()),
            ),
        }
    }

    // Note: Fragment spreads can be present in optimized operations.
    pub(crate) fn selection_set(&self) -> Result<Option<&SelectionSet>, FederationError> {
        match self {
            Selection::Field(field_selection) => Ok(field_selection.selection_set.as_ref()),
            Selection::FragmentSpread(_) => Ok(None),
            Selection::InlineFragment(inline_fragment_selection) => {
                Ok(Some(&inline_fragment_selection.selection_set))
            }
        }
    }

    pub(crate) fn try_selection_set(&self) -> Option<&SelectionSet> {
        match self {
            Selection::Field(field_selection) => field_selection.selection_set.as_ref(),
            Selection::FragmentSpread(_) => None,
            Selection::InlineFragment(inline_fragment_selection) => {
                Some(&inline_fragment_selection.selection_set)
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

    fn normalize(
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

    pub(crate) fn any_element(
        &self,
        parent_type_position: CompositeTypeDefinitionPosition,
        predicate: &mut impl FnMut(OpPathElement) -> Result<bool, FederationError>,
    ) -> Result<bool, FederationError> {
        match self {
            Selection::Field(field_selection) => field_selection.any_element(predicate),
            Selection::InlineFragment(inline_fragment_selection) => {
                inline_fragment_selection.any_element(predicate)
            }
            Selection::FragmentSpread(fragment_spread_selection) => {
                fragment_spread_selection.any_element(parent_type_position, predicate)
            }
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

mod field_selection {
    use std::collections::HashSet;
    use std::hash::Hash;
    use std::hash::Hasher;
    use std::sync::Arc;

    use apollo_compiler::ast;
    use apollo_compiler::executable;
    use apollo_compiler::executable::Name;
    use apollo_compiler::Node;

    use crate::error::FederationError;
    use crate::operation::sort_arguments;
    use crate::operation::sort_directives;
    use crate::operation::HasSelectionKey;
    use crate::operation::SelectionKey;
    use crate::operation::SelectionSet;
    use crate::query_graph::graph_path::OpPathElement;
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
    #[derive(Clone)]
    pub(crate) struct Field {
        data: FieldData,
        key: SelectionKey,
        sorted_arguments: Arc<Vec<Node<executable::Argument>>>,
    }

    impl std::fmt::Debug for Field {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.data.fmt(f)
        }
    }

    impl PartialEq for Field {
        fn eq(&self, other: &Self) -> bool {
            self.data.field_position.field_name() == other.data.field_position.field_name()
                && self.key == other.key
                && self.sorted_arguments == other.sorted_arguments
        }
    }

    impl Eq for Field {}

    impl Hash for Field {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.data.field_position.field_name().hash(state);
            self.key.hash(state);
            self.sorted_arguments.hash(state);
        }
    }

    impl Field {
        pub(crate) fn new(data: FieldData) -> Self {
            let mut arguments = data.arguments.as_ref().clone();
            sort_arguments(&mut arguments);
            Self {
                key: data.key(),
                sorted_arguments: Arc::new(arguments),
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
            if cfg!(debug_assertions) {
                if let Some(ref selection_set) = selection_set {
                    if let Ok(field_type) = self.data.output_base_type() {
                        if let Ok(field_type_position) =
                            CompositeTypeDefinitionPosition::try_from(field_type)
                        {
                            debug_assert_eq!(
                                field_type_position,
                                selection_set.type_position,
                                "Field and its selection set should point to the same type position [field position: {}, selection position: {}]", field_type_position, selection_set.type_position,
                            );
                            debug_assert_eq!(
                                self.data().schema,
                                selection_set.schema,
                                "Field and its selection set should point to the same schema",
                            );
                        } else {
                            debug_assert!(
                                false,
                                "Field with subselection does not reference CompositeTypePosition"
                            );
                        }
                    } else {
                        debug_assert!(
                            false,
                            "Field with subselection does not reference CompositeTypePosition"
                        );
                    }
                }
            }

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

        pub(super) fn directives_mut(&mut self) -> &mut Arc<executable::DirectiveList> {
            &mut self.data.directives
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

    #[derive(Debug, Clone)]
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
            let mut directives = self.directives.as_ref().clone();
            sort_directives(&mut directives);
            SelectionKey::Field {
                response_name: self.response_name(),
                directives: Arc::new(directives),
            }
        }
    }
}

pub(crate) use field_selection::Field;
pub(crate) use field_selection::FieldData;
pub(crate) use field_selection::FieldSelection;

mod fragment_spread_selection {
    use std::sync::Arc;

    use apollo_compiler::executable;
    use apollo_compiler::executable::Name;

    use crate::operation::is_deferred_selection;
    use crate::operation::sort_directives;
    use crate::operation::HasSelectionKey;
    use crate::operation::SelectionId;
    use crate::operation::SelectionKey;
    use crate::operation::SelectionSet;
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
    #[derive(Clone)]
    pub(crate) struct FragmentSpread {
        data: FragmentSpreadData,
        key: SelectionKey,
    }

    impl std::fmt::Debug for FragmentSpread {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.data.fmt(f)
        }
    }

    impl PartialEq for FragmentSpread {
        fn eq(&self, other: &Self) -> bool {
            self.key == other.key
        }
    }

    impl Eq for FragmentSpread {}

    impl FragmentSpread {
        pub(crate) fn new(data: FragmentSpreadData) -> Self {
            Self {
                key: data.key(),
                data,
            }
        }

        pub(super) fn directives_mut(&mut self) -> &mut Arc<executable::DirectiveList> {
            &mut self.data.directives
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

    #[derive(Debug, Clone)]
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
                let mut directives = self.directives.as_ref().clone();
                sort_directives(&mut directives);
                SelectionKey::FragmentSpread {
                    fragment_name: self.fragment_name.clone(),
                    directives: Arc::new(directives),
                }
            }
        }
    }
}

pub(crate) use fragment_spread_selection::FragmentSpread;
pub(crate) use fragment_spread_selection::FragmentSpreadData;
pub(crate) use fragment_spread_selection::FragmentSpreadSelection;

impl FragmentSpreadSelection {
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

    /// Creates a fragment spread selection (in an optimized operation).
    /// - `named_fragments`: Named fragment definitions that are rebased for the element's schema.
    pub(crate) fn new(
        fragment_spread: FragmentSpread,
        named_fragments: &NamedFragments,
    ) -> Result<Self, FederationError> {
        let fragment_name = &fragment_spread.data().fragment_name;
        let fragment = named_fragments.get(fragment_name).ok_or_else(|| {
            FederationError::internal(format!("Fragment {} not found", fragment_name))
        })?;
        debug_assert_eq!(fragment_spread.data().schema, fragment.schema);
        Ok(Self {
            spread: fragment_spread,
            selection_set: fragment.selection_set.clone(),
        })
    }

    fn normalize(
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

    pub(crate) fn any_element(
        &self,
        parent_type_position: CompositeTypeDefinitionPosition,
        predicate: &mut impl FnMut(OpPathElement) -> Result<bool, FederationError>,
    ) -> Result<bool, FederationError> {
        let inline_fragment = InlineFragment::new(InlineFragmentData {
            schema: self.spread.data().schema.clone(),
            parent_type_position,
            type_condition_position: Some(self.spread.data().type_condition_position.clone()),
            directives: self.spread.data().directives.clone(),
            selection_id: self.spread.data().selection_id.clone(),
        });
        if predicate(inline_fragment.into())? {
            return Ok(true);
        }
        self.selection_set.any_element(predicate)
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

mod inline_fragment_selection {
    use std::collections::HashSet;
    use std::hash::Hash;
    use std::hash::Hasher;
    use std::sync::Arc;

    use apollo_compiler::executable;
    use apollo_compiler::executable::Name;

    use super::field_selection::collect_variables_from_directive;
    use crate::error::FederationError;
    use crate::link::graphql_definition::defer_directive_arguments;
    use crate::link::graphql_definition::DeferDirectiveArguments;
    use crate::operation::is_deferred_selection;
    use crate::operation::sort_directives;
    use crate::operation::HasSelectionKey;
    use crate::operation::SelectionId;
    use crate::operation::SelectionKey;
    use crate::operation::SelectionSet;
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
    #[derive(Clone)]
    pub(crate) struct InlineFragment {
        data: InlineFragmentData,
        key: SelectionKey,
    }

    impl std::fmt::Debug for InlineFragment {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.data.fmt(f)
        }
    }

    impl PartialEq for InlineFragment {
        fn eq(&self, other: &Self) -> bool {
            self.key == other.key
        }
    }

    impl Eq for InlineFragment {}

    impl Hash for InlineFragment {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.key.hash(state);
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

        pub(super) fn directives_mut(&mut self) -> &mut Arc<executable::DirectiveList> {
            &mut self.data.directives
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

    #[derive(Debug, Clone)]
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

        pub(crate) fn casted_type(&self) -> CompositeTypeDefinitionPosition {
            self.type_condition_position
                .clone()
                .unwrap_or_else(|| self.parent_type_position.clone())
        }
    }

    impl HasSelectionKey for InlineFragmentData {
        fn key(&self) -> SelectionKey {
            if is_deferred_selection(&self.directives) {
                SelectionKey::Defer {
                    deferred_id: self.selection_id.clone(),
                }
            } else {
                let mut directives = self.directives.as_ref().clone();
                sort_directives(&mut directives);
                SelectionKey::InlineFragment {
                    type_condition: self
                        .type_condition_position
                        .as_ref()
                        .map(|pos| pos.type_name().clone()),
                    directives: Arc::new(directives),
                }
            }
        }
    }
}

pub(crate) use inline_fragment_selection::InlineFragment;
pub(crate) use inline_fragment_selection::InlineFragmentData;
pub(crate) use inline_fragment_selection::InlineFragmentSelection;

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

    /// NOTE: This is a private API and should be used with care, use `add_selection_set` instead.
    ///
    /// Merges the given normalized selection sets into this one.
    fn merge_into<'op>(
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

    /// NOTE: This is a private API and should be used with care, use `add_selection` instead.
    ///
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
                selection_map::Entry::Occupied(existing) => match existing.get() {
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
                selection_map::Entry::Vacant(vacant) => {
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
                    destination.push(
                        InlineFragmentSelection::new(
                            inline_selection.inline_fragment.clone(),
                            inline_selection.selection_set.expand_all_fragments()?,
                        )
                        .into(),
                    );
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

        let element =
            first
                .operation_element()?
                .rebase_on_or_error(parent_type, schema, named_fragments)?;
        let sub_selection_parent_type: Option<CompositeTypeDefinitionPosition> =
            element.sub_selection_type_position()?;

        let Some(ref sub_selection_parent_type) = sub_selection_parent_type else {
            // This is a leaf, so all updates should correspond ot the same field and we just use the first.
            return Selection::from_operation_element(
                element,
                /*sub_selection*/ None,
                named_fragments,
            );
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
        Selection::from_operation_element(element, updated_sub_selection, named_fragments)
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
                let type_if_abstract = subselection_type_if_abstract(selection)?;
                let updated_selection_set =
                    selection_set.add_typename_field_for_abstract_types(type_if_abstract)?;

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
    ///
    /// NOTE: This method assumes selection already points to the correct schema and parent type.
    pub(crate) fn add_local_selection(
        &mut self,
        selection: &Selection,
    ) -> Result<(), FederationError> {
        debug_assert_eq!(
            &self.schema,
            selection.schema(),
            "In order to add selection it needs to point to the same schema"
        );
        self.merge_selections_into(std::iter::once(selection))
    }

    /// Inserts a `SelectionSet` into the inner map. Should any sub selection with the same key already
    /// exist in the map, the existing selection and the given selection are merged, replacing the
    /// existing selection while keeping the same insertion index.
    ///
    /// NOTE: This method assumes the target selection set already points to the same schema and type
    /// position. Use `add_selection_set` instead if you need to rebase the selection set.
    pub(crate) fn add_local_selection_set(
        &mut self,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        debug_assert_eq!(
            self.schema, selection_set.schema,
            "In order to add selection set it needs to point to the same schema."
        );
        debug_assert_eq!(
            self.type_position, selection_set.type_position,
            "In order to add selection set it needs to point to the same type position"
        );
        self.merge_into(std::iter::once(selection_set))
    }

    /// Rebase given `SelectionSet` on self and then inserts it into the inner map. Should any sub
    /// selection with the same key already exist in the map, the existing selection and the given
    /// selection are merged, replacing the existing selection while keeping the same insertion index.
    pub(crate) fn add_selection_set(
        &mut self,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        let rebased = selection_set.rebase_on(
            &self.type_position,
            &NamedFragments::default(),
            &self.schema,
            RebaseErrorHandlingOption::ThrowError,
        )?;
        self.add_local_selection_set(&rebased)
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
    // Notes on NamedFragments argument: `add_at_path` only deals with expanded operations, so
    // the NamedFragments argument to `rebase_on` is not needed (passing the default value).
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
                let element = ele.rebase_on_or_error(&self.type_position, &self.schema)?;
                let Some(sub_selection_type) = element.sub_selection_type_position()? else {
                    return Err(FederationError::internal("unexpected error: add_at_path encountered a field that is not of a composite type".to_string()));
                };
                let mut selection = Arc::make_mut(&mut self.selections)
                    .entry(ele.key())
                    .or_insert(|| {
                        Selection::from_element(
                            element,
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
                // in-place, we eagerly construct the selection that needs to be rebased on the target
                // schema.
                let element = ele.rebase_on_or_error(&self.type_position, &self.schema)?;
                if selection_set.is_none() || selection_set.is_some_and(|s| s.is_empty()) {
                    // This is a somewhat common case when dealing with `@key` "conditions" that we can
                    // end up with trying to add empty sub selection set on a non-leaf node. There is
                    // nothing to do here - we know will have a node at specified path but currently
                    // we don't have any sub selections so there is nothing to merge.
                    // JS code was doing this check in `makeSelectionSet`
                    if !ele.is_terminal()? {
                        return Ok(());
                    } else {
                        // add leaf
                        let selection = Selection::from_element(element, None)?;
                        self.add_local_selection(&selection)?
                    }
                } else {
                    let selection_set = selection_set
                        .map(|selection_set| {
                            selection_set.rebase_on(
                                &element.sub_selection_type_position()?.ok_or_else(|| {
                                    FederationError::internal("unexpected: Element has a selection set with non-composite base type")
                                })?,
                                &NamedFragments::default(),
                                &self.schema,
                                RebaseErrorHandlingOption::ThrowError,
                            )
                        })
                        .transpose()?
                        .map(|selection_set| selection_set.without_unnecessary_fragments());
                    let selection = Selection::from_element(element, selection_set)?;
                    self.add_local_selection(&selection)?
                }
            }
            // If we don't have any path, we rebase and merge in the given sub selections at the root.
            None => {
                if let Some(sel) = selection_set {
                    self.add_selection_set(sel)?
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
    // PORT_NOTE: this is now module-private, because it looks like it *can* be. If some place
    // outside this module *does* need it, feel free to mark it pub(crate).
    fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        option: NormalizeSelectionOption,
    ) -> Result<SelectionSet, FederationError> {
        let mut normalized_selections = Self {
            schema: schema.clone(),
            type_position: parent_type.clone(),
            selections: Default::default(), // start empty
        };
        for (_, selection) in self.selections.iter() {
            if let Some(selection_or_set) =
                selection.normalize(parent_type, named_fragments, schema, option)?
            {
                match selection_or_set {
                    SelectionOrSet::Selection(normalized_selection) => {
                        normalized_selections.add_local_selection(&normalized_selection)?;
                    }
                    SelectionOrSet::SelectionSet(normalized_set) => {
                        // Since the `selection` has been expanded/lifted, we use
                        // `add_selection_set` to make sure it's rebased.
                        normalized_selections.add_selection_set(&normalized_set)?;
                    }
                }
            }
        }
        Ok(normalized_selections)
    }

    /// Removes the @defer directive from all selections without removing that selection.
    fn without_defer(&mut self) {
        // TODO: This doesn't seem like the correct way to get the directive name...
        let Some(defer_name) = self.schema.get_directive_definition(&name!("defer")) else {
            // TODO: Return an error? Continue? Dunno...
            return;
        };
        for (_key, mut selection) in Arc::make_mut(&mut self.selections).iter_mut() {
            Arc::make_mut(selection.get_directives_mut())
                .retain(|dir| dir.name != defer_name.directive_name);
            if let Some(set) = selection.get_selection_set_mut() {
                set.without_defer();
            }
        }
        debug_assert!(!self.has_defer());
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

    /// JS PORT NOTE: In Rust implementation we are doing the selection set updates in-place whereas
    /// JS code was pooling the updates and only apply those when building the final selection set.
    /// See `makeSelectionSet` method for details.
    ///
    /// Manipulating selection sets may result in some inefficiencies. As a result we may end up with
    /// some unnecessary top level inline fragment selections, i.e. fragments without any directives
    /// and with the type condition same as the parent type that should be inlined.
    ///
    /// This method inlines those unnecessary top level fragments only. While the JS code was applying
    /// this logic recursively, since we are manipulating selections sets in-place we only need to
    /// apply this normalization at the top level.
    fn without_unnecessary_fragments(&self) -> SelectionSet {
        let parent_type = &self.type_position;
        let mut final_selections = SelectionMap::new();
        for selection in self.selections.values() {
            match selection {
                Selection::InlineFragment(inline_fragment) => {
                    if inline_fragment.is_unnecessary(parent_type) {
                        final_selections.extend_ref(&inline_fragment.selection_set.selections);
                    } else {
                        final_selections.insert(selection.clone());
                    }
                }
                _ => {
                    final_selections.insert(selection.clone());
                }
            }
        }
        SelectionSet {
            schema: self.schema.clone(),
            type_position: parent_type.clone(),
            selections: Arc::new(final_selections),
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Selection> {
        self.selections.values()
    }

    /// Returns true if any elements in this selection set or its descendants returns true for the
    /// given predicate. Note that fragment spread selections are converted to inline fragment
    /// elements, and their fragment selection sets are recursed into.
    // PORT_NOTE: The JS codebase calls this "some()", but that's easy to confuse with "Some" in
    // Rust.
    pub(crate) fn any_element(
        &self,
        predicate: &mut impl FnMut(OpPathElement) -> Result<bool, FederationError>,
    ) -> Result<bool, FederationError> {
        for selection in self.selections.values() {
            if selection.any_element(self.type_position.clone(), predicate)? {
                return Ok(true);
            }
        }
        Ok(false)
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
        let field_schema = field.field.schema().schema();
        let field_data = field.field.data();
        let field_name = field_data.name();
        let response_name = field_data.response_name();
        let field_type = &field_data.field_position.get(field_schema)?.ty;

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
) -> Result<Option<AbstractType>, FederationError> {
    let Some(sub_selection_type) = selection.element()?.sub_selection_type_position()? else {
        return Ok(None);
    };
    match sub_selection_type {
        CompositeTypeDefinitionPosition::Interface(interface_type) => {
            Ok(Some(interface_type.into()))
        }
        CompositeTypeDefinitionPosition::Union(union_type) => Ok(Some(union_type.into())),
        CompositeTypeDefinitionPosition::Object(_) => Ok(None),
    }
}

impl FieldData {
    fn with_updated_position(
        &self,
        schema: ValidFederationSchema,
        field_position: FieldDefinitionPosition,
    ) -> Self {
        Self {
            schema,
            field_position,
            ..self.clone()
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

    fn with_updated_element(&self, element: FieldData) -> Self {
        Self {
            field: Field::new(element),
            ..self.clone()
        }
    }

    fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        option: NormalizeSelectionOption,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        let field_position =
            if self.field.schema() == schema && self.field.parent_type_position() == *parent_type {
                self.field.data().field_position.clone()
            } else {
                parent_type.field(self.field.data().name().clone())?
            };

        let field_element = if self.field.schema() == schema
            && self.field.data().field_position == field_position
        {
            self.field.data().clone()
        } else {
            self.field
                .data()
                .with_updated_position(schema.clone(), field_position)
        };

        if let Some(selection_set) = &self.selection_set {
            let field_composite_type_position: CompositeTypeDefinitionPosition =
                field_element.output_base_type()?.try_into()?;
            let mut normalized_selection: SelectionSet =
                if NormalizeSelectionOption::NormalizeRecursively == option {
                    selection_set.normalize(
                        &field_composite_type_position,
                        named_fragments,
                        schema,
                        option,
                    )?
                } else {
                    selection_set.clone()
                };

            let mut selection = self.with_updated_element(field_element);
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
                        field_position: field_composite_type_position
                            .introspection_typename_field(),
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
                self.with_updated_element(field_element),
            ))))
        }
    }

    pub(crate) fn has_defer(&self) -> bool {
        self.field.has_defer() || self.selection_set.as_ref().is_some_and(|s| s.has_defer())
    }

    pub(crate) fn any_element(
        &self,
        predicate: &mut impl FnMut(OpPathElement) -> Result<bool, FederationError>,
    ) -> Result<bool, FederationError> {
        if predicate(self.field.clone().into())? {
            return Ok(true);
        }
        if let Some(selection_set) = &self.selection_set {
            if selection_set.any_element(predicate)? {
                return Ok(true);
            }
        }
        Ok(false)
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
    pub(crate) fn has_defer(&self) -> bool {
        // @defer cannot be on field at the moment
        false
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
    pub(crate) fn new(inline_fragment: InlineFragment, selection_set: SelectionSet) -> Self {
        debug_assert_eq!(
            inline_fragment.data().casted_type(),
            selection_set.type_position,
            "Inline fragment type condition and its selection set should point to the same type position",
        );
        debug_assert_eq!(
            inline_fragment.data().schema,
            selection_set.schema,
            "Inline fragment and its selection set should point to the same schema",
        );
        Self {
            inline_fragment,
            selection_set,
        }
    }

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
        let new_selection_set =
            SelectionSet::from_selection_set(&inline_fragment.selection_set, fragments, schema)?;
        let new_inline_fragment = InlineFragment::new(InlineFragmentData {
            schema: schema.clone(),
            parent_type_position: parent_type_position.clone(),
            type_condition_position,
            directives: Arc::new(inline_fragment.directives.clone()),
            selection_id: SelectionId::new(),
        });
        Ok(InlineFragmentSelection::new(
            new_inline_fragment,
            new_selection_set,
        ))
    }

    pub(crate) fn from_fragment_spread_selection(
        parent_type_position: CompositeTypeDefinitionPosition,
        fragment_spread_selection: &Arc<FragmentSpreadSelection>,
    ) -> Result<InlineFragmentSelection, FederationError> {
        let fragment_spread_data = fragment_spread_selection.spread.data();
        // Note: We assume that fragment_spread_data.type_condition_position is the same as
        //       fragment_spread_selection.selection_set.type_position.
        Ok(InlineFragmentSelection::new(
            InlineFragment::new(InlineFragmentData {
                schema: fragment_spread_data.schema.clone(),
                parent_type_position,
                type_condition_position: Some(fragment_spread_data.type_condition_position.clone()),
                directives: fragment_spread_data.directives.clone(),
                selection_id: SelectionId::new(),
            }),
            fragment_spread_selection
                .selection_set
                .expand_all_fragments()?,
        ))
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
        InlineFragmentSelection::new(InlineFragment::new(inline_fragment_data), selection_set)
    }

    fn normalize(
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
                // Try to skip this fragment and normalize self.selection_set with `parent_type`,
                // instead of its original type.
                let normalized_selection_set =
                    self.selection_set
                        .normalize(parent_type, named_fragments, schema, option)?;
                return if normalized_selection_set.is_empty() {
                    Ok(None)
                } else {
                    // We need to rebase since the parent type for the selection set could be
                    // changed.
                    // Note: Rebasing after normalization, since rebasing before that can error out.
                    //       Or, `normalize` could `rebase` at the same time.
                    let normalized_selection_set = if useless_fragment {
                        normalized_selection_set.clone()
                    } else {
                        normalized_selection_set.rebase_on(
                            parent_type,
                            named_fragments,
                            schema,
                            RebaseErrorHandlingOption::ThrowError,
                        )?
                    };
                    Ok(Some(SelectionOrSet::SelectionSet(normalized_selection_set)))
                };
            }
        }

        // We preserve the current fragment, so we only recurse within the sub-selection if we're asked to be recursive.
        // (note that even if we're not recursive, we may still have some "lifting" to do)
        // Note: This normalized_selection_set is not rebased here yet. It will be rebased later as necessary.
        let normalized_selection_set = if NormalizeSelectionOption::NormalizeRecursively == option {
            let normalized = self.selection_set.normalize(
                &self.selection_set.type_position,
                named_fragments,
                &self.selection_set.schema,
                option,
            )?;
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

                    // Return `... [on <rebased condition>] { __typename @include(if: false) }`
                    let rebased_casted_type = rebased_fragment.data().casted_type();
                    return Ok(Some(SelectionOrSet::Selection(
                        InlineFragmentSelection::new(
                            rebased_fragment,
                            SelectionSet::from_selection(
                                rebased_casted_type,
                                typename_field_selection,
                            ),
                        )
                        .into(),
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
                // Rebasing is necessary since this normalized sub-selection set changed its parent.
                let rebased_selection_set = normalized_selection_set.rebase_on(
                    parent_type,
                    named_fragments,
                    schema,
                    RebaseErrorHandlingOption::ThrowError,
                )?;
                return Ok(Some(SelectionOrSet::SelectionSet(rebased_selection_set)));
            }

            // Otherwise, if there are "liftable" selections, we must return a set comprised of those lifted selection,
            // and the current fragment _without_ those lifted selections.
            if liftable_selections.len() > 0 {
                // Converting `... [on T] { <liftable_selections> <non-liftable_selections> }` into
                // `{ ... [on T] { <non-liftable_selections> } <liftable_selections> }`.
                // PORT_NOTE: It appears that this lifting could be repeatable (meaning lifted
                // selection could be broken down further and lifted again), but normalize is not
                // applied recursively. This could be worth investigating.
                let Some(rebased_inline_fragment) = self.inline_fragment.rebase_on(
                    parent_type,
                    schema,
                    RebaseErrorHandlingOption::ThrowError,
                )?
                else {
                    return Err(FederationError::internal(
                        "Rebase should've thrown an error",
                    ));
                };
                let mut mutable_selections = self.selection_set.selections.clone();
                let final_fragment_selections = Arc::make_mut(&mut mutable_selections);
                final_fragment_selections.retain(|k, _| !liftable_selections.contains_key(k));
                let rebased_casted_type = rebased_inline_fragment.data().casted_type();
                let final_inline_fragment: Selection = InlineFragmentSelection::new(
                    rebased_inline_fragment,
                    SelectionSet {
                        schema: schema.clone(),
                        type_position: rebased_casted_type,
                        selections: Arc::new(final_fragment_selections.clone()),
                    },
                )
                .into();

                // Since liftable_selections are changing their parent, we need to rebase them.
                liftable_selections = liftable_selections
                    .into_iter()
                    .map(|(_key, sel)| {
                        sel.rebase_on(
                            parent_type,
                            named_fragments,
                            schema,
                            RebaseErrorHandlingOption::ThrowError,
                        )?
                        .ok_or_else(|| {
                            FederationError::internal("Unable to rebase selection updates")
                        })
                    })
                    .collect::<Result<_, _>>()?;

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
        } else if let Some(rebased_inline_fragment) = self.inline_fragment.rebase_on(
            parent_type,
            schema,
            RebaseErrorHandlingOption::ThrowError,
        )? {
            let rebased_casted_type = rebased_inline_fragment.data().casted_type();
            let rebased_selection_set = normalized_selection_set.rebase_on(
                &rebased_casted_type,
                named_fragments,
                schema,
                RebaseErrorHandlingOption::ThrowError,
            )?;
            Ok(Some(SelectionOrSet::Selection(Selection::InlineFragment(
                Arc::new(InlineFragmentSelection::new(
                    rebased_inline_fragment,
                    rebased_selection_set,
                )),
            ))))
        } else {
            unreachable!("We should always be able to either rebase the inline fragment OR throw an exception");
        }
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

    /// Returns true if this inline fragment selection is "unnecessary" and should be inlined.
    ///
    /// Fragment is unnecessary if following are true:
    /// * it has no applied directives
    /// * has no type condition OR type condition is same as passed in `maybe_parent`
    fn is_unnecessary(&self, maybe_parent: &CompositeTypeDefinitionPosition) -> bool {
        let inline_fragment = self.inline_fragment.data();
        let inline_fragment_type_condition = inline_fragment.type_condition_position.clone();
        inline_fragment.directives.is_empty()
            && (inline_fragment_type_condition.is_none()
                || inline_fragment_type_condition.is_some_and(|t| t == *maybe_parent))
    }

    pub(crate) fn any_element(
        &self,
        predicate: &mut impl FnMut(OpPathElement) -> Result<bool, FederationError>,
    ) -> Result<bool, FederationError> {
        if predicate(self.inline_fragment.clone().into())? {
            return Ok(true);
        }
        self.selection_set.any_element(predicate)
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
            ss.add_typename_field_for_abstract_types(/*parent_type_if_abstract*/ None)
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

/// Tracks fragments from the original operation, along with versions rebased on other subgraphs.
// XXX(@goto-bus-stop): improve/replace/reduce this structure. My notes:
// This gets cloned only in recursive query planning. Then whenever `.for_subgraph()` ends up being
// called, it always clones the `rebased_fragments` map. `.for_subgraph()` is called whenever the
// plan is turned into plan nodes by the FetchDependencyGraphToQueryPlanProcessor.
// This suggests that we can remove the Arc wrapper for `rebased_fragments` because we end up cloning the inner data anyways.
//
// This data structure is also used as an argument in several `crate::operation` functions. This
// seems wrong. The only useful method on this structure is `.for_subgraph()`, which is only used
// by the fetch dependency graph when creating plan nodes. That necessarily implies that all other
// uses of this structure only access `.original_fragments`. In that case, we should pass around
// the `NamedFragments` itself, not this wrapper structure.
//
// `.for_subgraph()` also requires a mutable reference to fill in the data. But
// `.rebased_fragments` is really a cache, so requiring a mutable reference isn't an ideal API.
// Conceptually you are just computing something and getting the result. Perhaps we can use a
// concurrent map, or prepopulate the HashMap for all subgraphs, or precompute the whole thing for
// all subgraphs (or precompute a hash map of subgraph names to OnceLocks).
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

impl Display for FragmentSpread {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let data = self.data();
        f.write_str("...")?;
        f.write_str(&data.fragment_name)?;
        data.directives.serialize().no_indent().fmt(f)
    }
}

impl Display for OperationElement {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationElement::Field(field) => field.fmt(f),
            OperationElement::InlineFragment(inline_fragment) => inline_fragment.fmt(f),
            OperationElement::FragmentSpread(fragment_spread) => fragment_spread.fmt(f),
        }
    }
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
    // We clear up the fragments since we've expanded all.
    // Also note that expanding fragment usually generate unnecessary fragments/inefficient
    // selections, so it basically always make sense to normalize afterwards. Besides, fragment
    // reuse (done by `optimize`) rely on the fact that its input is normalized to work properly,
    // so all the more reason to do it here.
    // PORT_NOTE: This was done in `Operation.expandAllFragments`, but it's moved here.
    normalized_selection_set = normalized_selection_set.normalize(
        &normalized_selection_set.type_position,
        &named_fragments,
        schema,
        NormalizeSelectionOption::NormalizeRecursively,
    )?;
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
