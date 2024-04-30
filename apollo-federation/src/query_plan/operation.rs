use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::error::SingleFederationError::Internal;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::query_graph::graph_path::OpPath;
use crate::query_graph::graph_path::OpPathElement;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::operation::normalized_field_selection::{
    NormalizedField, NormalizedFieldData, NormalizedFieldSelection,
};
use crate::query_plan::operation::normalized_fragment_spread_selection::{
    NormalizedFragmentSpread, NormalizedFragmentSpreadData, NormalizedFragmentSpreadSelection,
};
use crate::query_plan::operation::normalized_inline_fragment_selection::{
    NormalizedInlineFragment, NormalizedInlineFragmentData, NormalizedInlineFragmentSelection,
};
use crate::query_plan::operation::normalized_selection_map::{
    Entry, NormalizedFieldSelectionValue, NormalizedFragmentSpreadSelectionValue,
    NormalizedInlineFragmentSelectionValue, NormalizedSelectionMap, NormalizedSelectionValue,
};
use crate::query_plan::FetchDataKeyRenamer;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::schema::definitions::is_composite_type;
use crate::schema::definitions::types_can_be_merged;
use crate::schema::definitions::AbstractType;
use crate::schema::position::{
    CompositeTypeDefinitionPosition, InterfaceTypeDefinitionPosition, ObjectTypeDefinitionPosition,
    SchemaRootDefinitionKind,
};
use crate::schema::ValidFederationSchema;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::{Argument, Directive, DirectiveList, Name, OperationType, Value};
use apollo_compiler::executable;
use apollo_compiler::executable::{
    Field, Fragment, FragmentSpread, InlineFragment, Operation, Selection, SelectionSet,
    VariableDefinition,
};
use apollo_compiler::validation::Valid;
use apollo_compiler::NodeStr;
use apollo_compiler::{name, Node};
use indexmap::{IndexMap, IndexSet};
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use std::ops::Deref;
use std::sync::{atomic, Arc};

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
    pub(crate) named_fragments: NamedFragments,
}

pub(crate) struct NormalizedDefer {
    pub operation: NormalizedOperation,
    pub has_defers: bool,
    pub assigned_defer_labels: HashSet<NodeStr>,
    pub defer_conditions: IndexMap<String, IndexSet<String>>,
}

impl NormalizedOperation {
    // PORT_NOTE(@goto-bus-stop): It might make sense for the returned data structure to *be* the
    // `DeferNormalizer` from the JS side
    pub(crate) fn with_normalized_defer(self) -> NormalizedDefer {
        todo!()
    }

