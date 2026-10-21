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
    /// captures the field's identity: name and arguments. Two fields at the
    /// same response path with different signatures cannot be merged (they
    /// would violate the SameResponseShape rule, where fields at the same
    /// response path must have compatible response shapes). Both the op
    /// paths and the stored sub-selection sets are walked; a nested
    /// conflict is as unmergeable as a top-level one.
    ///
    /// Returns `None` when two entries assign different signatures to the
    /// same path, indicating a builder state that must not be merged.
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
            // The signature captures the field's identity: its name and
            // arguments determine the response shape at this path.
            if let Some(last) = entry.path.last()
                && let OpPathElement::Field(field) = last.as_ref()
                && !segments.is_empty()
            {
                let sig = field_signature(field.field_position.field_name(), &field.arguments);
                insert_signature(&mut signatures, segments.clone(), sig)?;
            }
            if let Some(selections) = &entry.selections {
                collect_selection_signatures(selections, &segments, &mut signatures)?;
            }
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

/// Render a field's merge-identity signature: declared name plus sorted
/// arguments.
fn field_signature(
    name: &apollo_compiler::Name,
    arguments: &crate::operation::ArgumentList,
) -> String {
    let mut sig = name.to_string();
    if !arguments.is_empty() {
        let mut args: Vec<String> = arguments
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

/// Record a signature, failing on a conflicting assignment for the same
/// response path.
fn insert_signature(
    signatures: &mut HashMap<Vec<String>, String>,
    path: Vec<String>,
    signature: String,
) -> Option<()> {
    match signatures.get(&path) {
        Some(taken) if *taken != signature => None,
        _ => {
            signatures.insert(path, signature);
            Some(())
        }
    }
}

/// Walk a stored sub-selection set, recording a signature per nested field.
fn collect_selection_signatures(
    selections: &SelectionSet,
    base: &[String],
    signatures: &mut HashMap<Vec<String>, String>,
) -> Option<()> {
    for selection in selections.selections.values() {
        match selection {
            crate::operation::Selection::Field(field_sel) => {
                let mut path = base.to_vec();
                path.push(field_sel.field.response_name().to_string());
                let sig = field_signature(
                    field_sel.field.field_position.field_name(),
                    &field_sel.field.arguments,
                );
                insert_signature(signatures, path.clone(), sig)?;
                if let Some(sub) = &field_sel.selection_set {
                    collect_selection_signatures(sub, &path, signatures)?;
                }
            }
            crate::operation::Selection::InlineFragment(frag_sel) => {
                let mut path = base.to_vec();
                if let Some(tc) = &frag_sel.inline_fragment.type_condition_position {
                    path.push(format!("...on {}", tc.type_name()));
                }
                collect_selection_signatures(&frag_sel.selection_set, &path, signatures)?;
            }
        }
    }
    Some(())
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

    /// A conflict nested below the entry path is visible: two builders
    /// selecting `value` and `value(scale: 100)` under the same path get
    /// different signatures at the nested key, so merge bucketing can
    /// separate them.
    #[test]
    fn field_signatures_detects_nested_argument_conflict_across_builders() {
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

        let mut builders = Vec::new();
        for query in [r#"{ node { value } }"#, r#"{ node { value(scale: 100) } }"#] {
            let op = crate::operation::Operation::parse(schema.clone(), query, "q.graphql")
                .expect("valid operation");
            let Some(Selection::Field(node_sel)) = op.selection_set.selections.values().next()
            else {
                panic!("expected node field");
            };
            let subs = Arc::new(node_sel.selection_set.clone().expect("has sub-selections"));
            let mut builder = SelectionBuilder::default();
            let path =
                SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_sel.field.clone())));
            builder.insert(&path, Some(&subs));
            builders.push(builder);
        }

        let sigs_a = builders[0].field_signatures().expect("consistent");
        let sigs_b = builders[1].field_signatures().expect("consistent");
        let key = vec!["node".to_string(), "value".to_string()];
        assert_eq!(sigs_a[&key], "value");
        assert_eq!(sigs_b[&key], "value(scale:100)");
        assert_ne!(sigs_a[&key], sigs_b[&key]);
    }

    /// Conflicting signatures for the same path within one builder mean the
    /// builder itself is unmergeable: field_signatures returns None instead
    /// of letting the last writer win. Duplicate identical entries stay fine.
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

        let mut subs = Vec::new();
        let mut node_path = None;
        for query in [r#"{ node { value } }"#, r#"{ node { value(scale: 100) } }"#] {
            let op = crate::operation::Operation::parse(schema.clone(), query, "q.graphql")
                .expect("valid operation");
            let Some(Selection::Field(node_sel)) = op.selection_set.selections.values().next()
            else {
                panic!("expected node field");
            };
            node_path = Some(
                SharedPath::new().pushed(Arc::new(OpPathElement::Field(node_sel.field.clone()))),
            );
            subs.push(Arc::new(
                node_sel.selection_set.clone().expect("has sub-selections"),
            ));
        }
        let path = node_path.expect("path built");

        let mut conflicted = SelectionBuilder::default();
        conflicted.insert(&path, Some(&subs[0]));
        conflicted.insert(&path, Some(&subs[1]));
        assert!(
            conflicted.field_signatures().is_none(),
            "value vs value(scale: 100) at one path must be unmergeable"
        );

        let mut duplicated = SelectionBuilder::default();
        duplicated.insert(&path, Some(&subs[0]));
        duplicated.insert(&path, Some(&subs[0]));
        assert!(
            duplicated.field_signatures().is_some(),
            "identical duplicate entries are consistent"
        );
    }

    /// Stored sub-selection sets are walked, including through inline
    /// fragments, so nested fields produce their own signature keys.
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

        assert_eq!(
            signatures
                .get(&vec!["node".to_string()])
                .map(String::as_str),
            Some("node")
        );
        let nested = vec![
            "node".to_string(),
            "...on User".to_string(),
            "address".to_string(),
            "street".to_string(),
        ];
        assert_eq!(signatures.get(&nested).map(String::as_str), Some("street"));
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
