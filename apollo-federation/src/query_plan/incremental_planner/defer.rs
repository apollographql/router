//! @defer helpers for the BULB planner: extract @defer metadata from
//! operations into the `DeferInfo` / `DeferBlockInfo` structures that
//! `to_query_plan_with_defer` uses to wrap fetch nodes in `DeferNode`s.

use std::collections::HashMap;
use std::sync::Arc;

use crate::operation::InlineFragment;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_plan::QueryPathElement;

/// @defer block info for plan generation, constructed from the original
/// (un-stripped) operation after search.
#[derive(Clone, Debug, Default)]
pub(crate) struct DeferInfo {
    /// Sub-selection string for the primary (non-deferred) response.
    pub(crate) primary_sub_selection: Option<String>,
    /// Per-label deferred block info.
    pub(crate) blocks: HashMap<String, DeferBlockInfo>,
    /// Labels synthesized by defer normalization (`qp__N`) to track
    /// unlabeled @defer blocks internally. They must not leak into the
    /// emitted plan because clients never wrote them.
    pub(crate) assigned_labels: Arc<apollo_compiler::collections::IndexSet<String>>,
}

/// Metadata for a single @defer block in the operation.
#[derive(Clone, Debug)]
pub(crate) struct DeferBlockInfo {
    /// Path to the @defer in the query (for DeferredDeferBlock.query_path).
    pub(crate) query_path: Vec<QueryPathElement>,
    /// The sub-selection string for this deferred response chunk.
    pub(crate) sub_selection: Option<String>,
    /// Label of the enclosing @defer block, if this @defer is nested.
    pub(crate) parent_label: Option<String>,
}

/// Extract the @defer label from an inline fragment, if present.
pub(super) fn extract_defer_label(inline_fragment: &InlineFragment) -> Option<String> {
    inline_fragment
        .defer_directive_arguments()
        .ok()
        .flatten()
        .and_then(|args| args.label)
}

/// Clone an inline fragment with the @defer directive removed.
pub(super) fn strip_defer_directive(inline_fragment: &InlineFragment) -> InlineFragment {
    InlineFragment {
        schema: inline_fragment.schema.clone(),
        parent_type_position: inline_fragment.parent_type_position.clone(),
        type_condition_position: inline_fragment.type_condition_position.clone(),
        directives: inline_fragment
            .directives
            .iter()
            .filter(|d| d.name != "defer")
            .cloned()
            .collect(),
        selection_id: inline_fragment.selection_id,
    }
}

/// Detect @defer on an inline fragment selection: returns the label plus a
/// copy of the fragment with @defer stripped (so subgraph operations don't
/// include it), or `(None, None)`.
pub(super) fn defer_context(selection: &Selection) -> (Option<String>, Option<InlineFragment>) {
    let Selection::InlineFragment(frag_sel) = selection else {
        return (None, None);
    };
    let has_defer = frag_sel
        .inline_fragment
        .directives
        .iter()
        .any(|d| d.name == "defer");
    if !has_defer {
        return (None, None);
    }
    let label = extract_defer_label(&frag_sel.inline_fragment);
    (
        label,
        Some(strip_defer_directive(&frag_sel.inline_fragment)),
    )
}

/// Build a `DeferInfo` from the original operation's selection set,
/// separating primary (non-deferred) selections from deferred blocks.
/// `assigned_labels` are the normalization-synthesized labels for unlabeled
/// `@defer`, tracked so plan generation can suppress them from the plan.
pub(super) fn build_defer_info(
    selection_set: &SelectionSet,
    assigned_labels: Arc<apollo_compiler::collections::IndexSet<String>>,
) -> DeferInfo {
    let mut info = DeferInfo {
        assigned_labels,
        ..DeferInfo::default()
    };
    let primary = collect_non_deferred_selection(selection_set);
    info.primary_sub_selection = (!primary.is_empty()).then(|| format!("{{ {primary} }}"));
    collect_deferred_blocks(selection_set, &mut info.blocks, &[], None);

    info
}

