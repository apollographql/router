use crate::error::FederationError;
use crate::error::SingleFederationError::Internal;
use crate::schema::position::{
    CompositeTypeDefinitionPosition, FieldDefinitionPosition, SchemaRootDefinitionKind,
};
use crate::schema::ValidFederationSchema;
use apollo_compiler::ast::{Argument, DirectiveList, Name};
use apollo_compiler::executable::{
    Field, Fragment, FragmentSpread, InlineFragment, Operation, Selection, SelectionSet,
    VariableDefinition,
};
use apollo_compiler::Node;
use indexmap::IndexMap;
use linked_hash_map::{Entry, LinkedHashMap};
use std::ops::Deref;
use std::sync::Arc;

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
    pub(crate) fragments: Arc<IndexMap<Name, Node<Fragment>>>,
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

/// A "normalized" selection map is an optimized representation of a selection set which does not
/// contain selections with the same selection "key". Selections that do have the same key are
/// merged during the normalization process. By storing a selection set as a map, we can efficiently
/// merge/join multiple selection sets.
///
/// Note that this must be a `LinkedHashMap` so that removals don't change the order.
pub(crate) type NormalizedSelectionMap = LinkedHashMap<NormalizedSelectionKey, NormalizedSelection>;

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
        // field alias (if specified) or field name in the resulting selection set
        response_name: Name,
        // directives applied on the field
        directives: Arc<DirectiveList>,
        // unique label/counter used to distinguish fields that cannot be merged
        label: i32,
    },
    FragmentSpread {
        // fragment name
        name: Name,
        // directives applied on the fragment spread
        directives: Arc<DirectiveList>,
        // unique label/counter used to distinguish fields that cannot be merged
        label: i32,
    },
    InlineFragment {
        // optional type condition of a fragment
        type_condition: Option<Name>,
        // directives applied on a fragment
        directives: Arc<DirectiveList>,
        // unique label/counter used to distinguish fragments that cannot be merged
        label: i32,
    },
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
            NormalizedSelection::Field(field_selection) => &field_selection.field.directives,
            NormalizedSelection::FragmentSpread(fragment_spread_selection) => {
                &fragment_spread_selection.directives
            }
            NormalizedSelection::InlineFragment(inline_fragment_selection) => {
                &inline_fragment_selection.inline_fragment.directives
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

/// The non-selection-set data of `NormalizedFieldSelection`, used with operation paths and graph
/// paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct NormalizedField {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) field_position: FieldDefinitionPosition,
    pub(crate) alias: Option<Name>,
    pub(crate) arguments: Arc<Vec<Node<Argument>>>,
    pub(crate) directives: Arc<DirectiveList>,
}

impl NormalizedField {
    fn name(&self) -> &Name {
        self.field_position.field_name()
    }

    fn response_name(&self) -> Name {
        self.alias.clone().unwrap_or_else(|| self.name().clone())
    }
}

/// An analogue of the apollo-compiler type `FragmentSpread` with these changes:
/// - Stores the schema (may be useful for directives).
/// - Encloses collection types in `Arc`s to facilitate cheaper cloning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedFragmentSpreadSelection {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) fragment_name: Name,
    pub(crate) directives: Arc<DirectiveList>,
}

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

/// The non-selection-set data of `NormalizedInlineFragmentSelection`, used with operation paths and
/// graph paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct NormalizedInlineFragment {
    pub(crate) schema: ValidFederationSchema,
    pub(crate) parent_type_position: CompositeTypeDefinitionPosition,
    pub(crate) type_condition_position: Option<CompositeTypeDefinitionPosition>,
    pub(crate) directives: Arc<DirectiveList>,
}

