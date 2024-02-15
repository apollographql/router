use crate::error::FederationError;
use crate::error::SingleFederationError::Internal;
use crate::query_graph::graph_path::OpPathElement;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::operation::normalized_field_selection::{
    NormalizedField, NormalizedFieldData, NormalizedFieldSelection,
};
use crate::query_plan::operation::normalized_fragment_spread_selection::{
    NormalizedFragmentSpreadData, NormalizedFragmentSpreadSelection,
};
use crate::query_plan::operation::normalized_inline_fragment_selection::{
    NormalizedInlineFragment, NormalizedInlineFragmentData, NormalizedInlineFragmentSelection,
};
use crate::query_plan::operation::normalized_selection_map::{
    Entry, NormalizedFieldSelectionValue, NormalizedFragmentSpreadSelectionValue,
    NormalizedInlineFragmentSelectionValue, NormalizedSelectionMap, NormalizedSelectionValue,
};
use crate::schema::position::{
    CompositeTypeDefinitionPosition, InterfaceTypeDefinitionPosition, SchemaRootDefinitionKind,
};
use crate::schema::ValidFederationSchema;
use apollo_compiler::ast::{DirectiveList, Name, OperationType};
use apollo_compiler::executable::{
    Field, Fragment, FragmentSpread, InlineFragment, Operation, Selection, SelectionSet,
    VariableDefinition,
};
use apollo_compiler::{name, Node};
use indexmap::{IndexMap, IndexSet};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::sync::{atomic, Arc};

const TYPENAME_FIELD: Name = name!("__typename");

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
/// - Swaps `operation_type` with `root_kind` (using the analogous federation-next type).
/// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
/// - Stores the fragments used by this operation (the executable document the operation was taken
///   from may contain other fragments that are not used by this operation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedOperation {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) root_kind: SchemaRootDefinitionKind,
    pub(crate) name: Option<Name>,
    pub(crate) variables: Arc<Vec<Node<VariableDefinition>>>,
    pub(crate) directives: Arc<DirectiveList>,
    pub(crate) selection_set: NormalizedSelectionSet,
    pub(crate) fragments: Arc<IndexMap<Name, Node<NormalizedFragment>>>,
}

/// An analogue of the apollo-compiler type `SelectionSet` with these changes:
/// - For the type, stores the schema and the position in that schema instead of just the
///   `NamedType`.
/// - Stores selections in a map so they can be normalized efficiently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedSelectionSet {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) type_position: CompositeTypeDefinitionPosition,
    pub(crate) selections: Arc<NormalizedSelectionMap>,
}

pub(crate) mod normalized_selection_map {
    use crate::error::FederationError;
    use crate::error::SingleFederationError::Internal;
    use crate::query_plan::operation::normalized_field_selection::NormalizedFieldSelection;
    use crate::query_plan::operation::normalized_fragment_spread_selection::NormalizedFragmentSpreadSelection;
    use crate::query_plan::operation::normalized_inline_fragment_selection::NormalizedInlineFragmentSelection;
    use crate::query_plan::operation::{
        HasNormalizedSelectionKey, NormalizedSelection, NormalizedSelectionKey,
        NormalizedSelectionSet,
    };
    use apollo_compiler::ast::Name;
    use indexmap::IndexMap;
    use std::borrow::{Borrow, Cow};
    use std::hash::Hash;
    use std::iter::Map;
    use std::ops::Deref;
    use std::sync::Arc;

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
    pub(crate) struct NormalizedSelectionMap(IndexMap<NormalizedSelectionKey, NormalizedSelection>);

