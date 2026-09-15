use std::collections::HashMap;
use std::sync::Arc;

use super::super::shared_path::SharedPath;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::operation::OpPathElement;

/// Opaque undo checkpoint: the entries Vec length at a point in time.
/// Restoring truncates all insertions made since.
#[derive(Clone, Debug)]
pub(super) struct SelectionCheckpoint(usize);

/// Append-only log of selections accumulated during BULB search.
///
/// Insert is O(path_depth) (Arc pointer copies); clone is O(n).
/// Entries are only materialized into a SelectionSet once, on the
/// winning plan.
#[derive(Clone, Debug, Default)]
pub(crate) struct SelectionBuilder {
    entries: Vec<SelectionEntry>,
}

/// A single entry in the selection log.
#[derive(Clone, Debug)]
pub(crate) struct SelectionEntry {
    path: SharedPath<Arc<OpPathElement>>,
    selections: Option<Arc<SelectionSet>>,
}

impl SelectionEntry {
    pub(crate) fn path(&self) -> &SharedPath<Arc<OpPathElement>> {
        &self.path
    }

    pub(crate) fn selections(&self) -> Option<&Arc<SelectionSet>> {
        self.selections.as_ref()
    }
}

impl SelectionBuilder {
    pub(crate) fn entries(&self) -> &[SelectionEntry] {
        &self.entries
    }

    /// Record a selection at `path` (OpPath elements from the fetch node
    /// root); `selections` is None for leaf fields with no sub-selections.
    ///
    /// Entries are recorded unchecked; consistency is the caller's
    /// responsibility.
    pub(crate) fn insert(
        &mut self,
        path: &SharedPath<Arc<OpPathElement>>,
        selections: Option<&Arc<SelectionSet>>,
    ) {
        self.entries.push(SelectionEntry {
            path: path.clone(),
            selections: selections.cloned(),
        });
    }

    /// Save the current length for later undo.
    pub(super) fn save_head(&self) -> SelectionCheckpoint {
        SelectionCheckpoint(self.entries.len())
    }

    /// Restore a saved length, undoing all insertions since the checkpoint.
    ///
    /// Checkpoints must be restored on the builder that issued them, in
    /// LIFO order, and never across a `merge_from` (a restore would
    /// truncate merged entries along with probe entries).
    pub(super) fn restore_head(&mut self, cp: SelectionCheckpoint) {
        debug_assert!(
            cp.0 <= self.entries.len(),
            "checkpoint is newer than the builder state; checkpoints must be restored in LIFO order",
        );
        self.entries.truncate(cp.0);
    }

    /// Absorb all entries from another builder. Used by
    /// `merge_sibling_entities` to combine selections when merging nodes.
    pub(super) fn merge_from(&mut self, other: &SelectionBuilder) {
        self.entries.extend(other.entries.iter().cloned());
    }

    /// Map from response path to field signature (name, alias, arguments,
    /// directives). Two builders with different signatures at the same
    /// response path cannot be merged into one fetch.
    ///
    /// Returns `None` if any response path has more than one distinct
    /// field signature within this builder.
    pub(super) fn field_signatures(&self) -> Option<HashMap<Vec<String>, String>> {
        // Keys are response paths as segment vectors rather than joined
        // strings: client-provided argument values can contain any
        // separator character, and the keys are only ever compared whole.
        fn record_selection_set(
            out: &mut HashMap<Vec<String>, String>,
            prefix: &[String],
            selections: &SelectionSet,
        ) -> bool {
            for sel in selections.selections.values() {
                match sel {
                    Selection::Field(field_sel) => {
                        let mut key = prefix.to_vec();
                        key.push(field_sel.field.response_name().to_string());
                        if !try_insert(out, key.clone(), field_sel.field.to_string()) {
                            return false;
                        }
                        if let Some(sub) = &field_sel.selection_set
                            && !record_selection_set(out, &key, sub)
                        {
                            return false;
                        }
                    }
                    Selection::InlineFragment(frag_sel) => {
                        if !record_selection_set(out, prefix, &frag_sel.selection_set) {
                            return false;
                        }
                    }
                }
            }
            true
        }

        /// Insert a field signature, returning false if a different
        /// signature already occupies the same key.
        fn try_insert(
            map: &mut HashMap<Vec<String>, String>,
            key: Vec<String>,
            value: String,
        ) -> bool {
            match map.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get() == value,
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(value);
                    true
                }
            }
        }

        let mut out = HashMap::new();
        for entry in &self.entries {
            let prefix: Vec<String> = entry.path.iter().map(|e| e.to_string()).collect();
            if let Some(sels) = &entry.selections {
                if !record_selection_set(&mut out, &prefix, sels) {
                    return None;
                }
            }
        }
        Some(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_of_empty_builder_is_empty() {
        let builder = SelectionBuilder::default();
        let cloned = builder.clone();
        assert!(builder.entries.is_empty());
        assert!(cloned.entries.is_empty());
    }

    #[test]
    fn insert_then_clone_preserves_snapshot() {
        let mut builder = SelectionBuilder::default();
        let empty = SharedPath::new();
        builder.insert(&empty, None);
        let snapshot = builder.clone();

        builder.insert(&empty, None);

        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(builder.entries.len(), 2);
    }
}