/// Collect a string representation of non-deferred selections.
fn collect_non_deferred_selection(selection_set: &SelectionSet) -> String {
    let mut parts: Vec<String> = Vec::new();
    for sel in selection_set.selections.values() {
        match sel {
            Selection::Field(field_sel) => {
                let response_name = field_sel.field.response_name();
                if let Some(sub_sel) = field_sel.selection_set.as_ref() {
                    let inner = collect_non_deferred_selection(sub_sel);
                    // A composite field whose entire sub-selection is
                    // deferred contributes nothing to the primary.
                    if !inner.is_empty() {
                        parts.push(format!("{response_name} {{ {inner} }}"));
                    }
                } else {
                    parts.push(response_name.to_string());
                }
            }
            Selection::InlineFragment(frag_sel) => {
                if extract_defer_label(&frag_sel.inline_fragment).is_some() {
                    continue;
                }
                let type_cond = frag_sel
                    .inline_fragment
                    .type_condition_position
                    .as_ref()
                    .map(|t| format!("... on {} ", t.type_name()));
                let inner = collect_non_deferred_selection(&frag_sel.selection_set);
                if !inner.is_empty() {
                    if let Some(tc) = type_cond {
                        parts.push(format!("{tc}{{ {inner} }}"));
                    } else {
                        parts.push(inner);
                    }
                }
            }
        }
    }
    parts.join(" ")
}

/// Recursively find @defer blocks and populate the DeferInfo blocks map;
/// `parent_defer_label` records nesting in `DeferBlockInfo::parent_label`.
fn collect_deferred_blocks(
    selection_set: &SelectionSet,
    blocks: &mut HashMap<String, DeferBlockInfo>,
    current_path: &[QueryPathElement],
    parent_defer_label: Option<&str>,
) {
    for sel in selection_set.selections.values() {
        match sel {
            Selection::Field(field_sel) => {
                let response_name = field_sel.field.response_name();
                let mut child_path = current_path.to_vec();
                child_path.push(QueryPathElement::Field {
                    response_key: response_name.clone(),
                });
                if let Some(sub_sel) = field_sel.selection_set.as_ref() {
                    collect_deferred_blocks(sub_sel, blocks, &child_path, parent_defer_label);
                }
            }
            Selection::InlineFragment(frag_sel) => {
                let defer_args = frag_sel
                    .inline_fragment
                    .defer_directive_arguments()
                    .ok()
                    .flatten();

                if let Some(args) = defer_args {
                    if let Some(ref label) = args.label {
                        // The type condition stays out of the query path:
                        // the chunk is delivered at the enclosing field's
                        // path, the condition wrapping the sub-selection.
                        let query_path = current_path.to_vec();
                        let inner = serialize_selection_set(&frag_sel.selection_set);
                        let sub_sel_str = match &frag_sel.inline_fragment.type_condition_position {
                            Some(tc) => format!("{{ ... on {} {} }}", tc.type_name(), inner),
                            None => inner,
                        };

                        blocks.insert(
                            label.clone(),
                            DeferBlockInfo {
                                query_path,
                                sub_selection: Some(sub_sel_str),
                                parent_label: parent_defer_label.map(|s| s.to_owned()),
                            },
                        );

                        // Recurse for nested @defer with this label as parent.
                        collect_deferred_blocks(
                            &frag_sel.selection_set,
                            blocks,
                            current_path,
                            Some(label),
                        );
                    }
                } else {
                    let mut child_path = current_path.to_vec();
                    if let Some(tc) = &frag_sel.inline_fragment.type_condition_position {
                        child_path.push(QueryPathElement::InlineFragment {
                            type_condition: tc.type_name().clone(),
                        });
                    }
                    collect_deferred_blocks(
                        &frag_sel.selection_set,
                        blocks,
                        &child_path,
                        parent_defer_label,
                    );
                }
            }
        }
    }
}