    impl Deref for NormalizedSelectionMap {
        type Target = IndexMap<NormalizedSelectionKey, NormalizedSelection>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl NormalizedSelectionMap {
        pub(crate) fn new() -> Self {
            NormalizedSelectionMap(IndexMap::new())
        }

        pub(crate) fn clear(&mut self) {
            self.0.clear();
        }

        pub(crate) fn insert(&mut self, value: NormalizedSelection) -> Option<NormalizedSelection> {
            self.0.insert(value.key(), value)
        }

        pub(crate) fn remove<Q: ?Sized>(&mut self, key: &Q) -> Option<NormalizedSelection>
        where
            NormalizedSelectionKey: Borrow<Q>,
            Q: Eq + Hash,
        {
            // We specifically use shift_remove() instead of swap_remove() to maintain order.
            self.0.shift_remove(key)
        }

        pub(crate) fn get_mut<Q: ?Sized>(&mut self, key: &Q) -> Option<NormalizedSelectionValue>
        where
            NormalizedSelectionKey: Borrow<Q>,
            Q: Eq + Hash,
        {
            self.0.get_mut(key).map(NormalizedSelectionValue::new)
        }

        pub(crate) fn iter_mut(&mut self) -> IterMut {
            self.0
                .iter_mut()
                .map(|(k, v)| (k, NormalizedSelectionValue::new(v)))
        }

        pub(super) fn entry(&mut self, key: NormalizedSelectionKey) -> Entry {
            match self.0.entry(key) {
                indexmap::map::Entry::Occupied(entry) => Entry::Occupied(OccupiedEntry(entry)),
                indexmap::map::Entry::Vacant(entry) => Entry::Vacant(VacantEntry(entry)),
            }
        }

        /// Returns the selection set resulting from "recursively" filtering any selection
        /// that does not match the provided predicate.
        /// This method calls `predicate` on every selection of the selection set,
        /// not just top-level ones, and apply a "depth-first" strategy:
        /// when the predicate is called on a given selection it is guaranteed that
        /// filtering has happened on all the selections of its sub-selection.
        pub(crate) fn filter_recursive_depth_first(
            &self,
            predicate: &mut dyn FnMut(&NormalizedSelection) -> Result<bool, FederationError>,
        ) -> Result<Cow<'_, Self>, FederationError> {
            fn recur_sub_selections<'sel>(
                selection: &'sel NormalizedSelection,
                predicate: &mut dyn FnMut(&NormalizedSelection) -> Result<bool, FederationError>,
            ) -> Result<Cow<'sel, NormalizedSelection>, FederationError> {
                Ok(match selection {
                    NormalizedSelection::Field(field) => {
                        if let Some(sub_selections) = &field.selection_set {
                            match sub_selections.filter_recursive_depth_first(predicate)? {
                                Cow::Borrowed(_) => Cow::Borrowed(selection),
                                Cow::Owned(new) => Cow::Owned(NormalizedSelection::Field(
                                    Arc::new(NormalizedFieldSelection {
                                        field: field.field.clone(),
                                        selection_set: Some(new),
                                    }),
                                )),
                            }
                        } else {
                            Cow::Borrowed(selection)
                        }
                    }
                    NormalizedSelection::InlineFragment(fragment) => match fragment
                        .selection_set
                        .filter_recursive_depth_first(predicate)?
                    {
                        Cow::Borrowed(_) => Cow::Borrowed(selection),
                        Cow::Owned(selection_set) => {
                            Cow::Owned(NormalizedSelection::InlineFragment(Arc::new(
                                NormalizedInlineFragmentSelection {
                                    inline_fragment: fragment.inline_fragment.clone(),
                                    selection_set,
                                },
                            )))
                        }
                    },
                    NormalizedSelection::FragmentSpread(_) => {
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

    type IterMut<'a> = Map<
        indexmap::map::IterMut<'a, NormalizedSelectionKey, NormalizedSelection>,
        fn(
            (&'a NormalizedSelectionKey, &'a mut NormalizedSelection),
        ) -> (&'a NormalizedSelectionKey, NormalizedSelectionValue<'a>),
    >;

    /// A mutable reference to a `NormalizedSelection` value in a `NormalizedSelectionMap`, which
    /// also disallows changing key-related data (to maintain the invariant that a value's key is
    /// the same as it's map entry's key).
    #[derive(Debug)]
    pub(crate) enum NormalizedSelectionValue<'a> {
        Field(NormalizedFieldSelectionValue<'a>),
        FragmentSpread(NormalizedFragmentSpreadSelectionValue<'a>),
        InlineFragment(NormalizedInlineFragmentSelectionValue<'a>),
    }

    impl<'a> NormalizedSelectionValue<'a> {
        pub(crate) fn new(selection: &'a mut NormalizedSelection) -> Self {
            match selection {
                NormalizedSelection::Field(field_selection) => NormalizedSelectionValue::Field(
                    NormalizedFieldSelectionValue::new(field_selection),
                ),
                NormalizedSelection::FragmentSpread(fragment_spread_selection) => {
                    NormalizedSelectionValue::FragmentSpread(
                        NormalizedFragmentSpreadSelectionValue::new(fragment_spread_selection),
                    )
                }
                NormalizedSelection::InlineFragment(inline_fragment_selection) => {
                    NormalizedSelectionValue::InlineFragment(
                        NormalizedInlineFragmentSelectionValue::new(inline_fragment_selection),
                    )
                }
            }
        }
    }

    #[derive(Debug)]
    pub(crate) struct NormalizedFieldSelectionValue<'a>(&'a mut Arc<NormalizedFieldSelection>);

    impl<'a> NormalizedFieldSelectionValue<'a> {
        pub(crate) fn new(field_selection: &'a mut Arc<NormalizedFieldSelection>) -> Self {
            Self(field_selection)
        }

        pub(crate) fn get(&self) -> &Arc<NormalizedFieldSelection> {
            self.0
        }

        pub(crate) fn get_sibling_typename_mut(&mut self) -> &mut Option<Name> {
            Arc::make_mut(self.0).field.sibling_typename_mut()
        }

        pub(crate) fn get_selection_set_mut(&mut self) -> &mut Option<NormalizedSelectionSet> {
            &mut Arc::make_mut(self.0).selection_set
        }
    }

    #[derive(Debug)]
    pub(crate) struct NormalizedFragmentSpreadSelectionValue<'a>(
        &'a mut Arc<NormalizedFragmentSpreadSelection>,
    );

    impl<'a> NormalizedFragmentSpreadSelectionValue<'a> {
        pub(crate) fn new(
            fragment_spread_selection: &'a mut Arc<NormalizedFragmentSpreadSelection>,
        ) -> Self {
            Self(fragment_spread_selection)
        }

        pub(crate) fn get(&self) -> &Arc<NormalizedFragmentSpreadSelection> {
            self.0
        }
    }

    #[derive(Debug)]
    pub(crate) struct NormalizedInlineFragmentSelectionValue<'a>(
        &'a mut Arc<NormalizedInlineFragmentSelection>,
    );

    impl<'a> NormalizedInlineFragmentSelectionValue<'a> {
        pub(crate) fn new(
            inline_fragment_selection: &'a mut Arc<NormalizedInlineFragmentSelection>,
        ) -> Self {
            Self(inline_fragment_selection)
        }

        pub(crate) fn get(&self) -> &Arc<NormalizedInlineFragmentSelection> {
            self.0
        }

        pub(crate) fn get_selection_set_mut(&mut self) -> &mut NormalizedSelectionSet {
            &mut Arc::make_mut(self.0).selection_set
        }
    }

    pub(crate) enum Entry<'a> {
        Occupied(OccupiedEntry<'a>),
        Vacant(VacantEntry<'a>),
    }

    pub(crate) struct OccupiedEntry<'a>(
        indexmap::map::OccupiedEntry<'a, NormalizedSelectionKey, NormalizedSelection>,
    );

    impl<'a> OccupiedEntry<'a> {
        pub(crate) fn get(&self) -> &NormalizedSelection {
            self.0.get()
        }

        pub(crate) fn get_mut(&mut self) -> NormalizedSelectionValue {
            NormalizedSelectionValue::new(self.0.get_mut())
        }

        pub(crate) fn into_mut(self) -> NormalizedSelectionValue<'a> {
            NormalizedSelectionValue::new(self.0.into_mut())
        }

        pub(crate) fn key(&self) -> &NormalizedSelectionKey {
            self.0.key()
        }

        pub(crate) fn remove(self) -> NormalizedSelection {
            // We specifically use shift_remove() instead of swap_remove() to maintain order.
            self.0.shift_remove()
        }
    }

    pub(crate) struct VacantEntry<'a>(
        indexmap::map::VacantEntry<'a, NormalizedSelectionKey, NormalizedSelection>,
    );

    impl<'a> VacantEntry<'a> {
        pub(crate) fn key(&self) -> &NormalizedSelectionKey {
            self.0.key()
        }

        pub(crate) fn insert(
            self,
            value: NormalizedSelection,
        ) -> Result<NormalizedSelectionValue<'a>, FederationError> {
            if *self.key() != value.key() {
                return Err(Internal {
                    message: format!(
                        "Key mismatch when inserting selection {} into vacant entry ",
                        value
                    ),
                }
                .into());
            }
            Ok(NormalizedSelectionValue::new(self.0.insert(value)))
        }
    }

    impl IntoIterator for NormalizedSelectionMap {
        type Item = <IndexMap<NormalizedSelectionKey, NormalizedSelection> as IntoIterator>::Item;
        type IntoIter =
            <IndexMap<NormalizedSelectionKey, NormalizedSelection> as IntoIterator>::IntoIter;

        fn into_iter(self) -> Self::IntoIter {
            <IndexMap<NormalizedSelectionKey, NormalizedSelection> as IntoIterator>::into_iter(
                self.0,
            )
        }
    }
}

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
pub(crate) enum NormalizedSelectionKey {
    Field {
        /// The field alias (if specified) or field name in the resulting selection set.
        response_name: Name,
        /// directives applied on the field
        directives: Arc<DirectiveList>,
    },
    FragmentSpread {
        /// The fragment name referenced in the spread.
        name: Name,
        /// Directives applied on the fragment spread (does not contain @defer).
        directives: Arc<DirectiveList>,
    },
    DeferredFragmentSpread {
        /// Unique selection ID used to distinguish deferred fragment spreads that cannot be merged.
        deferred_id: SelectionId,
    },
    InlineFragment {
        /// The optional type condition of the inline fragment.
        type_condition: Option<Name>,
        /// Directives applied on the inline fragment (does not contain @defer).
        directives: Arc<DirectiveList>,
    },
    DeferredInlineFragment {
        /// Unique selection ID used to distinguish deferred inline fragments that cannot be merged.
        deferred_id: SelectionId,
    },
}

