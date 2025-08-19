//! Provides methods for recursively merging selections and selection sets.
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;

use super::FieldSelection;
use super::FieldSelectionValue;
use super::HasSelectionKey as _;
use super::InlineFragmentSelection;
use super::InlineFragmentSelectionValue;
use super::Selection;
use super::SelectionSet;
use super::SelectionValue;
use super::selection_map;
use crate::bail;
use crate::ensure;
use crate::error::FederationError;
use crate::error::SingleFederationError;

impl FieldSelectionValue<'_> {
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
    fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op FieldSelection>,
    ) -> Result<(), FederationError> {
        let self_field = &self.get().field;
        let mut selection_sets = vec![];
        for other in others {
            let other_field = &other.field;
            ensure!(
                other_field.schema == self_field.schema,
                "Cannot merge field selections from different schemas",
            );
            if other_field.field_position != self_field.field_position {
                return Err(SingleFederationError::InternalUnmergeableFields {
                    message: format!(
                        "Cannot merge field selection for field \"{}\" into a field selection for \
                        field \"{}\". This is a known query planning bug in the old Javascript \
                        query planner that was silently ignored. The Rust-native query planner \
                        does not address this bug at this time, but in some cases does catch when \
                        this bug occurs. If you're seeing this message, this bug was likely \
                        triggered by one of the field selections mentioned previously having an \
                        alias that was the same name as the field in the other field selection. \
                        The recommended workaround is to change this alias to a different one in \
                        your operation.",
                        other_field.field_position, self_field.field_position,
                    ),
                }
                .into());
            }
            if self.get().selection_set.is_some() {
                let Some(other_selection_set) = &other.selection_set else {
                    bail!(
                        "Field \"{}\" has composite type but not a selection set",
                        other_field.field_position,
                    );
                };
                selection_sets.push(other_selection_set);
            } else if other.selection_set.is_some() {
                bail!(
                    "Field \"{}\" has non-composite type but also has a selection set",
                    other_field.field_position,
                );
            }
        }
        if let Some(self_selection_set) = self.get_selection_set_mut() {
            self_selection_set.merge_into(selection_sets.into_iter())?;
        }
        Ok(())
    }
}

impl InlineFragmentSelectionValue<'_> {
    /// Merges the given normalized inline fragment selections into this one.
    ///
    /// # Preconditions
    /// All selections must have the same selection key (directives). Otherwise this function
    /// produces invalid output.
    ///
    /// # Errors
    /// Returns an error if the parent type or schema of any selection does not match `self`'s.
    fn merge_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op InlineFragmentSelection>,
    ) -> Result<(), FederationError> {
        let self_inline_fragment = &self.get().inline_fragment;
        let mut selection_sets = vec![];
        for other in others {
            let other_inline_fragment = &other.inline_fragment;
            ensure!(
                other_inline_fragment.schema == self_inline_fragment.schema,
                "Cannot merge inline fragment from different schemas",
            );
            ensure!(
                other_inline_fragment.parent_type_position
                    == self_inline_fragment.parent_type_position,
                "Cannot merge inline fragment of parent type \"{}\" into an inline fragment of parent type \"{}\"",
                other_inline_fragment.parent_type_position,
                self_inline_fragment.parent_type_position,
            );
            selection_sets.push(&other.selection_set);
        }
        self.get_selection_set_mut()
            .merge_into(selection_sets.into_iter())?;
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
            ensure!(
                other.schema == self.schema,
                "Cannot merge selection sets from different schemas",
            );
            ensure!(
                other.type_position == self.type_position,
                "Cannot merge selection set for type \"{}\" into a selection set for type \"{}\"",
                other.type_position,
                self.type_position,
            );
            selections_to_merge.extend(other.selections.values());
        }
        self.merge_selections_into(selections_to_merge.into_iter())
    }

    /// NOTE: This is a private API and should be used with care, use `add_selection` instead.
    ///
    /// A helper function for merging the given selections into this one.
    ///
    /// # Errors
    /// Returns an error if the parent type or schema of any selection does not match `self`'s.
    ///
    /// Returns an error if any selection contains invalid GraphQL that prevents the merge.
    pub(super) fn merge_selections_into<'op>(
        &mut self,
        others: impl Iterator<Item = &'op Selection>,
    ) -> Result<(), FederationError> {
        let mut fields = IndexMap::default();
        let mut inline_fragments = IndexMap::default();
        let target = Arc::make_mut(&mut self.selections);
        for other_selection in others {
            let other_key = other_selection.key();
            match target.entry(other_key) {
                selection_map::Entry::Occupied(existing) => match existing.get() {
                    Selection::Field(self_field_selection) => {
                        let Selection::Field(other_field_selection) = other_selection else {
                            bail!(
                                "Field selection key for field \"{}\" references non-field selection",
                                self_field_selection.field.field_position,
                            );
                        };
                        fields
                            .entry(other_key.to_owned_key())
                            .or_insert_with(Vec::new)
                            .push(other_field_selection);
                    }
                    Selection::InlineFragment(self_inline_fragment_selection) => {
                        let Selection::InlineFragment(other_inline_fragment_selection) =
                            other_selection
                        else {
                            bail!(
                                "Inline fragment selection key under parent type \"{}\" {}references non-field selection",
                                self_inline_fragment_selection
                                    .inline_fragment
                                    .parent_type_position,
                                self_inline_fragment_selection
                                    .inline_fragment
                                    .type_condition_position
                                    .clone()
                                    .map_or_else(String::new, |cond| format!(
                                        "(type condition: {cond}) "
                                    ),),
                            );
                        };
                        inline_fragments
                            .entry(other_key.to_owned_key())
                            .or_insert_with(Vec::new)
                            .push(other_inline_fragment_selection);
                    }
                },
                selection_map::Entry::Vacant(vacant) => {
                    vacant.insert(other_selection.clone())?;
                }
            }
        }

        for self_selection in target.values_mut() {
            let key = self_selection.key().to_owned_key();
            match self_selection {
                SelectionValue::Field(mut self_field_selection) => {
                    if let Some(other_field_selections) = fields.shift_remove(&key) {
                        self_field_selection.merge_into(
                            other_field_selections.iter().map(|selection| &***selection),
                        )?;
                    }
                }
                SelectionValue::InlineFragment(mut self_inline_fragment_selection) => {
                    if let Some(other_inline_fragment_selections) =
                        inline_fragments.shift_remove(&key)
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
        ensure!(
            self.schema == *selection.schema(),
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
        ensure!(
            self.schema == selection_set.schema,
            "In order to add selection set it needs to point to the same schema."
        );
        ensure!(
            self.type_position == selection_set.type_position,
            "In order to add selection set it needs to point to the same type position"
        );
        self.merge_into(std::iter::once(selection_set))
    }

    /// Rebase given `SelectionSet` on self and then inserts it into the inner map.
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
        let rebased = selection_set.rebase_on(&self.type_position, &self.schema)?;
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
        bail!("merge_selection_sets(): must have at least one selection set");
    };
    first.merge_into(remainder.iter())?;

    // Take ownership of the first element and discard the rest;
    // we can unwrap because `split_first_mut()` guarantees at least one element will be yielded
    Ok(selection_sets.into_iter().next().unwrap())
}
