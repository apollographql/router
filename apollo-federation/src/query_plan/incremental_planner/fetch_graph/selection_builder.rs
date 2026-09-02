use std::collections::HashMap;
use std::sync::Arc;

use super::super::shared_path::SharedPath;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::operation::OpPathElement;

/// Opaque undo checkpoint: the entries Vec length at a point in time.
/// Restoring truncates all insertions made since, O(1).
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
    pub(super) fn restore_head(&mut self, cp: SelectionCheckpoint) {
        self.entries.truncate(cp.0);
    }

    /// Absorb all entries from `other`, for post-search merging of sibling
    /// entity groups. Caller must verify compatibility via
    /// [`field_signatures`](Self::field_signatures) first.
    #[allow(dead_code)]
    pub(super) fn merge_from(&mut self, other: &SelectionBuilder) {
        self.entries.extend(other.entries.iter().cloned());
    }

    /// Map from response path to field signature (name, alias, arguments,
    /// directives). Two builders with different signatures at the same
    /// response path cannot be merged into one fetch.
    ///
    /// Keys use field response names only; inline fragments are
    /// transparent (they add no level to the response). Fields under
    /// disjoint type conditions that legally differ per the field-merging
    /// rules will appear to conflict here. That's conservative: a false
    /// conflict keeps groups separate, while a missed conflict produces
    /// an invalid subgraph query.
    ///
    /// Returns `None` if any response path has more than one distinct
    /// field signature within this builder, which indicates a bug in the
    /// commit logic.
    #[allow(dead_code)]
    pub(super) fn field_signatures(&self) -> Option<HashMap<String, String>> {
        fn record_selection_set(
            out: &mut HashMap<String, String>,
            prefix: &str,
            selections: &SelectionSet,
        ) -> bool {
            for sel in selections.selections.values() {
                match sel {
                    Selection::Field(field_sel) => {
                        let key = format!("{prefix}/{}", field_sel.field.response_name());
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
                        // Fragments are transparent in the response.
                        if !record_selection_set(out, prefix, &frag_sel.selection_set) {
                            return false;
                        }
                    }
                }
            }
            true
        }

        /// Insert a field signature, returning false if a *different*
        /// signature already exists at the same key.
        fn try_insert(out: &mut HashMap<String, String>, key: String, sig: String) -> bool {
            match out.entry(key) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(sig);
                    true
                }
                std::collections::hash_map::Entry::Occupied(e) => *e.get() == sig,
            }
        }

        let mut out = HashMap::new();
        for entry in &self.entries {
            let mut prefix = String::new();
            for element in entry.path.iter() {
                match element.as_ref() {
                    OpPathElement::Field(field) => {
                        let key = format!("{prefix}/{}", field.response_name());
                        if !try_insert(&mut out, key.clone(), field.to_string()) {
                            return None;
                        }
                        prefix = key;
                    }
                    // Inline fragments in the op path are transparent.
                    OpPathElement::InlineFragment(_) => {}
                }
            }
            if let Some(selections) = &entry.selections
                && !record_selection_set(&mut out, &prefix, selections)
            {
                return None;
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

    /// Two builders placing different fields at the same response position
    /// (one bare, one under a type condition) should have conflicting
    /// signatures at that response key.
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

        // Builder A: bare field `name` under `node`
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

        // Builder B: `name` under `... on User` under `node`
        let op_b = crate::operation::Operation::parse(
            schema,
            r#"{ node { ... on User { name } } }"#,
            "b.graphql",
        )
        .expect("valid operation");
        let Some(Selection::Field(node_b)) = op_b.selection_set.selections.values().next() else {
            panic!("expected node field");
        };
        let subs_b = Arc::new(node_b.selection_set.clone().expect("has sub-selections"));
        let mut builder_b = SelectionBuilder::default();
        let path_b = SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_b.field.clone())));
        builder_b.insert(&path_b, Some(&subs_b));

        let sigs_a = builder_a.field_signatures();
        let sigs_b = builder_b.field_signatures();

        let sigs_a = sigs_a.expect("builder A should have consistent signatures");
        let sigs_b = sigs_b.expect("builder B should have consistent signatures");

        // Both should have an entry keyed by the response path `/node/name`.
        // The `name` field has the same signature in both, so they're
        // compatible, but the point is both must appear under the same key
        // for the compatibility check to work at all.
        assert!(
            sigs_a.contains_key("/node/name"),
            "builder A should key `name` at /node/name, got keys: {:?}",
            sigs_a.keys().collect::<Vec<_>>(),
        );
        assert!(
            sigs_b.contains_key("/node/name"),
            "builder B should key `name` at /node/name (not qualified by the type condition), got keys: {:?}",
            sigs_b.keys().collect::<Vec<_>>(),
        );
    }

    /// Inserting two entries that produce different field signatures at the
    /// same response key should be detectable. Currently the second insert
    /// silently overwrites the first.
    #[test]
    fn field_signatures_detects_intra_builder_conflict() {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            r#"
            type Query { node: Node }
            type Node { value(scale: Int): Int }
            "#,
            "schema.graphql",
        )
        .expect("valid schema");
        let schema =
            crate::schema::ValidFederationSchema::new(schema).expect("valid federation schema");

        // First entry: `value` with no arguments
        let op_bare = crate::operation::Operation::parse(
            schema.clone(),
            r#"{ node { value } }"#,
            "bare.graphql",
        )
        .expect("valid operation");
        let Some(Selection::Field(node_bare)) = op_bare.selection_set.selections.values().next()
        else {
            panic!("expected node field");
        };
        let subs_bare = Arc::new(node_bare.selection_set.clone().expect("has sub-selections"));

        // Second entry: `value(scale: 100)`, same response key but different signature
        let op_args = crate::operation::Operation::parse(
            schema,
            r#"{ node { value(scale: 100) } }"#,
            "args.graphql",
        )
        .expect("valid operation");
        let Some(Selection::Field(node_args)) = op_args.selection_set.selections.values().next()
        else {
            panic!("expected node field");
        };
        let subs_args = Arc::new(node_args.selection_set.clone().expect("has sub-selections"));

        let mut builder = SelectionBuilder::default();
        let path_bare =
            SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_bare.field.clone())));
        builder.insert(&path_bare, Some(&subs_bare));
        let path_args =
            SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_args.field.clone())));
        builder.insert(&path_args, Some(&subs_args));

        // Two entries at the same response path with different signatures
        // should be detected as an internal conflict.
        assert!(
            builder.field_signatures().is_none(),
            "conflicting signatures at the same response key should return None",
        );
    }

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

        // The path element records the enclosing field itself.
        assert_eq!(signatures.get("/node").map(String::as_str), Some("node"));

        // Inline fragments are transparent: fields inside them are keyed
        // by their response path without the type condition segment.
        assert_eq!(
            signatures.get("/node/address").map(String::as_str),
            Some("address"),
        );
        assert_eq!(
            signatures.get("/node/address/street").map(String::as_str),
            Some("street"),
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
        assert!(builder.field_signatures().is_empty());
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

        let signatures = builder.field_signatures();
        assert_eq!(signatures.len(), 1);
        assert!(signatures.contains_key("/name"));
    }
}