pub(crate) trait HasNormalizedSelectionKey {
    fn key(&self) -> NormalizedSelectionKey;
}

/// An analogue of the apollo-compiler type `Selection` that stores our other selection analogues
/// instead of the apollo-compiler types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NormalizedSelection {
    Field(Arc<NormalizedFieldSelection>),
    FragmentSpread(Arc<NormalizedFragmentSpreadSelection>),
    InlineFragment(Arc<NormalizedInlineFragmentSelection>),
}

impl NormalizedSelection {
    fn directives(&self) -> &Arc<DirectiveList> {
        match self {
            NormalizedSelection::Field(field_selection) => &field_selection.field.data().directives,
            NormalizedSelection::FragmentSpread(fragment_spread_selection) => {
                &fragment_spread_selection.data().directives
            }
            NormalizedSelection::InlineFragment(inline_fragment_selection) => {
                &inline_fragment_selection.inline_fragment.data().directives
            }
        }
    }

    pub(crate) fn element(&self) -> Result<OpPathElement, FederationError> {
        match self {
            NormalizedSelection::Field(field_selection) => {
                Ok(OpPathElement::Field(field_selection.field.clone()))
            }
            NormalizedSelection::FragmentSpread(_) => Err(Internal {
                message: "Fragment spread does not have element".to_owned(),
            }
            .into()),
            NormalizedSelection::InlineFragment(inline_fragment_selection) => Ok(
                OpPathElement::InlineFragment(inline_fragment_selection.inline_fragment.clone()),
            ),
        }
    }

    pub(crate) fn selection_set(&self) -> Result<Option<&NormalizedSelectionSet>, FederationError> {
        match self {
            NormalizedSelection::Field(field_selection) => {
                Ok(field_selection.selection_set.as_ref())
            }
            NormalizedSelection::FragmentSpread(_) => Err(Internal {
                message: "Fragment spread does not directly have a selection set".to_owned(),
            }
            .into()),
            NormalizedSelection::InlineFragment(inline_fragment_selection) => {
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
                NormalizedSelection::Field(_) => {
                    // The sub-selections of this field don't affect whether we should query this
                    // field, so we explicitly do not merge them in.
                    //
                    // PORT_NOTE: The JS codebase merges the sub-selections' conditions in with the
                    // field's conditions when field's selections are non-boolean. This is arguably
                    // a bug, so we've fixed it here.
                    Ok(self_conditions)
                }
                NormalizedSelection::InlineFragment(inline) => {
                    Ok(self_conditions.merge(inline.selection_set.conditions()?))
                }
                NormalizedSelection::FragmentSpread(_x) => Err(FederationError::internal(
                    "Unexpected fragment spread in NormalizedSelection::conditions()",
                )),
            }
        }
    }

    pub(crate) fn has_defer(&self) -> Result<bool, FederationError> {
        todo!()
    }
}

impl HasNormalizedSelectionKey for NormalizedSelection {
    fn key(&self) -> NormalizedSelectionKey {
        match self {
            NormalizedSelection::Field(field_selection) => field_selection.key(),
            NormalizedSelection::FragmentSpread(fragment_spread_selection) => {
                fragment_spread_selection.key()
            }
            NormalizedSelection::InlineFragment(inline_fragment_selection) => {
                inline_fragment_selection.key()
            }
        }
    }
}

/// An analogue of the apollo-compiler type `Fragment` with these changes:
/// - Stores the type condition explicitly, which means storing the schema and position (in
///   apollo-compiler, this is in the `SelectionSet`).
/// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedFragment {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) name: Name,
    pub(crate) type_condition_position: CompositeTypeDefinitionPosition,
    pub(crate) directives: Arc<DirectiveList>,
    pub(crate) selection_set: NormalizedSelectionSet,
}

impl NormalizedFragment {
    fn normalize(
        fragment: &Fragment,
        schema: &ValidFederationSchema,
    ) -> Result<Self, FederationError> {
        Ok(Self {
            schema: schema.clone(),
            name: fragment.name.clone(),
            type_condition_position: schema
                .get_type(fragment.type_condition().clone())?
                .try_into()?,
            directives: Arc::new(fragment.directives.clone()),
            selection_set: NormalizedSelectionSet::normalize_and_expand_fragments(
                &fragment.selection_set,
                &IndexMap::new(),
                schema,
                FragmentSpreadNormalizationOption::PreserveFragmentSpread,
            )?,
        })
    }
}

pub(crate) mod normalized_field_selection {
    use crate::error::FederationError;
    use crate::query_plan::operation::{
        directives_with_sorted_arguments, HasNormalizedSelectionKey, NormalizedSelectionKey,
        NormalizedSelectionSet,
    };
    use crate::schema::position::{FieldDefinitionPosition, TypeDefinitionPosition};
    use crate::schema::ValidFederationSchema;
    use apollo_compiler::ast::{Argument, DirectiveList, Name};
    use apollo_compiler::Node;
    use std::sync::Arc;