impl NormalizedSelectionSet {
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
            selections: Arc::new(LinkedHashMap::new()),
        };
        merged.merge_pairs_into(normalized_selections)?;
        Ok(merged)
    }

    /// A helper function for normalizing a list of selections into a destination.
    fn normalize_selections(
        selections: &[Selection],
        parent_type_position: &CompositeTypeDefinitionPosition,
        destination: &mut Vec<(NormalizedSelectionKey, NormalizedSelection)>,
        fragments: &IndexMap<Name, Node<Fragment>>,
        schema: &ValidFederationSchema,
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
                        )?
                    else {
                        continue;
                    };
                    let key: NormalizedSelectionKey = (&normalized_field_selection).into();
                    destination.push((
                        key,
                        NormalizedSelection::Field(Arc::new(normalized_field_selection)),
                    ));
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
                        )?;
                    } else {
                        let normalized_inline_fragment_selection =
                            NormalizedFragmentSpreadSelection::normalize_and_expand_fragments(
                                fragment_spread_selection,
                                parent_type_position,
                                fragments,
                                schema,
                            )?;
                        let key: NormalizedSelectionKey =
                            (&normalized_inline_fragment_selection).into();
                        destination.push((
                            key,
                            NormalizedSelection::InlineFragment(Arc::new(
                                normalized_inline_fragment_selection,
                            )),
                        ));
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
                        )?;
                    } else {
                        let normalized_inline_fragment_selection =
                            NormalizedInlineFragmentSelection::normalize_and_expand_fragments(
                                inline_fragment_selection,
                                parent_type_position,
                                fragments,
                                schema,
                            )?;
                        let key: NormalizedSelectionKey =
                            (&normalized_inline_fragment_selection).into();
                        destination.push((
                            key,
                            NormalizedSelection::InlineFragment(Arc::new(
                                normalized_inline_fragment_selection,
                            )),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Merges the given normalized selection sets into this one.
    pub(crate) fn merge_into(
        &mut self,
        others: Vec<NormalizedSelectionSet>,
    ) -> Result<(), FederationError> {
        if !others.is_empty() {
            let mut pairs = vec![];
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
                for pair in selections {
                    pairs.push(pair);
                }
            }
            self.merge_pairs_into(pairs)?;
        }
        Ok(())
    }

    /// A helper function for merging a vector of (key, selection) pairs into this one.
    fn merge_pairs_into(
        &mut self,
        others: Vec<(NormalizedSelectionKey, NormalizedSelection)>,
    ) -> Result<(), FederationError> {
        if !others.is_empty() {
            let mut fields = IndexMap::new();
            let mut fragment_spreads = IndexMap::new();
            let mut inline_fragments = IndexMap::new();
            for (other_key, other_selection) in others {
                let is_deferred = is_deferred_selection(other_selection.directives());
                let other_key = if is_deferred {
                    let mut new_key = other_key;
                    while self.selections.contains_key(&new_key) {
                        new_key = new_key.next_key();
                    }
                    new_key
                } else {
                    other_key
                };
                match Arc::make_mut(&mut self.selections).entry(other_key.clone()) {
                    Entry::Occupied(existing) => match existing.get() {
                        NormalizedSelection::Field(self_field_selection) => {
                            let NormalizedSelection::Field(other_field_selection) = other_selection
                            else {
                                return Err(Internal {
                                        message: format!(
                                            "Field selection key for field \"{}\" references non-field selection",
                                            self_field_selection.field.field_position,
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
                                            self_fragment_spread_selection.fragment_name,
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
                                            self_inline_fragment_selection.inline_fragment.parent_type_position,
                                            self_inline_fragment_selection.inline_fragment.type_condition_position.clone()
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
                        vacant.insert(other_selection);
                    }
                }
            }
            for (key, self_selection) in Arc::make_mut(&mut self.selections).iter_mut() {
                match self_selection {
                    NormalizedSelection::Field(self_field_selection) => {
                        if let Some(other_field_selections) = fields.remove(key) {
                            Arc::make_mut(self_field_selection)
                                .merge_into(other_field_selections)?;
                        }
                    }
                    NormalizedSelection::FragmentSpread(self_fragment_spread_selection) => {
                        if let Some(other_fragment_spread_selections) = fragment_spreads.remove(key)
                        {
                            Arc::make_mut(self_fragment_spread_selection)
                                .merge_into(other_fragment_spread_selections)?;
                        }
                    }
                    NormalizedSelection::InlineFragment(self_inline_fragment_selection) => {
                        if let Some(other_inline_fragment_selections) = inline_fragments.remove(key)
                        {
                            Arc::make_mut(self_inline_fragment_selection)
                                .merge_into(other_inline_fragment_selections)?;
                        }
                    }
                }
            }
        }
        Ok(())
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
            field: NormalizedField {
                schema: schema.clone(),
                field_position,
                alias: field.alias.clone(),
                arguments: Arc::new(field.arguments.clone()),
                directives: Arc::new(field.directives.clone()),
            },
            selection_set: if field_composite_type_result.is_ok() {
                Some(NormalizedSelectionSet::normalize_and_expand_fragments(
                    &field.selection_set,
                    fragments,
                    schema,
                )?)
            } else {
                None
            },
        }))
    }

    /// Merges the given normalized field selections into this one (this method assumes the keys
    /// already match).
    pub(crate) fn merge_into(
        &mut self,
        others: Vec<NormalizedFieldSelection>,
    ) -> Result<(), FederationError> {
        if !others.is_empty() {
            let self_field = &self.field;
            let mut selection_sets = vec![];
            for other in others {
                let other_field = &other.field;
                if other_field.schema != self_field.schema {
                    return Err(Internal {
                        message: "Cannot merge field selections from different schemas".to_owned(),
                    }
                    .into());
                }
                if other_field.field_position != self_field.field_position {
                    return Err(Internal {
                        message: format!(
                            "Cannot merge field selection for field \"{}\" into a field selection for field \"{}\"",
                            other_field.field_position,
                            self_field.field_position,
                        ),
                    }.into());
                }
                if self.selection_set.is_some() {
                    let Some(other_selection_set) = other.selection_set else {
                        return Err(Internal {
                            message: format!(
                                "Field \"{}\" has composite type but not a selection set",
                                other_field.field_position,
                            ),
                        }
                        .into());
                    };
                    selection_sets.push(other_selection_set);
                } else if other.selection_set.is_some() {
                    return Err(Internal {
                        message: format!(
                            "Field \"{}\" has non-composite type but also has a selection set",
                            other_field.field_position,
                        ),
                    }
                    .into());
                }
            }
            if let Some(self_selection_set) = &mut self.selection_set {
                self_selection_set.merge_into(selection_sets)?;
            }
        }
        Ok(())
    }
}

impl NormalizedFragmentSpreadSelection {
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
            inline_fragment: NormalizedInlineFragment {
                schema: schema.clone(),
                parent_type_position: parent_type_position.clone(),
                type_condition_position: Some(type_condition_position),
                directives: Arc::new(fragment_spread.directives.clone()),
            },
            selection_set: NormalizedSelectionSet::normalize_and_expand_fragments(
                &fragment.selection_set,
                fragments,
                schema,
            )?,
        })
    }

    /// Merges the given normalized fragment spread selections into this one (this method assumes
    /// the keys already match).
    pub(crate) fn merge_into(
        &mut self,
        others: Vec<NormalizedFragmentSpreadSelection>,
    ) -> Result<(), FederationError> {
        if !others.is_empty() {
            for other in others {
                if other.schema != self.schema {
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
    ) -> Result<NormalizedInlineFragmentSelection, FederationError> {
        let type_condition_position: Option<CompositeTypeDefinitionPosition> =
            if let Some(type_condition) = &inline_fragment.type_condition {
                Some(schema.get_type(type_condition.clone())?.try_into()?)
            } else {
                None
            };
        Ok(NormalizedInlineFragmentSelection {
            inline_fragment: NormalizedInlineFragment {
                schema: schema.clone(),
                parent_type_position: parent_type_position.clone(),
                type_condition_position,
                directives: Arc::new(inline_fragment.directives.clone()),
            },
            selection_set: NormalizedSelectionSet::normalize_and_expand_fragments(
                &inline_fragment.selection_set,
                fragments,
                schema,
            )?,
        })
    }

    /// Merges the given normalized inline fragment selections into this one (this method assumes
    /// the keys already match).
    pub(crate) fn merge_into(
        &mut self,
        others: Vec<NormalizedInlineFragmentSelection>,
    ) -> Result<(), FederationError> {
        if !others.is_empty() {
            let self_inline_fragment = &self.inline_fragment;
            let mut selection_sets = vec![];
            for other in others {
                let other_inline_fragment = &other.inline_fragment;
                if other_inline_fragment.schema != self_inline_fragment.schema {
                    return Err(Internal {
                        message: "Cannot merge inline fragment from different schemas".to_owned(),
                    }
                    .into());
                }
                if other_inline_fragment.parent_type_position
                    != self_inline_fragment.parent_type_position
                {
                    return Err(Internal {
                        message: format!(
                            "Cannot merge inline fragment of parent type \"{}\" into an inline fragment of parent type \"{}\"",
                            other_inline_fragment.parent_type_position,
                            self_inline_fragment.parent_type_position,
                        ),
                    }.into());
                }
                selection_sets.push(other.selection_set);
            }
            self.selection_set.merge_into(selection_sets)?;
        }
        Ok(())
    }
}

impl TryFrom<&NormalizedSelectionSet> for SelectionSet {
    type Error = FederationError;

    fn try_from(val: &NormalizedSelectionSet) -> Result<Self, Self::Error> {
        let mut flattened = vec![];
        for normalized_selection in val.selections.values() {
            let selection = match normalized_selection {
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
            };
            flattened.push(selection);
        }
        Ok(Self {
            ty: val.type_position.type_name().clone(),
            selections: flattened,
        })
    }
}

impl TryFrom<&NormalizedFieldSelection> for Field {
    type Error = FederationError;

    fn try_from(val: &NormalizedFieldSelection) -> Result<Self, Self::Error> {
        let normalized_field = &val.field;
        let definition = normalized_field
            .field_position
            .get(normalized_field.schema.schema())?
            .node
            .to_owned();
        let selection_set = if let Some(selection_set) = &val.selection_set {
            selection_set.try_into()?
        } else {
            SelectionSet {
                ty: definition.ty.inner_named_type().clone(),
                selections: vec![],
            }
        };
        Ok(Self {
            definition,
            alias: normalized_field.alias.to_owned(),
            name: normalized_field.name().to_owned(),
            arguments: normalized_field.arguments.deref().to_owned(),
            directives: normalized_field.directives.deref().to_owned(),
            selection_set,
        })
    }
}

impl TryFrom<&NormalizedInlineFragmentSelection> for InlineFragment {
    type Error = FederationError;

    fn try_from(val: &NormalizedInlineFragmentSelection) -> Result<Self, Self::Error> {
        let normalized_inline_fragment = &val.inline_fragment;
        Ok(Self {
            type_condition: normalized_inline_fragment
                .type_condition_position
                .as_ref()
                .map(|pos| pos.type_name().clone()),
            directives: normalized_inline_fragment.directives.deref().to_owned(),
            selection_set: (&val.selection_set).try_into()?,
        })
    }
}

impl From<&NormalizedFragmentSpreadSelection> for FragmentSpread {
    fn from(val: &NormalizedFragmentSpreadSelection) -> Self {
        Self {
            fragment_name: val.fragment_name.to_owned(),
            directives: val.directives.deref().to_owned(),
        }
    }
}

impl NormalizedSelectionKey {
    /// Generate new key by incrementing unique label value
    fn next_key(self) -> Self {
        match self {
            Self::Field {
                response_name,
                directives,
                label,
            } => Self::Field {
                response_name: response_name.clone(),
                directives: directives.clone(),
                label: label + 1,
            },
            Self::FragmentSpread {
                name,
                directives,
                label,
            } => Self::FragmentSpread {
                name: name.clone(),
                directives: directives.clone(),
                label: label + 1,
            },
            Self::InlineFragment {
                type_condition,
                directives,
                label,
            } => Self::InlineFragment {
                type_condition: type_condition.clone(),
                directives: directives.clone(),
                label: label + 1,
            },
        }
    }
}

impl From<&NormalizedSelection> for NormalizedSelectionKey {
    fn from(value: &NormalizedSelection) -> Self {
        match value {
            NormalizedSelection::Field(field_selection) => field_selection.deref().into(),
            NormalizedSelection::FragmentSpread(fragment_spread_selection) => {
                fragment_spread_selection.deref().into()
            }
            NormalizedSelection::InlineFragment(inline_fragment_selection) => {
                inline_fragment_selection.deref().into()
            }
        }
    }
}

impl From<&NormalizedFieldSelection> for NormalizedSelectionKey {
    fn from(field_selection: &NormalizedFieldSelection) -> Self {
        (&field_selection.field).into()
    }
}

impl From<&NormalizedField> for NormalizedSelectionKey {
    fn from(field: &NormalizedField) -> Self {
        Self::Field {
            response_name: field.response_name(),
            directives: Arc::new(directives_with_sorted_arguments(&field.directives)),
            label: 0,
        }
    }
}

impl From<&NormalizedFragmentSpreadSelection> for NormalizedSelectionKey {
    fn from(fragment_spread_selection: &NormalizedFragmentSpreadSelection) -> Self {
        Self::FragmentSpread {
            name: fragment_spread_selection.fragment_name.clone(),
            directives: Arc::new(directives_with_sorted_arguments(
                &fragment_spread_selection.directives,
            )),
            label: 0,
        }
    }
}

impl From<&NormalizedInlineFragmentSelection> for NormalizedSelectionKey {
    fn from(inline_fragment_selection: &NormalizedInlineFragmentSelection) -> Self {
        (&inline_fragment_selection.inline_fragment).into()
    }
}

impl From<&NormalizedInlineFragment> for NormalizedSelectionKey {
    fn from(inline_fragment: &NormalizedInlineFragment) -> Self {
        Self::InlineFragment {
            type_condition: inline_fragment
                .type_condition_position
                .as_ref()
                .map(|pos| pos.type_name().clone()),
            directives: Arc::new(directives_with_sorted_arguments(
                &inline_fragment.directives,
            )),
            label: 0,
        }
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
    directives.iter().any(|d| d.name == "defer")
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
pub fn normalize_operation(
    operation: &mut Operation,
    fragments: &IndexMap<Name, Node<Fragment>>,
    schema: &ValidFederationSchema,
) -> Result<(), FederationError> {
    let normalized_selection_set = NormalizedSelectionSet::normalize_and_expand_fragments(
        &operation.selection_set,
        fragments,
        schema,
    )?;

    // Flatten it back into a `SelectionSet`.
    operation.selection_set = (&normalized_selection_set).try_into()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::query_plan::operation::normalize_operation;
    use crate::schema::ValidFederationSchema;
    use apollo_compiler::ExecutableDocument;

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
            let operation = operation.make_mut();
            normalize_operation(operation, &executable_document.fragments, &schema).unwrap();

            let expected = r#"query NamedFragmentQuery {
  foo {
    id
    bar
    baz
  }
}"#;
            let actual = operation.to_string();
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
            let operation = operation.make_mut();
            normalize_operation(operation, &executable_document.fragments, &schema).unwrap();

            let expected = r#"query NestedFragmentQuery {
  foo {
    id
    bar
    baz
  }
}"#;
            let actual = format!("{}", operation);
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
            let operation = operation.make_mut();
            normalize_operation(operation, &executable_document.fragments, &schema).unwrap();

            assert!(operation.selection_set.selections.is_empty());
        }
    }
}
