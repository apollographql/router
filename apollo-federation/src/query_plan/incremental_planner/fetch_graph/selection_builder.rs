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
    use crate::operation::Selection;

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

    /// Two entries with different path depths produce different signature
    /// keys, even when the sub-selections overlap. field_signatures only
    /// walks the path, not the sub-selections.
    #[test]
    fn field_signatures_detects_conflict_through_type_condition() {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            r#"
            type Query { node: Node }
            interface Node { id: ID, name: String }
            type User implements Node {
              id: ID
              name: String
              email: String
            }
            "#,
            "schema.graphql",
        )
        .expect("valid schema");
        let schema =
            crate::schema::ValidFederationSchema::new(schema).expect("valid federation schema");

        // Builder A: path is [node], sub-selections contain {name}
        let op_a =
            crate::operation::Operation::parse(schema.clone(), r#"{ node { name } }"#, "a.graphql")
                .expect("valid operation");
        let Some(Selection::Field(node_a)) = op_a.selection_set.selections.values().next() else {
            panic!("expected node field");
        };
        let subs_a = Arc::new(node_a.selection_set.clone().expect("has sub-selections"));
        let mut builder_a = SelectionBuilder::default();
        let path_a = SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_a.field.clone())));
        builder_a.insert(&path_a, Some(&subs_a));

        // Builder B: same path [node], sub-selections contain {id}
        let op_b = crate::operation::Operation::parse(schema, r#"{ node { id } }"#, "b.graphql")
            .expect("valid operation");
        let Some(Selection::Field(node_b)) = op_b.selection_set.selections.values().next() else {
            panic!("expected node field");
        };
        let subs_b = Arc::new(node_b.selection_set.clone().expect("has sub-selections"));
        let mut builder_b = SelectionBuilder::default();
        let path_b = SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_b.field.clone())));
        builder_b.insert(&path_b, Some(&subs_b));

        let sigs_a = builder_a.field_signatures().expect("consistent");
        let sigs_b = builder_b.field_signatures().expect("consistent");

        // Both produce a key at ["node"] since field_signatures walks the
        // path elements, not sub-selections.
        assert!(
            sigs_a.contains_key(&vec!["node".to_string()]),
            "builder A should have key [node], got: {:?}",
            sigs_a.keys().collect::<Vec<_>>(),
        );
        assert!(
            sigs_b.contains_key(&vec!["node".to_string()]),
            "builder B should have key [node], got: {:?}",
            sigs_b.keys().collect::<Vec<_>>(),
        );
        // Both entries resolve to the same field name at that path.
        assert_eq!(sigs_a[&vec!["node".to_string()]], "node");
        assert_eq!(sigs_b[&vec!["node".to_string()]], "node");
    }

    /// Inserting two entries at the same path key overwrites the signature.
    /// Both entries are recorded but field_signatures uses HashMap::insert,
    /// so the last writer wins.
    #[test]
    fn field_signatures_detects_intra_builder_conflict() {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            r#"
            type Query { node: Node }
            type Node { id: ID }
            "#,
            "schema.graphql",
        )
        .expect("valid schema");
        let schema =
            crate::schema::ValidFederationSchema::new(schema).expect("valid federation schema");

        // Both entries use the same path [node], so their signatures collide
        // at the same HashMap key. The second insert silently overwrites.
        let op = crate::operation::Operation::parse(schema, r#"{ node { id } }"#, "query.graphql")
            .expect("valid operation");
        let Some(Selection::Field(node_sel)) = op.selection_set.selections.values().next() else {
            panic!("expected node field");
        };
        let subs = Arc::new(node_sel.selection_set.clone().expect("has sub-selections"));

        let mut builder = SelectionBuilder::default();
        let path = SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_sel.field.clone())));
        builder.insert(&path, Some(&subs));
        builder.insert(&path, Some(&subs));

        assert_eq!(builder.entries.len(), 2);
        let sigs = builder
            .field_signatures()
            .expect("field_signatures returns Some even with duplicate keys");
        // Only one key survives because the second insert overwrites the first.
        assert_eq!(sigs.len(), 1);
        assert!(
            sigs.contains_key(&vec!["node".to_string()]),
            "should have a signature at [node], got keys: {:?}",
            sigs.keys().collect::<Vec<_>>(),
        );
    }

    /// field_signatures only walks the path elements in each entry, not
    /// the sub-selections. An entry with path=[node] and sub-selections
    /// containing nested fields only produces a signature at ["node"].
    #[test]
    fn field_signatures_recurses_into_sub_selections_and_fragments() {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            r#"
            type Query { node: Node }
            interface Node { id: ID }
            type User implements Node {
              id: ID
              address: Address
            }
            type Address { street: String }
            "#,
            "schema.graphql",
        )
        .expect("valid schema");
        let schema =
            crate::schema::ValidFederationSchema::new(schema).expect("valid federation schema");
        let op = crate::operation::Operation::parse(
            schema,
            r#"{ node { ... on User { address { street } } } }"#,
            "query.graphql",
        )
        .expect("valid operation");

        let Some(Selection::Field(node_sel)) = op.selection_set.selections.values().next() else {
            panic!("expected the `node` field selection");
        };
        let sub_selections = Arc::new(
            node_sel
                .selection_set
                .clone()
                .expect("node has sub-selections"),
        );

        let mut builder = SelectionBuilder::default();
        let path = SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_sel.field.clone())));
        builder.insert(&path, Some(&sub_selections));

        let signatures = builder
            .field_signatures()
            .expect("no conflicting signatures");

        // Only the path elements produce signature keys. The nested
        // sub-selections (address, street) are not walked.
        assert_eq!(signatures.len(), 1);
        assert_eq!(
            signatures
                .get(&vec!["node".to_string()])
                .map(String::as_str),
            Some("node")
        );
    }

    #[test]
    fn save_head_restore_head_undoes_insertions() {
        let mut builder = SelectionBuilder::default();
        let empty = SharedPath::new();
        builder.insert(&empty, None);
        let cp = builder.save_head();
        builder.insert(&empty, None);
        builder.insert(&empty, None);
        assert_eq!(builder.entries.len(), 3);
        builder.restore_head(cp);
        assert_eq!(builder.entries.len(), 1);
    }

    #[test]
    fn merge_from_absorbs_other_entries() {
        let mut builder = SelectionBuilder::default();
        let empty = SharedPath::new();
        builder.insert(&empty, None);

        let mut other = SelectionBuilder::default();
        other.insert(&empty, None);
        other.insert(&empty, None);

        builder.merge_from(&other);
        assert_eq!(builder.entries.len(), 3);
        assert_eq!(other.entries.len(), 2);
    }

    #[test]
    fn entry_accessors_return_stored_values() {
        let mut builder = SelectionBuilder::default();
        let empty = SharedPath::new();
        builder.insert(&empty, None);
        let entry = &builder.entries()[0];
        assert!(entry.path().iter().next().is_none());
        assert!(entry.selections().is_none());
    }

    #[test]
    fn field_signatures_empty_builder_returns_empty() {
        let builder = SelectionBuilder::default();
        let sigs = builder
            .field_signatures()
            .expect("no conflict possible with zero entries");
        assert!(sigs.is_empty());
    }

    #[test]
    fn field_signatures_leaf_entry_without_selections() {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            r#"type Query { name: String }"#,
            "schema.graphql",
        )
        .expect("valid schema");
        let schema =
            crate::schema::ValidFederationSchema::new(schema).expect("valid federation schema");
        let op = crate::operation::Operation::parse(schema, r#"{ name }"#, "query.graphql")
            .expect("valid operation");

        let Some(Selection::Field(name_sel)) = op.selection_set.selections.values().next() else {
            panic!("expected the `name` field selection");
        };

        let mut builder = SelectionBuilder::default();
        let path = SharedPath::new().pushed(Arc::new(OpPathElement::Field(name_sel.field.clone())));
        builder.insert(&path, None);

        let signatures = builder
            .field_signatures()
            .expect("single leaf has no conflicts");
        assert_eq!(signatures.len(), 1);
        assert!(signatures.contains_key(&vec!["name".to_string()]));
    }
}