    /// An analogue of the apollo-compiler type `Field` with these changes:
    /// - Makes the selection set optional. This is because `NormalizedSelectionSet` requires a type of
    ///   `CompositeTypeDefinitionPosition`, which won't exist for fields returning a non-composite type
    ///   (scalars and enums).
    /// - Stores the field data (other than the selection set) in `NormalizedField`, to facilitate
    ///   operation paths and graph paths.
    /// - For the field definition, stores the schema and the position in that schema instead of just
    ///   the `FieldDefinition` (which contains no references to the parent type or schema).
    /// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct NormalizedFieldSelection {
        pub(crate) field: NormalizedField,
        pub(crate) selection_set: Option<NormalizedSelectionSet>,
    }

    impl HasNormalizedSelectionKey for NormalizedFieldSelection {
        fn key(&self) -> NormalizedSelectionKey {
            self.field.key()
        }
    }

    /// The non-selection-set data of `NormalizedFieldSelection`, used with operation paths and graph
    /// paths.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct NormalizedField {
        data: NormalizedFieldData,
        key: NormalizedSelectionKey,
    }

    impl NormalizedField {
        pub(crate) fn new(data: NormalizedFieldData) -> Self {
            Self {
                key: data.key(),
                data,
            }
        }

        pub(crate) fn data(&self) -> &NormalizedFieldData {
            &self.data
        }

        pub(crate) fn sibling_typename_mut(&mut self) -> &mut Option<Name> {
            &mut self.data.sibling_typename
        }
    }

    impl HasNormalizedSelectionKey for NormalizedField {
        fn key(&self) -> NormalizedSelectionKey {
            self.key.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct NormalizedFieldData {
        pub(crate) schema: ValidFederationSchema,
        pub(crate) field_position: FieldDefinitionPosition,
        pub(crate) alias: Option<Name>,
        pub(crate) arguments: Arc<Vec<Node<Argument>>>,
        pub(crate) directives: Arc<DirectiveList>,
        pub(crate) sibling_typename: Option<Name>,
    }

    impl NormalizedFieldData {
        pub(crate) fn name(&self) -> &Name {
            self.field_position.field_name()
        }

        pub(crate) fn response_name(&self) -> Name {
            self.alias.clone().unwrap_or_else(|| self.name().clone())
        }

        pub(crate) fn is_leaf(&self) -> Result<bool, FederationError> {
            let definition = self.field_position.get(self.schema.schema())?;
            let base_type_position = self
                .schema
                .get_type(definition.ty.inner_named_type().clone())?;
            Ok(matches!(
                base_type_position,
                TypeDefinitionPosition::Scalar(_) | TypeDefinitionPosition::Enum(_)
            ))
        }
    }

    impl HasNormalizedSelectionKey for NormalizedFieldData {
        fn key(&self) -> NormalizedSelectionKey {
            NormalizedSelectionKey::Field {
                response_name: self.response_name(),
                directives: Arc::new(directives_with_sorted_arguments(&self.directives)),
            }
        }
    }
}

pub(crate) mod normalized_fragment_spread_selection {
    use crate::query_plan::operation::{
        directives_with_sorted_arguments, is_deferred_selection, HasNormalizedSelectionKey,
        NormalizedSelectionKey, SelectionId,
    };
    use crate::schema::ValidFederationSchema;
    use apollo_compiler::ast::{DirectiveList, Name};
    use std::sync::Arc;

    /// An analogue of the apollo-compiler type `FragmentSpread` with these changes:
    /// - Stores the schema (may be useful for directives).
    /// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct NormalizedFragmentSpreadSelection {
        data: NormalizedFragmentSpreadData,
        key: NormalizedSelectionKey,
    }

    impl NormalizedFragmentSpreadSelection {
        pub(crate) fn new(data: NormalizedFragmentSpreadData) -> Self {
            Self {
                key: data.key(),
                data,
            }
        }

        pub(crate) fn data(&self) -> &NormalizedFragmentSpreadData {
            &self.data
        }
    }

    impl HasNormalizedSelectionKey for NormalizedFragmentSpreadSelection {
        fn key(&self) -> NormalizedSelectionKey {
            self.key.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct NormalizedFragmentSpreadData {
        pub(crate) schema: ValidFederationSchema,
        pub(crate) fragment_name: Name,
        pub(crate) directives: Arc<DirectiveList>,
        pub(crate) selection_id: SelectionId,
    }

    impl HasNormalizedSelectionKey for NormalizedFragmentSpreadData {
        fn key(&self) -> NormalizedSelectionKey {
            if is_deferred_selection(&self.directives) {
                NormalizedSelectionKey::DeferredFragmentSpread {
                    deferred_id: self.selection_id.clone(),
                }
            } else {
                NormalizedSelectionKey::FragmentSpread {
                    name: self.fragment_name.clone(),
                    directives: Arc::new(directives_with_sorted_arguments(&self.directives)),
                }
            }
        }
    }
}

pub(crate) mod normalized_inline_fragment_selection {
    use crate::error::FederationError;
    use crate::link::graphql_definition::{defer_directive_arguments, DeferDirectiveArguments};
    use crate::query_plan::operation::{
        directives_with_sorted_arguments, is_deferred_selection, HasNormalizedSelectionKey,
        NormalizedSelectionKey, NormalizedSelectionSet, SelectionId,
    };
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use apollo_compiler::ast::DirectiveList;
    use std::sync::Arc;

    /// An analogue of the apollo-compiler type `InlineFragment` with these changes:
    /// - Stores the inline fragment data (other than the selection set) in `NormalizedInlineFragment`,
    ///   to facilitate operation paths and graph paths.
    /// - For the type condition, stores the schema and the position in that schema instead of just
    ///   the `NamedType`.
    /// - Stores the parent type explicitly, which means storing the position (in apollo-compiler, this
    ///   is in the parent selection set).
    /// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct NormalizedInlineFragmentSelection {
        pub(crate) inline_fragment: NormalizedInlineFragment,
        pub(crate) selection_set: NormalizedSelectionSet,
    }

    impl HasNormalizedSelectionKey for NormalizedInlineFragmentSelection {
        fn key(&self) -> NormalizedSelectionKey {
            self.inline_fragment.key()
        }
    }

    /// The non-selection-set data of `NormalizedInlineFragmentSelection`, used with operation paths and
    /// graph paths.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct NormalizedInlineFragment {
        data: NormalizedInlineFragmentData,
        key: NormalizedSelectionKey,
    }

    impl NormalizedInlineFragment {
        pub(crate) fn new(data: NormalizedInlineFragmentData) -> Self {
            Self {
                key: data.key(),
                data,
            }
        }

        pub(crate) fn data(&self) -> &NormalizedInlineFragmentData {
            &self.data
        }
    }

    impl HasNormalizedSelectionKey for NormalizedInlineFragment {
        fn key(&self) -> NormalizedSelectionKey {
            self.key.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct NormalizedInlineFragmentData {
        pub(crate) schema: ValidFederationSchema,
        pub(crate) parent_type_position: CompositeTypeDefinitionPosition,
        pub(crate) type_condition_position: Option<CompositeTypeDefinitionPosition>,
        pub(crate) directives: Arc<DirectiveList>,
        pub(crate) selection_id: SelectionId,
    }

    impl NormalizedInlineFragmentData {
        pub(crate) fn defer_directive_arguments(
            &self,
        ) -> Result<Option<DeferDirectiveArguments>, FederationError> {
            if let Some(directive) = self.directives.get("defer") {
                Ok(Some(defer_directive_arguments(directive)?))
            } else {
                Ok(None)
            }
        }
    }

    impl HasNormalizedSelectionKey for NormalizedInlineFragmentData {
        fn key(&self) -> NormalizedSelectionKey {
            if is_deferred_selection(&self.directives) {
                NormalizedSelectionKey::DeferredInlineFragment {
                    deferred_id: self.selection_id.clone(),
                }
            } else {
                NormalizedSelectionKey::InlineFragment {
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

/// Available fragment spread normalization options
#[derive(Copy, Clone)]
pub(crate) enum FragmentSpreadNormalizationOption {
    InlineFragmentSpread,
    PreserveFragmentSpread,
}

impl NormalizedSelectionSet {
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

    fn is_empty(&self) -> bool {
        self.selections.is_empty()
    }

    pub(crate) fn contains_top_level_field(
        &self,
        field: &NormalizedField,
    ) -> Result<bool, FederationError> {
        if let Some(selection) = self.selections.get(&field.key()) {
            let NormalizedSelection::Field(field_selection) = selection else {
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
    pub(crate) fn normalize_and_expand_fragments(
        selection_set: &SelectionSet,
        fragments: &IndexMap<Name, Node<Fragment>>,
        schema: &ValidFederationSchema,
        normalize_fragment_spread_option: FragmentSpreadNormalizationOption,
    ) -> Result<NormalizedSelectionSet, FederationError> {
        let type_position: CompositeTypeDefinitionPosition =
            schema.get_type(selection_set.ty.clone())?.try_into()?;
        let mut normalized_selections = vec![];
        NormalizedSelectionSet::normalize_selections(
            &selection_set.selections,
            &type_position,
            &mut normalized_selections,
            fragments,
            schema,
            normalize_fragment_spread_option,
        )?;
        let mut merged = NormalizedSelectionSet {
            schema: schema.clone(),
            type_position,
            selections: Arc::new(NormalizedSelectionMap::new()),
        };
        merged.merge_selections_into(normalized_selections.into_iter())?;
        Ok(merged)
    }

    /// A helper function for normalizing a list of selections into a destination.
    fn normalize_selections(
        selections: &[Selection],
        parent_type_position: &CompositeTypeDefinitionPosition,
        destination: &mut Vec<NormalizedSelection>,
        fragments: &IndexMap<Name, Node<Fragment>>,
        schema: &ValidFederationSchema,
        normalize_fragment_spread_option: FragmentSpreadNormalizationOption,
    ) -> Result<(), FederationError> {
        for selection in selections {
            match selection {
                Selection::Field(field_selection) => {
                    let Some(normalized_field_selection) =
                        NormalizedFieldSelection::normalize_and_expand_fragments(
                            field_selection,
                            parent_type_position,
                            fragments,
                            schema,
                            normalize_fragment_spread_option,
                        )?
                    else {
                        continue;
                    };
                    destination.push(NormalizedSelection::Field(Arc::new(
                        normalized_field_selection,
                    )));
                }
                Selection::FragmentSpread(fragment_spread_selection) => {
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
                    if let FragmentSpreadNormalizationOption::InlineFragmentSpread =
                        normalize_fragment_spread_option
                    {
                        // We can hoist/collapse named fragments if their type condition is on the
                        // parent type and they don't have any directives.
                        if fragment.type_condition() == parent_type_position.type_name()
                            && fragment_spread_selection.directives.is_empty()
                        {
                            NormalizedSelectionSet::normalize_selections(
                                &fragment.selection_set.selections,
                                parent_type_position,
                                destination,
                                fragments,
                                schema,
                                normalize_fragment_spread_option,
                            )?;
                        } else {
                            let normalized_inline_fragment_selection =
                                NormalizedFragmentSpreadSelection::normalize_and_expand_fragments(
                                    fragment_spread_selection,
                                    parent_type_position,
                                    fragments,
                                    schema,
                                    normalize_fragment_spread_option,
                                )?;
                            destination.push(NormalizedSelection::InlineFragment(Arc::new(
                                normalized_inline_fragment_selection,
                            )));
                        }
                    } else {
                        // if we don't expand fragments, we just convert FragmentSpread to NormalizedFragmentSpreadSelection
                        let normalized_fragment_spread =
                            NormalizedFragmentSpreadSelection::normalize(
                                fragment_spread_selection,
                                schema,
                            );
                        destination.push(NormalizedSelection::FragmentSpread(Arc::new(
                            normalized_fragment_spread,
                        )));
                    }
                }
                Selection::InlineFragment(inline_fragment_selection) => {
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
                        NormalizedSelectionSet::normalize_selections(
                            &inline_fragment_selection.selection_set.selections,
                            parent_type_position,
                            destination,
                            fragments,
                            schema,
                            normalize_fragment_spread_option,
                        )?;
                    } else {
                        let normalized_inline_fragment_selection =
                            NormalizedInlineFragmentSelection::normalize_and_expand_fragments(
                                inline_fragment_selection,
                                parent_type_position,
                                fragments,
                                schema,
                                normalize_fragment_spread_option,
                            )?;
                        destination.push(NormalizedSelection::InlineFragment(Arc::new(
                            normalized_inline_fragment_selection,
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Merges the given normalized selection sets into this one.
    pub(crate) fn merge_into(
        &mut self,
        others: impl Iterator<Item = NormalizedSelectionSet> + ExactSizeIterator,
    ) -> Result<(), FederationError> {
        if others.len() > 0 {
            let mut selections_to_merge = vec![];
            for other in others {
                if other.schema != self.schema {
                    return Err(Internal {
                        message: "Cannot merge selection sets from different schemas".to_owned(),
                    }
                    .into());
                }
                if other.type_position != self.type_position {
                    return Err(Internal {
                        message: format!(
                            "Cannot merge selection set for type \"{}\" into a selection set for type \"{}\"",
                            other.type_position,
                            self.type_position,
                        ),
                    }.into());
                }
                let selections = Arc::try_unwrap(other.selections)
                    .unwrap_or_else(|selections| selections.deref().clone());
                for (_, value) in selections {
                    selections_to_merge.push(value);
                }
            }
            self.merge_selections_into(selections_to_merge.into_iter())?;
        }
        Ok(())
    }

    /// A helper function for merging the given selections into this one.
    fn merge_selections_into(
        &mut self,
        others: impl Iterator<Item = NormalizedSelection> + ExactSizeIterator,
    ) -> Result<(), FederationError> {
        if others.len() > 0 {
            let mut fields = IndexMap::new();
            let mut fragment_spreads = IndexMap::new();
            let mut inline_fragments = IndexMap::new();
            for other_selection in others {
                let other_key = other_selection.key();
                match Arc::make_mut(&mut self.selections).entry(other_key.clone()) {
                    Entry::Occupied(existing) => match existing.get() {
                        NormalizedSelection::Field(self_field_selection) => {
                            let NormalizedSelection::Field(other_field_selection) = other_selection
                            else {
                                return Err(Internal {
                                        message: format!(
                                            "Field selection key for field \"{}\" references non-field selection",
                                            self_field_selection.field.data().field_position,
                                        ),
                                    }.into());
                            };
                            let other_field_selection = Arc::try_unwrap(other_field_selection)
                                .unwrap_or_else(|selection| selection.deref().clone());
                            fields
                                .entry(other_key)
                                .or_insert_with(Vec::new)
                                .push(other_field_selection);
                        }
                        NormalizedSelection::FragmentSpread(self_fragment_spread_selection) => {
                            let NormalizedSelection::FragmentSpread(
                                other_fragment_spread_selection,
                            ) = other_selection
                            else {
                                return Err(Internal {
                                        message: format!(
                                            "Fragment spread selection key for fragment \"{}\" references non-field selection",
                                            self_fragment_spread_selection.data().fragment_name,
                                        ),
                                    }.into());
                            };
                            let other_fragment_spread_selection =
                                Arc::try_unwrap(other_fragment_spread_selection)
                                    .unwrap_or_else(|selection| selection.deref().clone());
                            fragment_spreads
                                .entry(other_key)
                                .or_insert_with(Vec::new)
                                .push(other_fragment_spread_selection);
                        }
                        NormalizedSelection::InlineFragment(self_inline_fragment_selection) => {
                            let NormalizedSelection::InlineFragment(
                                other_inline_fragment_selection,
                            ) = other_selection
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
                            let other_inline_fragment_selection =
                                Arc::try_unwrap(other_inline_fragment_selection)
                                    .unwrap_or_else(|selection| selection.deref().clone());
                            inline_fragments
                                .entry(other_key)
                                .or_insert_with(Vec::new)
                                .push(other_inline_fragment_selection);
                        }
                    },
                    Entry::Vacant(vacant) => {
                        vacant.insert(other_selection)?;
                    }
                }
            }
            for (key, self_selection) in Arc::make_mut(&mut self.selections).iter_mut() {
                match self_selection {
                    NormalizedSelectionValue::Field(mut self_field_selection) => {
                        if let Some(other_field_selections) = fields.shift_remove(key) {
                            self_field_selection.merge_into(other_field_selections.into_iter())?;
                        }
                    }
                    NormalizedSelectionValue::FragmentSpread(
                        mut self_fragment_spread_selection,
                    ) => {
                        if let Some(other_fragment_spread_selections) =
                            fragment_spreads.shift_remove(key)
                        {
                            self_fragment_spread_selection
                                .merge_into(other_fragment_spread_selections.into_iter())?;
                        }
                    }
                    NormalizedSelectionValue::InlineFragment(
                        mut self_inline_fragment_selection,
                    ) => {
                        if let Some(other_inline_fragment_selections) =
                            inline_fragments.shift_remove(key)
                        {
                            self_inline_fragment_selection
                                .merge_into(other_inline_fragment_selections.into_iter())?;
                        }
                    }
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
        let mut typename_field_key: Option<NormalizedSelectionKey> = None;
        let mut sibling_field_key: Option<NormalizedSelectionKey> = None;

        let mutable_selection_map = Arc::make_mut(&mut self.selections);
        for (key, entry) in mutable_selection_map.iter_mut() {
            match entry {
                NormalizedSelectionValue::Field(mut field_selection) => {
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
                NormalizedSelectionValue::InlineFragment(mut inline_fragment) => {
                    inline_fragment
                        .get_selection_set_mut()
                        .optimize_sibling_typenames(interface_types_with_interface_objects)?;
                }
                NormalizedSelectionValue::FragmentSpread(fragment_spread) => {
                    // at this point in time all fragment spreads should have been converted into inline fragments
                    return Err(FederationError::SingleFederationError(Internal {
                        message: format!(
                            "Error while optimizing sibling typename information, selection set contains {} named fragment",
                            fragment_spread.get().data().fragment_name
                        ),
                    }));
                }
            }
        }

        if let (Some(typename_key), Some(sibling_field_key)) =
            (typename_field_key, sibling_field_key)
        {
            if let (
                Some(NormalizedSelection::Field(typename_field)),
                Some(NormalizedSelectionValue::Field(mut sibling_field)),
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
            NormalizedSelection::Field(field) => Ok(if let Some(set) = &field.selection_set {
                !set.is_empty()
            } else {
                true
            }),
            NormalizedSelection::InlineFragment(inline) => Ok(!inline.selection_set.is_empty()),
            NormalizedSelection::FragmentSpread(_) => {
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
        predicate: &mut dyn FnMut(&NormalizedSelection) -> Result<bool, FederationError>,
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

    pub(crate) fn add_back_typename_in_attachments(
        &self,
    ) -> Result<NormalizedSelectionSet, FederationError> {
        todo!()
    }

    pub(crate) fn add_typename_field_for_abstract_types(
        &self,
    ) -> Result<NormalizedSelectionSet, FederationError> {
        todo!()
    }
}

impl NormalizedFieldSelection {
    /// Normalize this field selection (merging selections with the same keys), with the following
    /// additional transformations:
    /// - Expand fragment spreads into inline fragments.
    /// - Remove `__schema` or `__type` introspection fields, as these shouldn't be handled by query
    ///   planning.
    /// - Hoist fragment spreads/inline fragments into their parents if they have no directives and
    ///   their parent type matches.
    pub(crate) fn normalize_and_expand_fragments(
        field: &Field,
        parent_type_position: &CompositeTypeDefinitionPosition,
        fragments: &IndexMap<Name, Node<Fragment>>,
        schema: &ValidFederationSchema,
        normalize_fragment_spread_option: FragmentSpreadNormalizationOption,
    ) -> Result<Option<NormalizedFieldSelection>, FederationError> {
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

        Ok(Some(NormalizedFieldSelection {
            field: NormalizedField::new(NormalizedFieldData {
                schema: schema.clone(),
                field_position,
                alias: field.alias.clone(),
                arguments: Arc::new(field.arguments.clone()),
                directives: Arc::new(field.directives.clone()),
                sibling_typename: None,
            }),
            selection_set: if field_composite_type_result.is_ok() {
                Some(NormalizedSelectionSet::normalize_and_expand_fragments(
                    &field.selection_set,
                    fragments,
                    schema,
                    normalize_fragment_spread_option,
                )?)
            } else {
                None
            },
        }))
    }
}

impl<'a> NormalizedFieldSelectionValue<'a> {
    /// Merges the given normalized field selections into this one (this method assumes the keys
    /// already match).
    pub(crate) fn merge_into(
        &mut self,
        others: impl Iterator<Item = NormalizedFieldSelection> + ExactSizeIterator,
    ) -> Result<(), FederationError> {
        if others.len() > 0 {
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
                    let Some(other_selection_set) = other.selection_set else {
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
        }
        Ok(())
    }
}

impl NormalizedFragmentSpreadSelection {
    /// Copies fragment spread selection and assigns it a new unique selection ID.
    pub(crate) fn with_unique_id(&self) -> Self {
        let mut data = self.data().clone();
        data.selection_id = SelectionId::new();
        Self::new(data)
    }

    /// Normalize this fragment spread into a "normalized" spread representation with following
    /// modifications
    /// - Stores the schema (may be useful for directives).
    /// - Encloses list of directives in `Arc`s to facilitate cheaper cloning.
    /// - Stores unique selection ID (used for deferred fragments)
    pub(crate) fn normalize(
        fragment_spread: &FragmentSpread,
        schema: &ValidFederationSchema,
    ) -> NormalizedFragmentSpreadSelection {
        NormalizedFragmentSpreadSelection::new(NormalizedFragmentSpreadData {
            schema: schema.clone(),
            fragment_name: fragment_spread.fragment_name.clone(),
            directives: Arc::new(fragment_spread.directives.clone()),
            selection_id: SelectionId::new(),
        })
    }

    /// Normalize this fragment spread (merging selections with the same keys), with the following
    /// additional transformations:
    /// - Expand fragment spreads into inline fragments.
    /// - Remove `__schema` or `__type` introspection fields, as these shouldn't be handled by query
    ///   planning.
    /// - Hoist fragment spreads/inline fragments into their parents if they have no directives and
    ///   their parent type matches.
    pub(crate) fn normalize_and_expand_fragments(
        fragment_spread: &FragmentSpread,
        parent_type_position: &CompositeTypeDefinitionPosition,
        fragments: &IndexMap<Name, Node<Fragment>>,
        schema: &ValidFederationSchema,
        normalize_fragment_spread_option: FragmentSpreadNormalizationOption,
    ) -> Result<NormalizedInlineFragmentSelection, FederationError> {
        let Some(fragment) = fragments.get(&fragment_spread.fragment_name) else {
            return Err(Internal {
                message: format!(
                    "Fragment spread referenced non-existent fragment \"{}\"",
                    fragment_spread.fragment_name,
                ),
            }
            .into());
        };
        let type_condition_position: CompositeTypeDefinitionPosition = schema
            .get_type(fragment.type_condition().clone())?
            .try_into()?;

        // PORT_NOTE: The JS codebase combined the fragment spread's directives with the fragment
        // definition's directives. This was invalid GraphQL, so we're explicitly ignoring the
        // fragment definition's directives here (which isn't great, but there's not a simple
        // alternative at the moment).
        Ok(NormalizedInlineFragmentSelection {
            inline_fragment: NormalizedInlineFragment::new(NormalizedInlineFragmentData {
                schema: schema.clone(),
                parent_type_position: parent_type_position.clone(),
                type_condition_position: Some(type_condition_position),
                directives: Arc::new(fragment_spread.directives.clone()),
                selection_id: SelectionId::new(),
            }),
            selection_set: NormalizedSelectionSet::normalize_and_expand_fragments(
                &fragment.selection_set,
                fragments,
                schema,
                normalize_fragment_spread_option,
            )?,
        })
    }
}

impl<'a> NormalizedFragmentSpreadSelectionValue<'a> {
    /// Merges the given normalized fragment spread selections into this one (this method assumes
    /// the keys already match).
    pub(crate) fn merge_into(
        &mut self,
        others: impl Iterator<Item = NormalizedFragmentSpreadSelection> + ExactSizeIterator,
    ) -> Result<(), FederationError> {
        if others.len() > 0 {
            for other in others {
                if other.data().schema != self.get().data().schema {
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
        }
        Ok(())
    }
}

impl NormalizedInlineFragmentSelection {
    /// Copies inline fragment selection and assigns it a new unique selection ID.
    pub(crate) fn with_unique_id(&self) -> Self {
        let mut data = self.inline_fragment.data().clone();
        data.selection_id = SelectionId::new();
        Self {
            inline_fragment: NormalizedInlineFragment::new(data),
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
    pub(crate) fn normalize_and_expand_fragments(
        inline_fragment: &InlineFragment,
        parent_type_position: &CompositeTypeDefinitionPosition,
        fragments: &IndexMap<Name, Node<Fragment>>,
        schema: &ValidFederationSchema,
        normalize_fragment_spread_option: FragmentSpreadNormalizationOption,
    ) -> Result<NormalizedInlineFragmentSelection, FederationError> {
        let type_condition_position: Option<CompositeTypeDefinitionPosition> =
            if let Some(type_condition) = &inline_fragment.type_condition {
                Some(schema.get_type(type_condition.clone())?.try_into()?)
            } else {
                None
            };
        Ok(NormalizedInlineFragmentSelection {
            inline_fragment: NormalizedInlineFragment::new(NormalizedInlineFragmentData {
                schema: schema.clone(),
                parent_type_position: parent_type_position.clone(),
                type_condition_position,
                directives: Arc::new(inline_fragment.directives.clone()),
                selection_id: SelectionId::new(),
            }),
            selection_set: NormalizedSelectionSet::normalize_and_expand_fragments(
                &inline_fragment.selection_set,
                fragments,
                schema,
                normalize_fragment_spread_option,
            )?,
        })
    }
}

impl<'a> NormalizedInlineFragmentSelectionValue<'a> {
    /// Merges the given normalized inline fragment selections into this one (this method assumes
    /// the keys already match).
    pub(crate) fn merge_into(
        &mut self,
        others: impl Iterator<Item = NormalizedInlineFragmentSelection> + ExactSizeIterator,
    ) -> Result<(), FederationError> {
        if others.len() > 0 {
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
                selection_sets.push(other.selection_set);
            }
            self.get_selection_set_mut()
                .merge_into(selection_sets.into_iter())?;
        }
        Ok(())
    }
}

pub(crate) fn merge_selection_sets(
    mut selection_sets: impl Iterator<Item = NormalizedSelectionSet> + ExactSizeIterator,
) -> Result<NormalizedSelectionSet, FederationError> {
    let Some(mut first) = selection_sets.next() else {
        return Err(Internal {
            message: "".to_owned(),
        }
        .into());
    };
    first.merge_into(selection_sets)?;
    Ok(first)
}

pub(crate) fn equal_selection_sets(
    _a: &NormalizedSelectionSet,
    _b: &NormalizedSelectionSet,
) -> Result<bool, FederationError> {
    // TODO: Once operation processing is done, we should be able to call into that logic here.
    // We're specifically wanting the equivalent of something like
    // ```
    // selectionSetOfNode(...).equals(selectionSetOfNode(...));
    // ```
    // from the JS codebase. It may be more performant for federation-next to use its own
    // representation instead of repeatedly inter-converting between its representation and the
    // apollo-rs one, but we'll cross that bridge if we come to it.
    todo!();
}

impl TryFrom<&NormalizedOperation> for Operation {
    type Error = FederationError;

    fn try_from(normalized_operation: &NormalizedOperation) -> Result<Self, Self::Error> {
        let operation_type: OperationType = normalized_operation.root_kind.into();
        Ok(Self {
            operation_type,
            name: normalized_operation.name.clone(),
            variables: normalized_operation.variables.deref().clone(),
            directives: normalized_operation.directives.deref().clone(),
            selection_set: (&normalized_operation.selection_set).try_into()?,
        })
    }
}

impl TryFrom<&NormalizedSelectionSet> for SelectionSet {
    type Error = FederationError;

    fn try_from(val: &NormalizedSelectionSet) -> Result<Self, Self::Error> {
        let mut flattened = vec![];
        for normalized_selection in val.selections.values() {
            let selection: Selection = normalized_selection.try_into()?;
            flattened.push(selection);
        }
        Ok(Self {
            ty: val.type_position.type_name().clone(),
            selections: flattened,
        })
    }
}

impl TryFrom<&NormalizedSelection> for Selection {
    type Error = FederationError;

    fn try_from(val: &NormalizedSelection) -> Result<Self, Self::Error> {
        Ok(match val {
            NormalizedSelection::Field(normalized_field_selection) => {
                Selection::Field(Node::new(normalized_field_selection.deref().try_into()?))
            }
            NormalizedSelection::FragmentSpread(normalized_fragment_spread_selection) => {
                Selection::FragmentSpread(Node::new(
                    normalized_fragment_spread_selection.deref().into(),
                ))
            }
            NormalizedSelection::InlineFragment(normalized_inline_fragment_selection) => {
                Selection::InlineFragment(Node::new(
                    normalized_inline_fragment_selection.deref().try_into()?,
                ))
            }
        })
    }
}

impl TryFrom<&NormalizedField> for Field {
    type Error = FederationError;

    fn try_from(normalized_field: &NormalizedField) -> Result<Self, Self::Error> {
        let definition = normalized_field
            .data()
            .field_position
            .get(normalized_field.data().schema.schema())?
            .node
            .to_owned();
        let selection_set = SelectionSet {
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

impl TryFrom<&NormalizedFieldSelection> for Field {
    type Error = FederationError;

    fn try_from(val: &NormalizedFieldSelection) -> Result<Self, Self::Error> {
        let mut field = Self::try_from(&val.field)?;
        if let Some(selection_set) = &val.selection_set {
            field.selection_set = selection_set.try_into()?;
        }
        Ok(field)
    }
}

impl TryFrom<&NormalizedInlineFragment> for InlineFragment {
    type Error = FederationError;

    fn try_from(
        normalized_inline_fragment: &NormalizedInlineFragment,
    ) -> Result<Self, Self::Error> {
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
            selection_set: SelectionSet {
                ty,
                selections: Vec::new(),
            },
        })
    }
}

impl TryFrom<&NormalizedInlineFragmentSelection> for InlineFragment {
    type Error = FederationError;

    fn try_from(val: &NormalizedInlineFragmentSelection) -> Result<Self, Self::Error> {
        Ok(Self {
            selection_set: (&val.selection_set).try_into()?,
            ..Self::try_from(&val.inline_fragment)?
        })
    }
}

impl From<&NormalizedFragmentSpreadSelection> for FragmentSpread {
    fn from(val: &NormalizedFragmentSpreadSelection) -> Self {
        Self {
            fragment_name: val.data().fragment_name.to_owned(),
            directives: val.data().directives.deref().to_owned(),
        }
    }
}

impl Display for NormalizedOperation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let operation: Operation = match self.try_into() {
            Ok(operation) => operation,
            Err(_) => return Err(std::fmt::Error),
        };
        operation.serialize().fmt(f)
    }
}

impl Display for NormalizedSelectionSet {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let selection_set: SelectionSet = match self.try_into() {
            Ok(selection_set) => selection_set,
            Err(_) => return Err(std::fmt::Error),
        };
        selection_set.serialize().no_indent().fmt(f)
    }
}

impl Display for NormalizedSelection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let selection: Selection = match self.try_into() {
            Ok(selection) => selection,
            Err(_) => return Err(std::fmt::Error),
        };
        selection.serialize().no_indent().fmt(f)
    }
}

impl Display for NormalizedFieldSelection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let field: Field = match self.try_into() {
            Ok(field) => field,
            Err(_) => return Err(std::fmt::Error),
        };
        field.serialize().no_indent().fmt(f)
    }
}

impl Display for NormalizedInlineFragmentSelection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let inline_fragment: InlineFragment = match self.try_into() {
            Ok(inline_fragment) => inline_fragment,
            Err(_) => return Err(std::fmt::Error),
        };
        inline_fragment.serialize().no_indent().fmt(f)
    }
}

impl Display for NormalizedFragmentSpreadSelection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let fragment_spread: FragmentSpread = self.into();
        fragment_spread.serialize().no_indent().fmt(f)
    }
}

impl Display for NormalizedField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // We create a selection with an empty selection set here, relying on `apollo-rs` to skip
        // serializing it when empty. Note we're implicitly relying on the lack of type-checking
        // in both `NormalizedFieldSelection` and `Field` display logic (specifically, we rely on
        // them not checking whether it is valid for the selection set to be empty).
        let selection = NormalizedFieldSelection {
            field: self.clone(),
            selection_set: None,
        };
        selection.fmt(f)
    }
}

impl Display for NormalizedInlineFragment {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // We can't use the same trick we did with `NormalizedField`'s display logic, since
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

fn directives_with_sorted_arguments(directives: &DirectiveList) -> DirectiveList {
    let mut directives = directives.clone();
    for directive in &mut directives {
        directive
            .make_mut()
            .arguments
            .sort_by(|a1, a2| a1.name.cmp(&a2.name))
    }
    directives
}

fn is_deferred_selection(directives: &DirectiveList) -> bool {
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
    operation: &Operation,
    fragments: &IndexMap<Name, Node<Fragment>>,
    schema: &ValidFederationSchema,
    interface_types_with_interface_objects: &IndexSet<InterfaceTypeDefinitionPosition>,
) -> Result<NormalizedOperation, FederationError> {
    let mut normalized_selection_set = NormalizedSelectionSet::normalize_and_expand_fragments(
        &operation.selection_set,
        fragments,
        schema,
        FragmentSpreadNormalizationOption::InlineFragmentSpread,
    )?;
    normalized_selection_set.optimize_sibling_typenames(interface_types_with_interface_objects)?;

    let normalized_fragments: IndexMap<Name, Node<NormalizedFragment>> = fragments
        .iter()
        .map(|(name, fragment)| {
            (
                name.clone(),
                Node::new(NormalizedFragment::normalize(fragment, schema).unwrap()),
            )
        })
        .collect();

    let schema_definition_root_kind = match operation.operation_type {
        OperationType::Query => SchemaRootDefinitionKind::Query,
        OperationType::Mutation => SchemaRootDefinitionKind::Mutation,
        OperationType::Subscription => SchemaRootDefinitionKind::Subscription,
    };
    let normalized_operation = NormalizedOperation {
        schema: schema.clone(),
        root_kind: schema_definition_root_kind,
        name: operation.name.clone(),
        variables: Arc::new(operation.variables.clone()),
        directives: Arc::new(operation.directives.clone()),
        selection_set: normalized_selection_set,
        fragments: Arc::new(normalized_fragments),
    };
    Ok(normalized_operation)
}

#[cfg(test)]
mod tests {
    use crate::query_plan::operation::normalize_operation;
    use crate::schema::position::InterfaceTypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use apollo_compiler::{name, ExecutableDocument};
    use indexmap::IndexSet;

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
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let expected = r#"query NamedFragmentQuery {
  foo {
    id
    bar
    baz
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
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
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let expected = r#"query NestedFragmentQuery {
  foo {
    id
    bar
    baz
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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

    // TODO enable when @defer is available in apollo-rs
    #[ignore]
    #[test]
    fn do_not_merge_fields_with_defer_directive() {
        let operation_defer_fields = r#"
query Test {
  t @defer {
    v1
  }
  t @defer {
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
        let (schema, mut executable_document) = parse_schema_and_operation(operation_defer_fields);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t @defer {
    v1
  }
  t @defer {
    v2
  }
}"#;
            let actual = normalized_operation.to_string();
            assert_eq!(expected, actual);
        } else {
            panic!("unable to parse document")
        }
    }

    // TODO enable when @defer is available in apollo-rs
    #[ignore]
    #[test]
    fn merge_nested_field_selections() {
        let nested_operation = r#"
query Test {
  t {
    t1
    v @defer {
      v1
    }
  }
  t {
    t1
    t2
    v @defer {
      v2
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
        let (schema, mut executable_document) = parse_schema_and_operation(nested_operation);
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t {
    t1
    v @defer {
      v1
    }
    t2
    v @defer {
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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

    // TODO enable when @defer is available in apollo-rs
    #[ignore]
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
                &executable_document.fragments,
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

    // TODO enable when @defer is available in apollo-rs
    #[ignore]
    #[test]
    fn merge_nested_fragments() {
        let operation_nested_fragments = r#"
query Test {
  t {
    ... on T {
      t1
    }
    ... on T {
      v @defer {
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
      v @defer {
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
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();
            let expected = r#"query Test {
  t {
    t1
    v @defer {
      v1
    }
    t2
    v @defer {
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
}
