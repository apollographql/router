use std::collections::HashMap;
use std::sync::Arc;

use super::super::shared_path::SharedPath;
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

    /// Compute field signatures keyed by response path. Each entry maps a
    /// response path (segments from root to leaf) to a signature string that
    /// captures the field's identity: name, declared type, and arguments.
    /// Two fields at the same response path with different signatures cannot
    /// be merged (they would violate the SameResponseShape rule, where fields
    /// at the same response path must have compatible response shapes).
    ///
    /// Returns `None` when a path element cannot be resolved to a signature,
    /// indicating a builder state that should not be merged.
    pub(crate) fn field_signatures(&self) -> Option<HashMap<Vec<String>, String>> {
        let mut signatures = HashMap::new();
        for entry in &self.entries {
            let mut segments: Vec<String> = Vec::new();
            for element in entry.path.iter() {
                match element.as_ref() {
                    OpPathElement::Field(field) => {
                        segments.push(field.response_name().to_string());
                    }
                    OpPathElement::InlineFragment(frag) => {
                        if let Some(tc) = &frag.type_condition_position {
                            segments.push(format!("...on {}", tc.type_name()));
                        }
                    }
                }
            }
            if segments.is_empty() {
                continue;
            }
            // The signature captures the field's identity: its name and
            // arguments determine the response shape at this path.
            let last = entry.path.last()?;
            let signature = match last.as_ref() {
                OpPathElement::Field(field) => {
                    let mut sig = field.field_position.field_name().to_string();
                    if !field.arguments.is_empty() {
                        let mut args: Vec<String> = field
                            .arguments
                            .iter()
                            .map(|a| format!("{}:{}", a.name, a.value))
                            .collect();
                        args.sort();
                        sig.push('(');
                        sig.push_str(&args.join(","));
                        sig.push(')');
                    }
                    sig
                }
                OpPathElement::InlineFragment(_) => continue,
            };
            signatures.insert(segments, signature);
        }
        Some(signatures)
    }

    /// Append all entries from another builder. Used during post-search
    /// sibling merging to absorb a merged node's selections into the
    /// survivor.
    ///
    /// Do not restore checkpoints issued before a merge, as the truncation
    /// would discard merged entries.
    pub(super) fn merge_from(&mut self, other: &SelectionBuilder) {
        self.entries.extend(other.entries.iter().cloned());
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
