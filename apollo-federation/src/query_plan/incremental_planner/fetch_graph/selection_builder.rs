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
    /// entity groups.
    pub(super) fn merge_from(&mut self, other: &SelectionBuilder) {
        self.entries.extend(other.entries.iter().cloned());
    }

    /// Every field occurrence, as a map from response path to field
    /// signature (its display: name, alias, arguments, directives). Two
    /// builders assigning different signatures to the same response path
    /// cannot be materialized into one fetch because the same response key would
    /// need two different field executions.
    pub(super) fn field_signatures(&self) -> HashMap<String, String> {
        fn record_selection_set(
            out: &mut HashMap<String, String>,
            prefix: &str,
            selections: &SelectionSet,
        ) {
            for sel in selections.selections.values() {
                match sel {
                    Selection::Field(field_sel) => {
                        let key = format!("{prefix}/{}", field_sel.field.response_name());
                        out.insert(key.clone(), field_sel.field.to_string());
                        if let Some(sub) = &field_sel.selection_set {
                            record_selection_set(out, &key, sub);
                        }
                    }
                    Selection::InlineFragment(frag_sel) => {
                        let key = format!("{prefix}/{}", frag_sel.inline_fragment);
                        record_selection_set(out, &key, &frag_sel.selection_set);
                    }
                }
            }
        }

        let mut out = HashMap::new();
        for entry in &self.entries {
            let mut prefix = String::new();
            for element in entry.path.iter() {
                match element.as_ref() {
                    OpPathElement::Field(field) => {
                        let key = format!("{prefix}/{}", field.response_name());
                        out.insert(key.clone(), field.to_string());
                        prefix = key;
                    }
                    OpPathElement::InlineFragment(frag) => {
                        prefix = format!("{prefix}/{frag}");
                    }
                }
            }
            if let Some(selections) = &entry.selections {
                record_selection_set(&mut out, &prefix, selections);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builder_clone_is_cheap() {
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

        let signatures = builder.field_signatures();

        // The path element records the enclosing field itself.
        assert_eq!(signatures.get("/node").map(String::as_str), Some("node"));

        // The inline fragment contributes a path segment but no signature of
        // its own; fields inside it (and their sub-selections) are recorded.
        let (address_key, address_sig) = signatures
            .iter()
            .find(|(key, _)| key.ends_with("/address"))
            .expect("address field recorded");
        assert!(address_key.starts_with("/node/"));
        assert!(address_key.contains("... on User"));
        assert_eq!(address_sig, "address");

        let (street_key, street_sig) = signatures
            .iter()
            .find(|(key, _)| key.ends_with("/street"))
            .expect("street field recorded");
        assert!(street_key.ends_with("/address/street"));
        assert_eq!(street_sig, "street");
    }
}
