//! Provides methods for recursively merging selections and selection sets.
use std::sync::Arc;

use selection_map::SelectionMap;

use super::selection_map;
use super::DirectiveList;
use super::FieldSelection;
use super::FieldSelectionValue;
use super::FragmentSpreadSelection;
use super::FragmentSpreadSelectionValue;
use super::InlineFragmentSelection;
use super::InlineFragmentSelectionValue;
use super::NamedFragments;
use super::Selection;
use super::SelectionSet;
use super::SelectionValue;
use crate::error::FederationError;
use crate::operation::HasSelectionKey;
use crate::schema::position::CompositeTypeDefinitionPosition;

impl<'a> FieldSelectionValue<'a> {
    /// Merges the given field selections into this one.
    ///
    /// # Preconditions
    /// All selections must have the same selection key (alias + directives). Otherwise
    /// this function produces invalid output.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The parent type or schema of any selection does not match `self`'s.
    /// - Any selection does not select the same field position as `self`.
    fn merge_into(&mut self, other: &FieldSelection) -> Result<(), FederationError> {
        let self_field = &self.get().field;
        let mut selection_set = None;
        let other_field = &other.field;
        if other_field.schema != self_field.schema {
            return Err(FederationError::internal(
                "Cannot merge field selections from different schemas",
            ));
        }
        if other_field.field_position != self_field.field_position {
            return Err(FederationError::internal(format!(
                    "Cannot merge field selection for field \"{}\" into a field selection for field \"{}\"",
                    other_field.field_position,
                    self_field.field_position,
                )));
        }
        if self.get().selection_set.is_some() {
            let Some(other_selection_set) = &other.selection_set else {
                return Err(FederationError::internal(format!(
                    "Field \"{}\" has composite type but not a selection set",
                    other_field.field_position,
                )));
            };
            selection_set = Some(other_selection_set);
        } else if other.selection_set.is_some() {
            return Err(FederationError::internal(format!(
                "Field \"{}\" has non-composite type but also has a selection set",
                other_field.field_position,
            )));
        }
        if let Some(self_selection_set) = self.get_selection_set_mut() {
            self_selection_set.merge_into(selection_set.into_iter())?;
        }
        Ok(())
    }
}

impl<'a> InlineFragmentSelectionValue<'a> {
    /// Merges the given normalized inline fragment selections into this one.
    ///
    /// # Preconditions
    /// All selections must have the same selection key (directives). Otherwise this function
    /// produces invalid output.
    ///
    /// # Errors
    /// Returns an error if the parent type or schema of any selection does not match `self`'s.
    fn merge_into(&mut self, other: &InlineFragmentSelection) -> Result<(), FederationError> {
        let self_inline_fragment = &self.get().inline_fragment;
        let other_inline_fragment = &other.inline_fragment;
        if other_inline_fragment.schema != self_inline_fragment.schema {
            return Err(FederationError::internal(
                "Cannot merge inline fragment from different schemas",
            ));
        }
        if other_inline_fragment.parent_type_position != self_inline_fragment.parent_type_position {
            return Err(FederationError::internal(
                format!(
                    "Cannot merge inline fragment of parent type \"{}\" into an inline fragment of parent type \"{}\"",
                    other_inline_fragment.parent_type_position,
                    self_inline_fragment.parent_type_position,
                ),
            ));
        }

        self.get_selection_set_mut()
            .merge_into(std::iter::once(&other.selection_set))
    }
}

impl<'a> FragmentSpreadSelectionValue<'a> {
    /// Merges the given normalized fragment spread selections into this one.
    ///
    /// # Preconditions
    /// All selections must have the same selection key (fragment name + directives).
    /// Otherwise this function produces invalid output.
    ///
    /// # Errors
    /// Returns an error if the parent type or schema of any selection does not match `self`'s.
    fn merge_into(&mut self, other: &FragmentSpreadSelection) -> Result<(), FederationError> {
        let self_fragment_spread = &self.get().spread;
        let other_fragment_spread = &other.spread;
        if other_fragment_spread.schema != self_fragment_spread.schema {
            return Err(FederationError::internal(
                "Cannot merge fragment spread from different schemas",
            ));
        }
        // Nothing to do since the fragment spread is already part of the selection set.
        // Fragment spreads are uniquely identified by fragment name and applied directives.
        // Since there is already an entry for the same fragment spread, there is no point
        // in attempting to merge its sub-selections, as the underlying entry should be
        // exactly the same as the currently processed one.
        Ok(())
    }
}