/// Serialize a SelectionSet into a brace-wrapped (`{ ... }`) string for
/// `DeferredDeferBlock.sub_selection`.
pub(super) fn serialize_selection_set(selection_set: &SelectionSet) -> String {
    let mut parts: Vec<String> = Vec::new();
    for sel in selection_set.selections.values() {
        match sel {
            Selection::Field(field_sel) => {
                let response_name = field_sel.field.response_name();
                if let Some(sub_sel) = field_sel.selection_set.as_ref() {
                    let inner = serialize_selection_set(sub_sel);
                    parts.push(format!("{response_name} {inner}"));
                } else {
                    parts.push(response_name.to_string());
                }
            }
            Selection::InlineFragment(frag_sel) => {
                let tc = frag_sel
                    .inline_fragment
                    .type_condition_position
                    .as_ref()
                    .map(|t| format!("... on {} ", t.type_name()))
                    .unwrap_or_default();
                let inner = serialize_selection_set(&frag_sel.selection_set);
                parts.push(format!("{tc}{inner}"));
            }
        }
    }
    format!("{{ {} }}", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;
    use crate::operation::Operation;
    use crate::schema::ValidFederationSchema;

    const SCHEMA: &str = r#"
        directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

        type Query {
          user: User
          node: Node
        }

        interface Node {
          id: ID
        }

        type User implements Node {
          id: ID
          name: String
          address: Address
        }

        type Address {
          street: String
          city: String
        }
    "#;

    fn parse(query: &str) -> Operation {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(SCHEMA, "schema.graphql")
            .expect("valid schema");
        let schema = ValidFederationSchema::new(schema).expect("valid federation schema");
        Operation::parse(schema, query, "query.graphql").expect("valid operation")
    }

    /// The first selection nested under the operation's first (field) selection.
    fn first_child_selection(op: &Operation) -> Selection {
        let Some(Selection::Field(field_sel)) = op.selection_set.selections.values().next() else {
            panic!("expected a field selection at the top level");
        };
        field_sel
            .selection_set
            .as_ref()
            .expect("field has sub-selections")
            .selections
            .values()
            .next()
            .expect("sub-selection exists")
            .clone()
    }

    #[test]
    fn defer_context_extracts_label_and_strips_directive() {
        let op = parse(r#"{ user { ... on User @defer(label: "d1") { name } } }"#);
        let fragment_sel = first_child_selection(&op);

        let (label, stripped) = defer_context(&fragment_sel);

        assert_eq!(label.as_deref(), Some("d1"));
        let stripped = stripped.expect("stripped fragment present");
        assert!(stripped.directives.iter().all(|d| d.name != "defer"));
        // The type condition survives the strip.
        assert_eq!(
            stripped
                .type_condition_position
                .as_ref()
                .map(|tc| tc.type_name().clone()),
            Some(name!("User"))
        );
    }

    #[test]
    fn defer_context_unlabeled_defer_still_strips_directive() {
        let op = parse(r#"{ user { ... on User @defer { name } } }"#);
        let fragment_sel = first_child_selection(&op);

        let (label, stripped) = defer_context(&fragment_sel);

        assert_eq!(label, None);
        let stripped = stripped.expect("stripped fragment present");
        assert!(stripped.directives.iter().all(|d| d.name != "defer"));
    }

    #[test]
    fn defer_context_ignores_fields_and_plain_fragments() {
        let op = parse(r#"{ user { name } }"#);
        let field_sel = op
            .selection_set
            .selections
            .values()
            .next()
            .expect("selection exists")
            .clone();
        let (label, stripped) = defer_context(&field_sel);
        assert_eq!(label, None);
        assert!(stripped.is_none());

        let op = parse(r#"{ node { ... on User { name } } }"#);
        let fragment_sel = first_child_selection(&op);
        let (label, stripped) = defer_context(&fragment_sel);
        assert_eq!(label, None);
        assert!(stripped.is_none());
    }

    #[test]
    fn build_defer_info_separates_primary_from_deferred() {
        let op = parse(
            r#"{
              node { ... on User { name } }
              user {
                name
                ... on User @defer(label: "d2") { address { street } }
              }
            }"#,
        );

        let info = build_defer_info(&op.selection_set, Default::default());

        // Non-deferred inline fragments stay in the primary with their type
        // condition; the deferred fragment is excluded from it.
        assert_eq!(
            info.primary_sub_selection.as_deref(),
            Some("{ node { ... on User { name } } user { name } }")
        );

        let block = info.blocks.get("d2").expect("deferred block recorded");
        assert_eq!(
            block.query_path,
            vec![QueryPathElement::Field {
                response_key: name!("user"),
            }]
        );
        assert_eq!(
            block.sub_selection.as_deref(),
            Some("{ ... on User { address { street } } }")
        );
        assert_eq!(block.parent_label, None);
    }

    #[test]
    fn build_defer_info_fully_deferred_field_has_no_primary() {
        let op = parse(r#"{ user { ... on User @defer(label: "d3") { name } } }"#);

        let info = build_defer_info(&op.selection_set, Default::default());

        // `user`'s entire sub-selection is deferred, so it contributes
        // nothing to the primary.
        assert_eq!(info.primary_sub_selection, None);
        assert!(info.blocks.contains_key("d3"));
    }

    #[test]
    fn deferred_block_inside_plain_fragment_records_fragment_in_path() {
        let op = parse(
            r#"{
              node {
                ... on User {
                  name
                  ... on User @defer(label: "d4") { address { street } }
                }
              }
            }"#,
        );

        let info = build_defer_info(&op.selection_set, Default::default());

        let block = info.blocks.get("d4").expect("deferred block recorded");
        // The enclosing non-defer fragment contributes an InlineFragment
        // path element; the deferred fragment's own condition does not.
        assert_eq!(
            block.query_path,
            vec![
                QueryPathElement::Field {
                    response_key: name!("node"),
                },
                QueryPathElement::InlineFragment {
                    type_condition: name!("User"),
                },
            ]
        );
        assert_eq!(block.parent_label, None);
    }
}