    pub(crate) fn without_defer(self) -> Self {
        if self.selection_set.has_defer()
            || self
                .named_fragments
                .fragments
                .values()
                .any(|f| f.has_defer())
        {
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

        pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<NormalizedSelection>
        where
            NormalizedSelectionKey: Borrow<Q>,
            Q: Eq + Hash + ?Sized,
        {
            // We specifically use shift_remove() instead of swap_remove() to maintain order.
            self.0.shift_remove(key)
        }

        pub(crate) fn retain(
            &mut self,
            mut predicate: impl FnMut(&NormalizedSelectionKey, &NormalizedSelection) -> bool,
        ) {
            self.0.retain(|k, v| predicate(k, v))
        }

        pub(crate) fn get_mut<Q>(&mut self, key: &Q) -> Option<NormalizedSelectionValue>
        where
            NormalizedSelectionKey: Borrow<Q>,
            Q: Eq + Hash + ?Sized,
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

        pub(crate) fn extend(&mut self, other: NormalizedSelectionMap) {
            self.0.extend(other.0)
        }

        pub(crate) fn extend_ref(&mut self, other: &NormalizedSelectionMap) {
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
    pub(crate) fn from_normalized_field(
        field: NormalizedField,
        sub_selections: Option<NormalizedSelectionSet>,
    ) -> Self {
        let field_selection = NormalizedFieldSelection {
            field,
            selection_set: sub_selections,
        };
        Self::Field(Arc::new(field_selection))
    }

    pub(crate) fn from_normalized_inline_fragment(
        inline_fragment: NormalizedInlineFragment,
        sub_selections: NormalizedSelectionSet,
    ) -> Self {
        let inline_fragment_selection = NormalizedInlineFragmentSelection {
            inline_fragment,
            selection_set: sub_selections,
        };
        Self::InlineFragment(Arc::new(inline_fragment_selection))
    }

    pub(crate) fn from_element(
        element: OpPathElement,
        sub_selections: Option<NormalizedSelectionSet>,
    ) -> Result<Self, FederationError> {
        match element {
            OpPathElement::Field(field) => Ok(Self::from_normalized_field(field, sub_selections)),
            OpPathElement::InlineFragment(inline_fragment) => {
                let Some(sub_selections) = sub_selections else {
                    return Err(FederationError::internal(
                        "unexpected inline fragment without sub-selections",
                    ));
                };
                Ok(Self::from_normalized_inline_fragment(
                    inline_fragment,
                    sub_selections,
                ))
            }
        }
    }

    pub(crate) fn schema(&self) -> &ValidFederationSchema {
        match self {
            NormalizedSelection::Field(field_selection) => &field_selection.field.data().schema,
            NormalizedSelection::FragmentSpread(fragment_spread_selection) => {
                &fragment_spread_selection.spread.data().schema
            }
            NormalizedSelection::InlineFragment(inline_fragment_selection) => {
                &inline_fragment_selection.inline_fragment.data().schema
            }
        }
    }

    fn directives(&self) -> &Arc<DirectiveList> {
        match self {
            NormalizedSelection::Field(field_selection) => &field_selection.field.data().directives,
            NormalizedSelection::FragmentSpread(fragment_spread_selection) => {
                &fragment_spread_selection.spread.data().directives
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

    pub(crate) fn collect_variables<'selection>(
        &'selection self,
        variables: &mut HashSet<&'selection Name>,
    ) -> Result<(), FederationError> {
        match self {
            NormalizedSelection::Field(field) => field.collect_variables(variables),
            NormalizedSelection::InlineFragment(inline_fragment) => {
                inline_fragment.collect_variables(variables)
            }
            NormalizedSelection::FragmentSpread(_) => {
                Err(FederationError::internal("unexpected fragment spread"))
            }
        }
    }

    pub(crate) fn has_defer(&self) -> bool {
        match self {
            NormalizedSelection::Field(field_selection) => field_selection.has_defer(),
            NormalizedSelection::FragmentSpread(fragment_spread_selection) => {
                fragment_spread_selection.has_defer()
            }
            NormalizedSelection::InlineFragment(inline_fragment_selection) => {
                inline_fragment_selection.has_defer()
            }
        }
    }

    fn collect_used_fragment_names(&self, aggregator: &mut HashMap<Name, i32>) {
        match self {
            NormalizedSelection::Field(field_selection) => {
                if let Some(s) = field_selection.selection_set.clone() {
                    s.collect_used_fragment_names(aggregator)
                }
            }
            NormalizedSelection::InlineFragment(inline) => {
                inline.selection_set.collect_used_fragment_names(aggregator);
            }
            NormalizedSelection::FragmentSpread(fragment) => {
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
    ) -> Result<Option<NormalizedSelection>, FederationError> {
        match self {
            NormalizedSelection::Field(field) => {
                field.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
            NormalizedSelection::FragmentSpread(spread) => {
                spread.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
            NormalizedSelection::InlineFragment(inline) => {
                inline.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
        }
    }

    pub(crate) fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        option: NormalizeSelectionOption,
    ) -> Result<Option<NormalizedSelectionOrSet>, FederationError> {
        match self {
            NormalizedSelection::Field(field) => {
                field.normalize(parent_type, named_fragments, schema, option)
            }
            NormalizedSelection::FragmentSpread(spread) => {
                spread.normalize(parent_type, named_fragments, schema)
            }
            NormalizedSelection::InlineFragment(inline) => {
                inline.normalize(parent_type, named_fragments, schema, option)
            }
        }
    }

    pub(crate) fn with_updated_selection_set(
        &self,
        selection_set: Option<NormalizedSelectionSet>,
    ) -> Result<Self, FederationError> {
        match self {
            NormalizedSelection::Field(field) => Ok(NormalizedSelection::Field(Arc::new(
                field.with_updated_selection_set(selection_set),
            ))),
            NormalizedSelection::InlineFragment(inline_fragment) => {
                Ok(NormalizedSelection::InlineFragment(Arc::new(
                    inline_fragment.with_updated_selection_set(selection_set),
                )))
            }
            NormalizedSelection::FragmentSpread(_) => {
                Err(FederationError::internal("unexpected fragment spread"))
            }
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NormalizedSelectionOrSet {
    Selection(NormalizedSelection),
    SelectionSet(NormalizedSelectionSet),
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
    fn from_fragment(
        fragment: &Fragment,
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
            selection_set: NormalizedSelectionSet::from_selection_set(
                &fragment.selection_set,
                named_fragments,
                schema,
            )?,
        })
    }

    // PORT NOTE: in JS code this is stored on the fragment
    fn fragment_usages(&self) -> HashMap<Name, i32> {
        let mut usages = HashMap::new();
        self.selection_set.collect_used_fragment_names(&mut usages);
        usages
    }

    // PORT NOTE: in JS code this is stored on the fragment
    fn collect_used_fragment_names(&self, aggregator: &mut HashMap<Name, i32>) {
        self.selection_set.collect_used_fragment_names(aggregator)
    }

    fn has_defer(&self) -> bool {
        self.selection_set.has_defer()
    }
}

pub(crate) mod normalized_field_selection {
    use crate::error::FederationError;
    use crate::query_graph::graph_path::OpPathElement;
    use crate::query_plan::operation::{
        directives_with_sorted_arguments, HasNormalizedSelectionKey, NormalizedSelectionKey,
        NormalizedSelectionSet,
    };
    use crate::query_plan::FetchDataPathElement;
    use crate::schema::position::{FieldDefinitionPosition, TypeDefinitionPosition};
    use crate::schema::ValidFederationSchema;
    use apollo_compiler::ast::{Argument, Directive, DirectiveList, Name};
    use apollo_compiler::Node;
    use std::collections::HashSet;
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

    impl NormalizedFieldSelection {
        pub(crate) fn with_updated_selection_set(
            &self,
            selection_set: Option<NormalizedSelectionSet>,
        ) -> Self {
            Self {
                field: self.field.clone(),
                selection_set,
            }
        }

        pub(crate) fn element(&self) -> OpPathElement {
            OpPathElement::Field(self.field.clone())
        }

        pub(crate) fn with_updated_alias(&self, alias: Name) -> NormalizedField {
            let mut data = self.field.data().clone();
            data.alias = Some(alias);
            NormalizedField::new(data)
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

        pub(crate) fn sibling_typename(&self) -> Option<&Name> {
            self.data.sibling_typename.as_ref()
        }

        pub(crate) fn sibling_typename_mut(&mut self) -> &mut Option<Name> {
            &mut self.data.sibling_typename
        }

        pub(crate) fn with_updated_directives(&self, directives: DirectiveList) -> NormalizedField {
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
        argument: &'selection Argument,
        variables: &mut HashSet<&'selection Name>,
    ) {
        if let Some(v) = argument.value.as_variable() {
            variables.insert(v);
        }
    }

    pub(crate) fn collect_variables_from_directive<'selection>(
        directive: &'selection Directive,
        variables: &mut HashSet<&'selection Name>,
    ) {
        for arg in directive.arguments.iter() {
            collect_variables_from_argument(arg, variables)
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
        NormalizedSelectionKey, NormalizedSelectionSet, SelectionId,
    };
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use apollo_compiler::ast::{DirectiveList, Name};
    use std::sync::Arc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct NormalizedFragmentSpreadSelection {
        pub(crate) spread: NormalizedFragmentSpread,
        pub(crate) selection_set: NormalizedSelectionSet,
    }

    impl HasNormalizedSelectionKey for NormalizedFragmentSpreadSelection {
        fn key(&self) -> NormalizedSelectionKey {
            self.spread.key()
        }
    }

    /// An analogue of the apollo-compiler type `FragmentSpread` with these changes:
    /// - Stores the schema (may be useful for directives).
    /// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct NormalizedFragmentSpread {
        pub(crate) data: NormalizedFragmentSpreadData,
        pub(crate) key: NormalizedSelectionKey,
    }

    impl NormalizedFragmentSpread {
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

    impl HasNormalizedSelectionKey for NormalizedFragmentSpread {
        fn key(&self) -> NormalizedSelectionKey {
            self.key.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct NormalizedFragmentSpreadData {
        pub(crate) schema: ValidFederationSchema,
        pub(crate) fragment_name: Name,
        pub(crate) type_condition_position: CompositeTypeDefinitionPosition,
        // directives applied on the fragment spread selection
        pub(crate) directives: Arc<DirectiveList>,
        // directives applied within the fragment definition
        //
        // PORT_NOTE: The JS codebase combined the fragment spread's directives with the fragment
        // definition's directives. This was invalid GraphQL as those directives may not be applicable
        // on different locations. While we now keep track of those references, they are currently ignored.
        pub(crate) fragment_directives: Arc<DirectiveList>,
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

impl NormalizedFragmentSpreadSelection {
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<NormalizedSelection>, FederationError> {
        // We preserve the parent type here, to make sure we don't lose context, but we actually don't
        // want to expand the spread as that would compromise the code that optimize subgraph fetches to re-use named
        // fragments.
        //
        // This is a little bit iffy, because the fragment may not apply at this parent type, but we
        // currently leave it to the caller to ensure this is not a mistake. But most of the
        // QP code works on selections with fully expanded fragments, so this code (and that of `can_add_to`
        // on come into play in the code for reusing fragments, and that code calls those methods
        // appropriately.
        if self.spread.data.schema == *schema
            && self.spread.data().type_condition_position == *parent_type
        {
            return Ok(Some(NormalizedSelection::FragmentSpread(Arc::new(
                self.clone(),
            ))));
        }

        // If we're rebasing on a _different_ schema, then we *must* have fragments, since reusing
        // `self.fragments` would be incorrect. If we're on the same schema though, we're happy to default
        // to `self.fragments`.
        let rebase_on_same_schema = self.spread.data.schema == *schema;
        let Some(named_fragment) = named_fragments.get(&self.spread.data.fragment_name) else {
            // If we're rebasing on another schema (think a subgraph), then named fragments will have been rebased on that, and some
            // of them may not contain anything that is on that subgraph, in which case they will not have been included at all.
            // If so, then as long as we're not asked to error if we cannot rebase, then we're happy to skip that spread (since again,
            // it expands to nothing that applies on the schema).
            return if let RebaseErrorHandlingOption::ThrowError = error_handling {
                Err(FederationError::internal(format!(
                    "Cannot rebase {} fragment if it isn't part of the provided fragments",
                    self.spread.data.fragment_name
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
            // In theory, we could return the selection set directly, but making `NormalizedSelectionSet.rebase_on` sometimes
            // return a `NormalizedSelectionSet` complicate things quite a bit. So instead, we encapsulate the selection set
            // in an "empty" inline fragment. This make for non-really-optimal selection sets in the (relatively
            // rare) case where this is triggered, but in practice this "inefficiency" is removed by future calls
            // to `normalize`.
            return if expanded_selection_set.selections.is_empty() {
                Ok(None)
            } else {
                let inline_fragment_selection = NormalizedInlineFragmentSelection {
                    inline_fragment: NormalizedInlineFragment::new(NormalizedInlineFragmentData {
                        schema: schema.clone(),
                        parent_type_position: parent_type.clone(),
                        type_condition_position: None,
                        directives: Arc::new(DirectiveList::new()),
                        selection_id: SelectionId::new(),
                    }),
                    selection_set: expanded_selection_set,
                };
                Ok(Some(NormalizedSelection::InlineFragment(Arc::new(
                    inline_fragment_selection,
                ))))
            };
        }

        let spread = NormalizedFragmentSpread::new(NormalizedFragmentSpreadData::from_fragment(
            &named_fragment,
            &self.spread.data.directives,
        ));
        Ok(Some(NormalizedSelection::FragmentSpread(Arc::new(
            NormalizedFragmentSpreadSelection {
                spread,
                selection_set: named_fragment.selection_set.clone(),
            },
        ))))
    }

    pub(crate) fn has_defer(&self) -> bool {
        self.spread.data.directives.has("defer") || self.selection_set.has_defer()
    }

    /// Copies fragment spread selection and assigns it a new unique selection ID.
    pub(crate) fn with_unique_id(&self) -> Self {
        let mut data = self.spread.data().clone();
        data.selection_id = SelectionId::new();
        Self {
            spread: NormalizedFragmentSpread::new(data),
            selection_set: self.selection_set.clone(),
        }
    }

    /// Normalize this fragment spread into a "normalized" spread representation with following
    /// modifications
    /// - Stores the schema (may be useful for directives).
    /// - Encloses list of directives in `Arc`s to facilitate cheaper cloning.
    /// - Stores unique selection ID (used for deferred fragments)
    pub(crate) fn from_fragment_spread(
        fragment_spread: &FragmentSpread,
        fragment: &Node<NormalizedFragment>,
    ) -> Result<NormalizedFragmentSpreadSelection, FederationError> {
        let spread_data =
            NormalizedFragmentSpreadData::from_fragment(fragment, &fragment_spread.directives);
        Ok(NormalizedFragmentSpreadSelection {
            spread: NormalizedFragmentSpread::new(spread_data),
            selection_set: fragment.selection_set.clone(),
        })
    }

    pub(crate) fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<Option<NormalizedSelectionOrSet>, FederationError> {
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
        if self.spread.data.schema != *schema {
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
            Ok(Some(NormalizedSelectionOrSet::Selection(
                rebased_fragment_spread,
            )))
        } else {
            unreachable!("We should always be able to either rebase the fragment spread OR throw an exception");
        }
    }
}

impl NormalizedFragmentSpreadData {
    pub(crate) fn from_fragment(
        fragment: &Node<NormalizedFragment>,
        spread_directives: &DirectiveList,
    ) -> NormalizedFragmentSpreadData {
        NormalizedFragmentSpreadData {
            schema: fragment.schema.clone(),
            fragment_name: fragment.name.clone(),
            type_condition_position: fragment.type_condition_position.clone(),
            directives: Arc::new(spread_directives.clone()),
            fragment_directives: fragment.directives.clone(),
            selection_id: SelectionId::new(),
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
    use crate::query_plan::FetchDataPathElement;
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use apollo_compiler::ast::{DirectiveList, Name};
    use std::sync::Arc;

    use super::normalized_field_selection::collect_variables_from_directive;
    use std::collections::HashSet;

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

    impl NormalizedInlineFragmentSelection {
        pub(crate) fn with_updated_selection_set(
            &self,
            selection_set: Option<NormalizedSelectionSet>,
        ) -> Self {
            Self {
                inline_fragment: self.inline_fragment.clone(),
                //FIXME
                selection_set: selection_set.unwrap(),
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
            directives: DirectiveList,
        ) -> NormalizedInlineFragment {
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

impl NormalizedOperation {
    pub(crate) fn optimize(
        &mut self,
        fragments: Option<&NamedFragments>,
        min_usages_to_optimize: Option<u32>,
    ) {
        let min_usages_to_optimize = min_usages_to_optimize.unwrap_or(2);
        let Some(fragments) = fragments else { return };
        if fragments.is_empty() {
            return;
        }
        assert!(
            min_usages_to_optimize >= 1,
            "Expected 'min_usages_to_optimize' to be at least 1, but got {min_usages_to_optimize}"
        );

        todo!(); // TODO: port JS `Operation.optimize` from `operations.ts`
    }
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

    pub(crate) fn from_selection(
        type_position: CompositeTypeDefinitionPosition,
        selection: NormalizedSelection,
    ) -> Self {
        let schema = selection.schema();
        let mut selection_map = NormalizedSelectionMap::new();
        selection_map.insert(selection.clone());
        Self {
            schema: schema.clone(),
            type_position,
            selections: Arc::new(selection_map),
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
    pub(crate) fn from_selection_set(
        selection_set: &SelectionSet,
        fragments: &NamedFragments,
        schema: &ValidFederationSchema,
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
        )?;
        let mut merged = NormalizedSelectionSet {
            schema: schema.clone(),
            type_position,
            selections: Arc::new(NormalizedSelectionMap::new()),
        };
        merged.merge_selections_into(normalized_selections.iter())?;
        Ok(merged)
    }

    /// A helper function for normalizing a list of selections into a destination.
    fn normalize_selections(
        selections: &[Selection],
        parent_type_position: &CompositeTypeDefinitionPosition,
        destination: &mut Vec<NormalizedSelection>,
        fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<(), FederationError> {
        for selection in selections {
            match selection {
                Selection::Field(field_selection) => {
                    let Some(normalized_field_selection) = NormalizedFieldSelection::from_field(
                        field_selection,
                        parent_type_position,
                        fragments,
                        schema,
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
                    // if we don't expand fragments, we need to normalize it
                    let normalized_fragment_spread =
                        NormalizedFragmentSpreadSelection::from_fragment_spread(
                            fragment_spread_selection,
                            &fragment,
                        )?;
                    destination.push(NormalizedSelection::FragmentSpread(Arc::new(
                        normalized_fragment_spread,
                    )));
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
                        )?;
                    } else {
                        let normalized_inline_fragment_selection =
                            NormalizedInlineFragmentSelection::from_inline_fragment(
                                inline_fragment_selection,
                                parent_type_position,
                                fragments,
                                schema,
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
    pub(crate) fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op NormalizedSelectionSet>,
    ) -> Result<(), FederationError> {
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
            selections_to_merge.extend(other.selections.values());
        }
        self.merge_selections_into(selections_to_merge.into_iter())
    }

    /// A helper function for merging the given selections into this one.
    fn merge_selections_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op NormalizedSelection>,
    ) -> Result<(), FederationError> {
        let mut fields = IndexMap::new();
        let mut fragment_spreads = IndexMap::new();
        let mut inline_fragments = IndexMap::new();
        let target = Arc::make_mut(&mut self.selections);
        for other_selection in others {
            let other_key = other_selection.key();
            match target.entry(other_key.clone()) {
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
                        fields
                            .entry(other_key)
                            .or_insert_with(Vec::new)
                            .push(other_field_selection);
                    }
                    NormalizedSelection::FragmentSpread(self_fragment_spread_selection) => {
                        let NormalizedSelection::FragmentSpread(other_fragment_spread_selection) =
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
                    NormalizedSelection::InlineFragment(self_inline_fragment_selection) => {
                        let NormalizedSelection::InlineFragment(other_inline_fragment_selection) =
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
                Entry::Vacant(vacant) => {
                    vacant.insert(other_selection.clone())?;
                }
            }
        }

        for (key, self_selection) in target.iter_mut() {
            match self_selection {
                NormalizedSelectionValue::Field(mut self_field_selection) => {
                    if let Some(other_field_selections) = fields.shift_remove(key) {
                        self_field_selection.merge_into(
                            other_field_selections.iter().map(|selection| &***selection),
                        )?;
                    }
                }
                NormalizedSelectionValue::FragmentSpread(mut self_fragment_spread_selection) => {
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
                NormalizedSelectionValue::InlineFragment(mut self_inline_fragment_selection) => {
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

    pub(crate) fn expand_all_fragments(&self) -> Result<NormalizedSelectionSet, FederationError> {
        let mut expanded_selections = vec![];
        NormalizedSelectionSet::expand_selection_set(&mut expanded_selections, self)?;

        let mut expanded = NormalizedSelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(NormalizedSelectionMap::new()),
        };
        expanded.merge_selections_into(expanded_selections.iter())?;
        Ok(expanded)
    }

    fn expand_selection_set(
        destination: &mut Vec<NormalizedSelection>,
        selection_set: &NormalizedSelectionSet,
    ) -> Result<(), FederationError> {
        for (_, value) in selection_set.selections.iter() {
            match value {
                NormalizedSelection::Field(field_selection) => {
                    let selections = match &field_selection.selection_set {
                        Some(s) => Some(s.expand_all_fragments()?),
                        None => None,
                    };
                    let expanded_selection = NormalizedFieldSelection {
                        field: field_selection.field.clone(),
                        selection_set: selections,
                    };
                    destination.push(NormalizedSelection::Field(Arc::new(expanded_selection)))
                }
                NormalizedSelection::FragmentSpread(spread_selection) => {
                    let fragment_spread_data = spread_selection.spread.data();
                    // We can hoist/collapse named fragments if their type condition is on the
                    // parent type and they don't have any directives.
                    if fragment_spread_data.type_condition_position == selection_set.type_position
                        && fragment_spread_data.directives.is_empty()
                    {
                        NormalizedSelectionSet::expand_selection_set(
                            destination,
                            &spread_selection.selection_set,
                        )?;
                    } else {
                        // convert to inline fragment
                        let expanded =
                            NormalizedInlineFragmentSelection::from_fragment_spread_selection(
                                spread_selection,
                            )?;
                        destination.push(NormalizedSelection::InlineFragment(Arc::new(expanded)));
                    }
                }
                NormalizedSelection::InlineFragment(inline_selection) => {
                    let expanded_selection = NormalizedInlineFragmentSelection {
                        inline_fragment: inline_selection.inline_fragment.clone(),
                        selection_set: inline_selection.selection_set.expand_all_fragments()?,
                    };
                    destination.push(NormalizedSelection::InlineFragment(Arc::new(
                        expanded_selection,
                    )));
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
                            fragment_spread.get().spread.data().fragment_name
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
        parent_type_if_abstract: Option<AbstractType>,
        fragments: &Option<&mut RebasedFragments>,
    ) -> Result<NormalizedSelectionSet, FederationError> {
        let mut selection_map = NormalizedSelectionMap::new();
        if let Some(parent) = parent_type_if_abstract {
            if !self.has_top_level_typename_field() {
                let field_position = parent.introspection_typename_field();
                let typename_selection = NormalizedFieldSelection {
                    field: NormalizedField::new(NormalizedFieldData {
                        schema: self.schema.clone(),
                        field_position,
                        alias: None,
                        arguments: Default::default(),
                        directives: Default::default(),
                        sibling_typename: None,
                    }),
                    selection_set: None,
                };
                selection_map.insert(NormalizedSelection::Field(Arc::new(typename_selection)));
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

        Ok(NormalizedSelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(selection_map),
        })
    }

    fn has_top_level_typename_field(&self) -> bool {
        todo!()
    }

    pub(crate) fn add_at_path(
        &mut self,
        path: &OpPath,
        selection_set: Option<&Arc<NormalizedSelectionSet>>,
    ) {
        Arc::make_mut(&mut self.selections).add_at_path(path, selection_set)
    }

    fn collect_used_fragment_names(&self, aggregator: &mut HashMap<Name, i32>) {
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
    ) -> Result<NormalizedSelectionSet, FederationError> {
        let mut rebased_selections = NormalizedSelectionMap::new();
        let rebased_results: Result<Vec<Option<NormalizedSelection>>, FederationError> = self
            .selections
            .iter()
            .map(|(_, selection)| {
                selection.rebase_on(parent_type, named_fragments, schema, error_handling)
            })
            .collect();
        for rebased in rebased_results?.iter().flatten() {
            rebased_selections.insert(rebased.clone());
        }
        Ok(NormalizedSelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(rebased_selections),
        })
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
    ) -> Result<NormalizedSelectionSet, FederationError> {
        let mut normalized_selection_map = NormalizedSelectionMap::new();
        for (_, selection) in self.selections.iter() {
            if let Some(selection_or_set) =
                selection.normalize(parent_type, named_fragments, schema, option)?
            {
                match selection_or_set {
                    NormalizedSelectionOrSet::Selection(normalized_selection) => {
                        normalized_selection_map.insert(normalized_selection);
                    }
                    NormalizedSelectionOrSet::SelectionSet(normalized_set) => {
                        normalized_selection_map.extend_ref(&normalized_set.selections);
                    }
                }
            }
        }

        Ok(NormalizedSelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(normalized_selection_map),
        })
    }

    fn has_defer(&self) -> bool {
        self.selections.values().any(|s| s.has_defer())
    }

    pub(crate) fn add_aliases_for_non_merging_fields(
        &self,
    ) -> Result<(NormalizedSelectionSet, Vec<Arc<FetchDataRewrite>>), FederationError> {
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
    ) -> Result<NormalizedSelectionSet, FederationError> {
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

        let mut selection_map = NormalizedSelectionMap::new();
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
                NormalizedSelection::Field(field) => {
                    let alias = path_element.and_then(|elem| at_current_level.get(&elem));
                    if alias.is_none() && selection_set == updated_selection_set.as_ref() {
                        selection_map.insert(selection.clone());
                    } else {
                        let sel = NormalizedFieldSelection {
                            field: match alias {
                                Some(alias) => field.with_updated_alias(alias.alias.clone()),
                                None => field.field.clone(),
                            },
                            selection_set: updated_selection_set,
                        };
                        selection_map.insert(NormalizedSelection::Field(Arc::new(sel)));
                    }
                }
                NormalizedSelection::InlineFragment(_) => {
                    if selection_set == updated_selection_set.as_ref() {
                        selection_map.insert(selection.clone());
                    } else {
                        selection_map
                            .insert(selection.with_updated_selection_set(updated_selection_set)?);
                    }
                }
                NormalizedSelection::FragmentSpread(_) => {
                    return Err(FederationError::internal("unexpected fragment spread"))
                }
            }
        }

        Ok(NormalizedSelectionSet {
            schema: self.schema.clone(),
            type_position: self.type_position.clone(),
            selections: Arc::new(selection_map),
        })
    }

    pub(crate) fn fields_in_set(&self) -> Vec<CollectedFieldInSet> {
        let mut fields = Vec::new();

        for (_key, selection) in self.selections.iter() {
            match selection {
                NormalizedSelection::Field(field) => fields.push(CollectedFieldInSet {
                    path: Vec::new(),
                    field: field.clone(),
                }),
                NormalizedSelection::FragmentSpread(_fragment) => {
                    todo!()
                }
                NormalizedSelection::InlineFragment(inline_fragment) => {
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
        _variable_definitions: &[Node<VariableDefinition>],
    ) -> Result<(), FederationError> {
        if self.selections.is_empty() {
            Err(SingleFederationError::Internal {
                message: "Invalid empty selection set".to_string(),
            }
            .into())
        } else {
            for selection in self.selections.values() {
                if let Some(s) = selection.selection_set()? {
                    s.validate(_variable_definitions)?;
                }
            }

            Ok(())
        }
    }
}

#[derive(Clone)]
pub(crate) struct SelectionSetAtPath {
    path: Vec<FetchDataPathElement>,
    selections: Option<NormalizedSelectionSet>,
}

pub(crate) struct FieldToAlias {
    path: Vec<FetchDataPathElement>,
    response_name: NodeStr,
    alias: Name,
}

pub(crate) struct SeenResponseName {
    field_name: Name,
    field_type: Type,
    selections: Option<Vec<SelectionSetAtPath>>,
}

pub(crate) struct CollectedFieldInSet {
    path: Vec<FetchDataPathElement>,
    field: Arc<NormalizedFieldSelection>,
}

struct FieldInPath {
    path: Vec<FetchDataPathElement>,
    field: Arc<NormalizedFieldSelection>,
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
    selection: &NormalizedSelection,
    schema: &ValidFederationSchema,
    fragments: &Option<&mut RebasedFragments>,
) -> Option<AbstractType> {
    match selection {
        NormalizedSelection::Field(field) => {
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
        NormalizedSelection::FragmentSpread(fragment_spread) => {
            let fragment = fragments
                .as_ref()
                .and_then(|r| {
                    r.original_fragments
                        .get(&fragment_spread.spread.data().fragment_name)
                })
                .ok_or(FederationError::SingleFederationError(
                    crate::error::SingleFederationError::InvalidGraphQL {
                        message: "missing fragment".to_string(),
                    },
                ))
                //FIXME: return error
                .ok()?;
            match fragment.type_condition_position.clone() {
                CompositeTypeDefinitionPosition::Interface(i) => Some(AbstractType::Interface(i)),
                CompositeTypeDefinitionPosition::Union(u) => Some(AbstractType::Union(u)),
                CompositeTypeDefinitionPosition::Object(_) => None,
            }
        }
        NormalizedSelection::InlineFragment(inline_fragment) => {
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

impl From<NormalizedSelectionSet> for executable::SelectionSet {
    fn from(_value: NormalizedSelectionSet) -> Self {
        todo!()
    }
}

impl NormalizedSelectionMap {
    /// Adds a path, and optional some selections following that path, to those updates.
    ///
    /// The final selections are optional (for instance, if `path` ends on a leaf field,
    /// then no followup selections would make sense),
    /// but when some are provided, uncesssary fragments will be automaticaly removed
    /// at the junction between the path and those final selections.
    /// For instance, suppose that we have:
    ///  - a `path` argument that is `a::b::c`,
    ///    where the type of the last field `c` is some object type `C`.
    ///  - a `selections` argument that is `{ ... on C { d } }`.
    /// Then the resulting built selection set will be: `{ a { b { c { d } } }`,
    /// and in particular the `... on C` fragment will be eliminated since it is unecesasry
    /// (since again, `c` is of type `C`).
    pub(crate) fn add_at_path(
        &mut self,
        _path: &OpPath,
        _selection_set: Option<&Arc<NormalizedSelectionSet>>,
    ) {
        // TODO: port a `SelectionSetUpdates` data structure or mutate directly?
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
    pub(crate) fn from_field(
        field: &Field,
        parent_type_position: &CompositeTypeDefinitionPosition,
        fragments: &NamedFragments,
        schema: &ValidFederationSchema,
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
                Some(NormalizedSelectionSet::from_selection_set(
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
    ) -> Result<Option<NormalizedSelectionOrSet>, FederationError> {
        if let Some(selection_set) = &self.selection_set {
            let mut normalized_selection: NormalizedSelectionSet =
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
                let directives = DirectiveList(vec![Node::new(Directive {
                    name: name!("include"),
                    arguments: vec![Node::new(Argument {
                        name: name!("if"),
                        value: Node::new(Value::Boolean(false)),
                    })],
                })]);
                let non_included_typename =
                    NormalizedSelection::Field(Arc::new(NormalizedFieldSelection {
                        field: NormalizedField::new(NormalizedFieldData {
                            schema: schema.clone(),
                            field_position: parent_type.introspection_typename_field(),
                            alias: None,
                            arguments: Arc::new(vec![]),
                            directives: Arc::new(directives),
                            sibling_typename: None,
                        }),
                        selection_set: None,
                    }));
                let mut typename_selection = NormalizedSelectionMap::new();
                typename_selection.insert(non_included_typename);

                normalized_selection.selections = Arc::new(typename_selection);
                selection.selection_set = Some(normalized_selection);
            } else {
                selection.selection_set = Some(normalized_selection);
            }
            Ok(Some(NormalizedSelectionOrSet::Selection(
                NormalizedSelection::Field(Arc::new(selection)),
            )))
        } else {
            // JS PORT NOTE: In JS implementation field selection stores field definition information,
            // in RS version we only store the field position reference so we don't need to update the
            // underlying elements
            Ok(Some(NormalizedSelectionOrSet::Selection(
                NormalizedSelection::Field(Arc::new(self.clone())),
            )))
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
    ) -> Result<Option<NormalizedSelection>, FederationError> {
        if &self.field.data().schema == schema
            && &self.field.data().field_position.parent() == parent_type
        {
            // we are rebasing field on the same parent within the same schema - we can just return self
            return Ok(Some(NormalizedSelection::Field(Arc::new(self.clone()))));
        }

        let Some(rebased) = self.field.rebase_on(parent_type, schema, error_handling)? else {
            // rebasing failed but we are ignoring errors
            return Ok(None);
        };

        let Some(selection_set) = &self.selection_set else {
            // leaf field
            return Ok(Some(NormalizedSelection::Field(Arc::new(
                NormalizedFieldSelection {
                    field: rebased,
                    selection_set: None,
                },
            ))));
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
            return Ok(Some(NormalizedSelection::Field(Arc::new(
                NormalizedFieldSelection {
                    field: rebased.clone(),
                    selection_set: self.selection_set.clone(),
                },
            ))));
        }

        let rebased_selection_set =
            selection_set.rebase_on(&rebased_base_type, named_fragments, schema, error_handling)?;
        if rebased_selection_set.selections.is_empty() {
            // empty selection set
            Ok(None)
        } else {
            Ok(Some(NormalizedSelection::Field(Arc::new(
                NormalizedFieldSelection {
                    field: rebased.clone(),
                    selection_set: Some(rebased_selection_set),
                },
            ))))
        }
    }

    pub(crate) fn has_defer(&self) -> bool {
        self.field.has_defer() || self.selection_set.as_ref().is_some_and(|s| s.has_defer())
    }
}

impl<'a> NormalizedFieldSelectionValue<'a> {
    /// Merges the given normalized field selections into this one (this method assumes the keys
    /// already match).
    pub(crate) fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op NormalizedFieldSelection>,
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

impl NormalizedField {
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<NormalizedField>, FederationError> {
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
                Ok(Some(NormalizedField::new(updated_field_data)))
            };
        }

        let field_from_parent = parent_type.field(self.data().name().clone())?;
        return if field_from_parent.get(schema.schema()).is_ok()
            && self.can_rebase_on(parent_type, schema)
        {
            let mut updated_field_data = self.data().clone();
            updated_field_data.schema = schema.clone();
            updated_field_data.field_position = field_from_parent;
            Ok(Some(NormalizedField::new(updated_field_data)))
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
}

impl<'a> NormalizedFragmentSpreadSelectionValue<'a> {
    /// Merges the given normalized fragment spread selections into this one (this method assumes
    /// the keys already match).
    pub(crate) fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op NormalizedFragmentSpreadSelection>,
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
    pub(crate) fn from_inline_fragment(
        inline_fragment: &InlineFragment,
        parent_type_position: &CompositeTypeDefinitionPosition,
        fragments: &NamedFragments,
        schema: &ValidFederationSchema,
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
            selection_set: NormalizedSelectionSet::from_selection_set(
                &inline_fragment.selection_set,
                fragments,
                schema,
            )?,
        })
    }

    pub(crate) fn from_fragment_spread_selection(
        fragment_spread_selection: &Arc<NormalizedFragmentSpreadSelection>,
    ) -> Result<NormalizedInlineFragmentSelection, FederationError> {
        let fragment_spread_data = fragment_spread_selection.spread.data();
        Ok(NormalizedInlineFragmentSelection {
            inline_fragment: NormalizedInlineFragment::new(NormalizedInlineFragmentData {
                schema: fragment_spread_data.schema.clone(),
                parent_type_position: fragment_spread_data.type_condition_position.clone(),
                type_condition_position: Some(fragment_spread_data.type_condition_position.clone()),
                directives: fragment_spread_data.directives.clone(),
                selection_id: SelectionId::new(),
            }),
            selection_set: fragment_spread_selection
                .selection_set
                .expand_all_fragments()?,
        })
    }

    pub(crate) fn normalize(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        option: NormalizeSelectionOption,
    ) -> Result<Option<NormalizedSelectionOrSet>, FederationError> {
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
                    Ok(Some(NormalizedSelectionOrSet::SelectionSet(
                        normalized_selection_set,
                    )))
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
                    let directives = DirectiveList(vec![Node::new(Directive {
                        name: name!("include"),
                        arguments: vec![Node::new(Argument {
                            name: name!("if"),
                            value: Node::new(Value::Boolean(false)),
                        })],
                    })]);
                    let parent_typename_field = if let Some(condition) = this_condition {
                        condition.introspection_typename_field()
                    } else {
                        parent_type.introspection_typename_field()
                    };
                    let typename_field_selection =
                        NormalizedSelection::Field(Arc::new(NormalizedFieldSelection {
                            field: NormalizedField::new(NormalizedFieldData {
                                schema: schema.clone(),
                                field_position: parent_typename_field,
                                alias: None,
                                arguments: Arc::new(vec![]),
                                directives: Arc::new(directives),
                                sibling_typename: None,
                            }),
                            selection_set: None,
                        }));
                    let mut normalized_selection = NormalizedSelectionMap::new();
                    normalized_selection.insert(typename_field_selection);

                    return Ok(Some(NormalizedSelectionOrSet::Selection(
                        NormalizedSelection::InlineFragment(Arc::new(
                            NormalizedInlineFragmentSelection {
                                inline_fragment: rebased_fragment,
                                selection_set: NormalizedSelectionSet {
                                    schema: schema.clone(),
                                    type_position: parent_type.clone(),
                                    selections: Arc::new(normalized_selection),
                                },
                            },
                        )),
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
            let mut liftable_selections = NormalizedSelectionMap::new();
            for (_, selection) in normalized_selection_set.selections.iter() {
                match selection {
                    NormalizedSelection::FragmentSpread(spread_selection) => {
                        let type_condition =
                            spread_selection.spread.data.type_condition_position.clone();
                        if type_condition.is_object_type()
                            && runtime_types_intersect(parent_type, &type_condition, schema)
                        {
                            liftable_selections.insert(NormalizedSelection::FragmentSpread(
                                spread_selection.clone(),
                            ));
                        }
                    }
                    NormalizedSelection::InlineFragment(inline_fragment_selection) => {
                        if let Some(type_condition) = inline_fragment_selection
                            .inline_fragment
                            .data()
                            .type_condition_position
                            .clone()
                        {
                            if type_condition.is_object_type()
                                && runtime_types_intersect(parent_type, &type_condition, schema)
                            {
                                liftable_selections.insert(NormalizedSelection::InlineFragment(
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
                return Ok(Some(NormalizedSelectionOrSet::SelectionSet(
                    normalized_selection_set,
                )));
            }

            // Otherwise, if there are "liftable" selections, we must return a set comprised of those lifted selection,
            // and the current fragment _without_ those lifted selections.
            if liftable_selections.len() > 0 {
                let mut mutable_selections = self.selection_set.selections.clone();
                let final_fragment_selections = Arc::make_mut(&mut mutable_selections);
                final_fragment_selections.retain(|k, _| !liftable_selections.contains_key(k));
                let final_inline_fragment = NormalizedInlineFragmentSelection {
                    inline_fragment: self.inline_fragment.clone(),
                    selection_set: NormalizedSelectionSet {
                        selections: Arc::new(final_fragment_selections.clone()),
                        schema: schema.clone(),
                        type_position: parent_type.clone(),
                    },
                };

                let mut final_selection_map = NormalizedSelectionMap::new();
                final_selection_map.insert(NormalizedSelection::InlineFragment(Arc::new(
                    final_inline_fragment,
                )));
                final_selection_map.extend(liftable_selections);
                let final_selections = NormalizedSelectionSet {
                    schema: schema.clone(),
                    type_position: parent_type.clone(),
                    selections: final_selection_map.into(),
                };
                return Ok(Some(NormalizedSelectionOrSet::SelectionSet(
                    final_selections,
                )));
            }
        }

        if self.inline_fragment.data().schema == *schema
            && self.inline_fragment.data().parent_type_position == *parent_type
            && self.selection_set == normalized_selection_set
        {
            // normalization did not change the fragment
            Ok(Some(NormalizedSelectionOrSet::Selection(
                NormalizedSelection::InlineFragment(Arc::new(self.clone())),
            )))
        } else if let Some(rebased) = self.inline_fragment.rebase_on(
            parent_type,
            schema,
            RebaseErrorHandlingOption::ThrowError,
        )? {
            Ok(Some(NormalizedSelectionOrSet::Selection(
                NormalizedSelection::InlineFragment(Arc::new(NormalizedInlineFragmentSelection {
                    inline_fragment: rebased,
                    selection_set: normalized_selection_set,
                })),
            )))
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
    ) -> Result<Option<NormalizedSelection>, FederationError> {
        if &self.inline_fragment.data().schema == schema
            && self.inline_fragment.data().parent_type_position == *parent_type
        {
            // we are rebasing inline fragment on the same parent within the same schema - we can just return self
            return Ok(Some(NormalizedSelection::InlineFragment(Arc::new(
                self.clone(),
            ))));
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
            Ok(Some(NormalizedSelection::InlineFragment(Arc::new(
                NormalizedInlineFragmentSelection {
                    inline_fragment: rebased_fragment,
                    selection_set: self.selection_set.clone(),
                },
            ))))
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
                Ok(Some(NormalizedSelection::InlineFragment(Arc::new(
                    NormalizedInlineFragmentSelection {
                        inline_fragment: rebased_fragment,
                        selection_set: rebased_selection_set,
                    },
                ))))
            }
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
}

impl<'a> NormalizedInlineFragmentSelectionValue<'a> {
    /// Merges the given normalized inline fragment selections into this one (this method assumes
    /// the keys already match).
    pub(crate) fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op NormalizedInlineFragmentSelection>,
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

impl NormalizedInlineFragment {
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<NormalizedInlineFragment>, FederationError> {
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
            Ok(Some(NormalizedInlineFragment::new(rebased_fragment_data)))
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
    mut selection_sets: Vec<NormalizedSelectionSet>,
) -> Result<NormalizedSelectionSet, FederationError> {
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
    fragments: Arc<IndexMap<Name, Node<NormalizedFragment>>>,
}

impl NamedFragments {
    pub(crate) fn new(
        fragments: &IndexMap<Name, Node<Fragment>>,
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

    pub(crate) fn insert(&mut self, fragment: NormalizedFragment) {
        Arc::make_mut(&mut self.fragments).insert(fragment.name.clone(), Node::new(fragment));
    }

    pub(crate) fn try_insert(
        &mut self,
        fragment: NormalizedFragment,
    ) -> Result<(), FederationError> {
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

    pub(crate) fn get(&self, name: &Name) -> Option<Node<NormalizedFragment>> {
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
        fragments: &IndexMap<Name, Node<Fragment>>,
        schema: &ValidFederationSchema,
    ) -> NamedFragments {
        struct FragmentDependencies {
            fragment: Node<Fragment>,
            depends_on: Vec<Name>,
        }

        let mut fragments_map: HashMap<Name, FragmentDependencies> = HashMap::new();
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
                        NormalizedFragment::from_fragment(&info.fragment, &mapped_fragments, schema)
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

    // JS PORT - we need to calculate those for both SelectionSet and NormalizedSelectionSet
    fn collect_fragment_usages(selection_set: &SelectionSet, aggregator: &mut HashMap<Name, i32>) {
        selection_set.selections.iter().for_each(|s| match s {
            Selection::Field(f) => {
                NamedFragments::collect_fragment_usages(&f.selection_set, aggregator);
            }
            Selection::InlineFragment(i) => {
                NamedFragments::collect_fragment_usages(&i.selection_set, aggregator);
            }
            Selection::FragmentSpread(f) => {
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
    pub(crate) fn is_selection_set_worth_using(selection_set: &NormalizedSelectionSet) -> bool {
        if selection_set.selections.len() == 0 {
            return false;
        }
        if selection_set.selections.len() == 1 {
            // true if NOT field selection OR non-leaf field
            return if let Some((_, NormalizedSelection::Field(field_selection))) =
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
                        let fragment = NormalizedFragment {
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
}

#[derive(Clone)]
pub(crate) struct RebasedFragments {
    pub(crate) original_fragments: NamedFragments,
    // JS PORT NOTE: In JS implementation values were optional
    /// Map key: subgraph name
    rebased_fragments: Arc<HashMap<NodeStr, NamedFragments>>,
}

impl RebasedFragments {
    pub(crate) fn new(fragments: &NamedFragments) -> Self {
        Self {
            original_fragments: fragments.clone(),
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

impl TryFrom<&NormalizedFragment> for Fragment {
    type Error = FederationError;

    fn try_from(normalized_fragment: &NormalizedFragment) -> Result<Self, Self::Error> {
        Ok(Self {
            name: normalized_fragment.name.clone(),
            directives: normalized_fragment.directives.deref().clone(),
            selection_set: (&normalized_fragment.selection_set).try_into()?,
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

impl TryFrom<NormalizedOperation> for Valid<executable::ExecutableDocument> {
    type Error = FederationError;

    fn try_from(_value: NormalizedOperation) -> Result<Self, Self::Error> {
        todo!()
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

impl Display for NormalizedFragment {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let fragment: Fragment = match self.try_into() {
            Ok(fragment) => fragment,
            Err(_) => return Err(std::fmt::Error),
        };
        fragment.serialize().fmt(f)
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
    let named_fragments = NamedFragments::new(fragments, schema);
    let mut normalized_selection_set = NormalizedSelectionSet::from_selection_set(
        &operation.selection_set,
        &named_fragments,
        schema,
    )?;
    normalized_selection_set = normalized_selection_set.expand_all_fragments()?;
    normalized_selection_set.optimize_sibling_typenames(interface_types_with_interface_objects)?;

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
    use crate::schema::position::InterfaceTypeDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use crate::{query_plan::operation::normalize_operation, subgraph::Subgraph};
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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
                &executable_document.fragments,
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

    //
    // REBASE TESTS
    //
    #[cfg(test)]
    mod rebase_tests {
        use crate::query_plan::operation::normalize_operation;
        use crate::query_plan::operation::tests::{parse_schema_and_operation, parse_subgraph};
        use crate::schema::position::InterfaceTypeDefinitionPosition;
        use apollo_compiler::name;
        use indexmap::IndexSet;

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
                    &executable_document.fragments,
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
                    &executable_document.fragments,
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
                    &executable_document.fragments,
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
                    &executable_document.fragments,
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
                    &executable_document.fragments,
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
                    &executable_document.fragments,
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
                    &executable_document.fragments,
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
}