impl SelectionSet {
    /// NOTE: This is a private API and should be used with care, use `add_selection_set` instead.
    ///
    /// Merges the given normalized selection sets into this one.
    ///
    /// # Errors
    /// Returns an error if the parent type or schema of any selection does not match `self`'s.
    ///
    /// Returns an error if any selection contains invalid GraphQL that prevents the merge.
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
    ///
    /// If the `directives_of_parent_selection` flag is `Some`, the subselections of inline
    /// fragments will be merged into the selection set if possible. This happens when the
    /// selection set's type position matches the inline fragment's type condition or the fragment
    /// has no type condition as well as the fragment has no directives or the same list of
    /// directives as the given list of directives. Note that this happens recursely.
    ///
    /// # Errors
    /// Returns an error if the parent type or schema of any selection does not match `self`'s.
    ///
    /// Returns an error if any selection contains invalid GraphQL that prevents the merge.
    #[allow(unreachable_code)]
    pub(super) fn merge_selections_into<'op>(
        &mut self,
        mut others: impl Iterator<Item = &'op Selection>,
    ) -> Result<(), FederationError> {
        fn insert_selection(
            target: &mut SelectionMap,
            selection: &Selection,
        ) -> Result<(), FederationError> {
            match target.entry(selection.key()) {
                selection_map::Entry::Vacant(vacant) => {
                    vacant.insert(selection.clone())?;
                }
                selection_map::Entry::Occupied(mut entry) => match entry.get_mut() {
                    SelectionValue::Field(mut field) => {
                        let Selection::Field(other_field) = selection else {
                            return Err(FederationError::internal(format!(
                                "Field selection key for field \"{}\" references non-field selection",
                                field.get().field.field_position,
                            )));
                        };
                        field.merge_into(other_field)?;
                    }
                    SelectionValue::FragmentSpread(mut spread) => {
                        let Selection::FragmentSpread(other_spread) = selection else {
                            return Err(FederationError::internal(
                                format!(
                                    "Fragment spread selection key for fragment \"{}\" references non-field selection",
                                    spread.get().spread.fragment_name,
                                ),
                            ));
                        };
                        spread.merge_into(other_spread)?;
                    }
                    SelectionValue::InlineFragment(mut inline) => {
                        let Selection::InlineFragment(other_inline) = selection else {
                            return Err(FederationError::internal(
                                format!(
                                    "Inline fragment selection key under parent type \"{}\" {}references non-field selection",
                                    inline.get().inline_fragment.parent_type_position,
                                    inline.get().inline_fragment.type_condition_position.clone()
                                        .map_or_else(
                                            String::new,
                                            |cond| format!("(type condition: {}) ", cond),
                                        ),
                                ),
                            ));
                        };
                        inline.merge_into(other_inline)?;
                    }
                },
            }

            Ok(())
        }

        fn recurse_on_inline_fragment<'a>(
            target: &mut SelectionMap,
            type_pos: &CompositeTypeDefinitionPosition,
            others: impl Iterator<Item = &'a Selection>,
        ) -> Result<(), FederationError> {
            for selection in others {
                if let Selection::InlineFragment(inline) = selection {
                    if inline.is_unnecessary(type_pos) {
                        recurse_on_inline_fragment(
                            target,
                            type_pos,
                            inline.selection_set.selections.values(),
                        )?;
                        continue;
                    }
                }
                insert_selection(target, selection)?;
            }
            Ok(())
        }

        let target = Arc::make_mut(&mut self.selections);
        recurse_on_inline_fragment(target, &self.type_position, others)
    }

    /// Inserts a `Selection` into the inner map. Should a selection with the same key already
    /// exist in the map, the existing selection and the given selection are merged, replacing the
    ///
    /// existing selection while keeping the same insertion index.
    ///
    /// # Preconditions
    /// The provided selection must have the same schema and type position as `self`. Rebase your
    /// selection first if it may not meet that precondition.
    ///
    /// # Errors
    /// Returns an error if either `self` or the selection contain invalid GraphQL that prevents the merge.
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
    /// # Preconditions
    /// The provided selection set must have the same schema and type position as `self`. Use
    /// [`SelectionSet::add_selection_set`] if your selection set may not meet that precondition.
    ///
    /// # Errors
    /// Returns an error if either selection set contains invalid GraphQL that prevents the merge.
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

    /// Rebase given `SelectionSet` on self and then inserts it into the inner map. Assumes that given
    /// selection set does not reference ANY named fragments. If it does, Use `add_selection_set_with_fragments`
    /// instead.
    ///
    /// Should any sub selection with the same key already exist in the map, the existing selection
    /// and the given selection are merged, replacing the existing selection while keeping the same
    /// insertion index.
    ///
    /// # Errors
    /// Returns an error if either selection set contains invalid GraphQL that prevents the merge.
    pub(crate) fn add_selection_set(
        &mut self,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        self.add_selection_set_with_fragments(selection_set, &Default::default())
    }

    /// Rebase given `SelectionSet` on self with the specified fragments and then inserts it into the
    /// inner map.
    ///
    /// Should any sub selection with the same key already exist in the map, the existing selection
    /// and the given selection are merged, replacing the existing selection while keeping the same
    /// insertion index.
    ///
    /// # Errors
    /// Returns an error if either selection set contains invalid GraphQL that prevents the merge.
    pub(crate) fn add_selection_set_with_fragments(
        &mut self,
        selection_set: &SelectionSet,
        named_fragments: &NamedFragments,
    ) -> Result<(), FederationError> {
        let rebased =
            selection_set.rebase_on(&self.type_position, named_fragments, &self.schema)?;
        self.add_local_selection_set(&rebased)
    }
}

/// # Preconditions
/// There must be at least one selection set.
/// The selection sets must all have the same schema and type position.
///
/// # Errors
/// Returns an error if any selection set contains invalid GraphQL that prevents the merge.
pub(crate) fn merge_selection_sets(
    mut selection_sets: Vec<SelectionSet>,
) -> Result<SelectionSet, FederationError> {
    let Some((first, remainder)) = selection_sets.split_first_mut() else {
        return Err(FederationError::internal(
            "merge_selection_sets(): must have at least one selection set",
        ));
    };
    first.merge_into(remainder.iter())?;

    // Take ownership of the first element and discard the rest;
    // we can unwrap because `split_first_mut()` guarantees at least one element will be yielded
    Ok(selection_sets.into_iter().next().unwrap())
}
