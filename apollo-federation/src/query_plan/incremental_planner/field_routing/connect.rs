//! Connector commit logic for the BULB planner: creating connector fetch
//! groups, partitioning sub-selections against the connector's output
//! shape, and re-routing selections the connector does not provide.

use std::sync::Arc;

use apollo_compiler::Name;
use petgraph::graph::NodeIndex;
use shape::Shape;
use shape::ShapeCase;
use tracing::trace;

use super::super::fetch_graph::InputContribution;
use super::super::shared_path::SharedPath;
use super::FieldRoutingSearchSpace;
use super::commit::field_response_elements;
use super::routing::HopKind;
use super::routing::RoutingChoice;
use super::state::PendingSelection;
use super::state::PlanState;
use crate::connectors::Connector;
use crate::error::FederationError;
use crate::operation::FieldSelection;
use crate::operation::InlineFragmentSelection;
use crate::operation::Selection;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::FetchDataPathElement;
use crate::schema::position::CompositeTypeDefinitionPosition;

/// A selection the connector's output shape does not provide, pulled out to
/// be routed independently (typically through an entity-resolver connector).
struct DeferredConnectorSelection {
    selection: Selection,
    /// The type containing the deferred selection.
    parent_type: Name,
    /// Query graph node for `parent_type` in the connector's subgraph.
    anchor_node: NodeIndex,
    /// Op path (within the connector's fetch group) to the selection's parent.
    op_path: SharedPath<Arc<OpPathElement>>,
    /// Response path (within the connector's fetch group) to the parent.
    response_path: SharedPath<FetchDataPathElement>,
}

/// Strip array wrappers off a shape: a selection set on a list-typed field
/// is checked against the element shape.
fn unwrap_list_shape(shape: &Shape) -> &Shape {
    let mut shape = shape;
    while let ShapeCase::Array { tail, .. } = shape.case() {
        shape = tail;
    }
    shape
}

impl FieldRoutingSearchSpace {
    /// Commit a routing choice that targets a connector. Connector fields
    /// are opaque to the planner — the connector resolves the field and its
    /// whole sub-selection tree, so the subtree is bulk-inserted rather than
    /// pushed onto the pending stack. Root-level fields get a connector root
    /// group; entity-resolver connectors (key hops) get a connector entity
    /// group depending on the parent.
    pub(super) fn commit_connector_choice(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        connector: &Arc<Connector>,
        source_subgraph: &Arc<str>,
        choice: &RoutingChoice,
    ) -> Result<(), FederationError> {
        trace!(
            source_subgraph = %source_subgraph,
            coordinate = %connector.id.coordinate(),
            "committing connector choice",
        );
        let current_node_data = self.qg().node_weight(pending.query_graph_node)?;

        let Selection::Field(field_sel) = &pending.selection else {
            // Connectors resolve fields, not inline fragments.
            return Ok(());
        };
        let field_op_element = Arc::new(OpPathElement::Field(field_sel.field.clone()));
        let child_defer_ref = pending.defer_ref.clone();
        let defer_for_children = child_defer_ref.clone();

        let (fetch_node, child_op_path, response_base) =
            if matches!(choice.hop_kind, HopKind::KeyHop) {
                // Entity-resolver connector: key fields ride the parent fetch;
                // the connector group merges at the pending's response path.
                let source = self.node_source(pending.query_graph_node)?;
                let key_locally_resolvable = match &choice.key_conditions {
                    Some(key_conditions) => self.can_resolve_in_place(
                        pending.query_graph_node,
                        key_conditions,
                        &source,
                    )?,
                    None => true,
                };
                let local_key = key_locally_resolvable
                    .then_some(choice.key_conditions.as_ref())
                    .flatten();
                self.append_entity_inputs(
                    state,
                    pending.fetch_node,
                    &pending.op_path,
                    local_key,
                    &source,
                );

                let merge_at = self.pending_merge_at(state, pending);

                let new_group = state.graph.add_connector_entity_group(
                    source_subgraph,
                    merge_at,
                    connector.clone(),
                    child_defer_ref,
                );
                let inputs = choice
                    .key_conditions
                    .iter()
                    .map(|key_conditions| InputContribution {
                        source_type_name: source.type_pos.type_name().clone(),
                        conditions: key_conditions.clone(),
                        rewrite_info: None,
                        condition_alias_rewrites: Vec::new(),
                    })
                    .collect();
                state
                    .graph
                    .add_dependency(pending.fetch_node, new_group, inputs);

                if !key_locally_resolvable && let Some(key_conditions) = &choice.key_conditions {
                    self.push_condition_pendings(state, pending, key_conditions, new_group)?;
                }

                let entity_base_path = self.entity_root_path(source.type_pos.type_name())?;
                (
                    new_group,
                    entity_base_path.pushed(field_op_element),
                    SharedPath::new(),
                )
            } else if matches!(
                current_node_data.type_,
                QueryGraphNodeType::FederatedRootType(_)
            ) {
                // Root-level connector field: a fresh root group per connector.
                let root_kind = match &current_node_data.type_ {
                    QueryGraphNodeType::FederatedRootType(kind) => *kind,
                    _ => unreachable!(),
                };
                let root_type_name = self
                    .supergraph_schema
                    .schema()
                    .root_operation(root_kind.into())
                    .ok_or_else(|| {
                        FederationError::internal(format!("no root type for {root_kind:?}"))
                    })?;
                let root_type: CompositeTypeDefinitionPosition = self
                    .supergraph_schema
                    .get_type(root_type_name)?
                    .try_into()?;
                let node = state.graph.add_connector_root_group(
                    source_subgraph,
                    root_type,
                    connector.clone(),
                    child_defer_ref,
                );
                (
                    node,
                    pending.op_path.pushed(field_op_element),
                    pending.path_in_fetch.clone(),
                )
            } else {
                // Non-root direct connector (no key): a child group merging
                // at the pending's response path.
                let source = self.node_source(pending.query_graph_node)?;
                let merge_at = self.pending_merge_at(state, pending);

                let new_group = state.graph.add_connector_entity_group(
                    source_subgraph,
                    merge_at,
                    connector.clone(),
                    child_defer_ref,
                );
                state
                    .graph
                    .add_dependency(pending.fetch_node, new_group, Vec::new());
                let entity_base_path = self.entity_root_path(source.type_pos.type_name())?;
                (
                    new_group,
                    entity_base_path.pushed(field_op_element),
                    SharedPath::new(),
                )
            };

        // Response path within the connector's fetch group to the field's
        // sub-selections.
        let mut child_response_path = response_base;
        for element in field_response_elements(&field_sel.field)? {
            child_response_path = child_response_path.pushed(element);
        }

        // Insert the portion of the tree the connector's output shape
        // provides. Fields outside the shape go back on the pending stack,
        // anchored at the connector's landing type.
        let sub_selections = pending.selection.selection_set();
        if let Some(sub_ss) = sub_selections.filter(|ss| !ss.is_empty()) {
            let restored = sub_ss.add_back_typename_in_attachments()?;
            let landing_type = field_sel
                .field
                .field_position
                .get(field_sel.field.schema.schema())?
                .ty
                .inner_named_type()
                .clone();
            let mut deferred = Vec::new();
            let kept = match self
                .connector_index
                .output_shape(&connector.id.coordinate())
            {
                Some(shape) if matches!(shape.case(), ShapeCase::Object { .. }) => self
                    .partition_connector_selections(
                        &restored,
                        shape,
                        &landing_type,
                        source_subgraph,
                        &child_op_path,
                        &child_response_path,
                        &mut deferred,
                    )?,
                // Non-object output shape (scalar / opaque): the connector
                // resolves the whole subtree.
                _ => Some(restored),
            };
            if let Some(kept) = kept {
                state
                    .graph
                    .append_selection(fetch_node, &child_op_path, Some(&Arc::new(kept)));
            }
            for deferred_selection in deferred {
                trace!(
                    coordinate = %connector.id.coordinate(),
                    parent_type = %deferred_selection.parent_type,
                    "re-routing selection the connector's output shape does not provide",
                );
                let parent_types = match self
                    .supergraph_schema
                    .get_type(&deferred_selection.parent_type)
                    .ok()
                    .and_then(|ty| CompositeTypeDefinitionPosition::try_from(ty).ok())
                {
                    Some(type_pos) => pending.parent_types.pushed(type_pos),
                    None => pending.parent_types.clone(),
                };
                state.push_pending(
                    pending
                        .fork(deferred_selection.selection)
                        .at(deferred_selection.anchor_node, fetch_node)
                        .with_op_path(deferred_selection.op_path)
                        .with_response_path(deferred_selection.response_path)
                        .with_defer(defer_for_children.clone())
                        .with_parent_types(parent_types),
                );
            }
        } else {
            state
                .graph
                .append_selection(fetch_node, &child_op_path, None);
        }

        if let Some(dependent) = pending.ordering_dependent() {
            state.graph.add_ordering_dependency(fetch_node, dependent)?;
        }

        Ok(())
    }

    /// Partition `sub_ss` against a connector's output `shape`: provided
    /// selections are kept; unprovided ones are collected into `deferred`,
    /// anchored at their parent type's node in the connector's subgraph.
    ///
    /// Returns `None` when nothing is kept.
    #[allow(clippy::too_many_arguments)]
    fn partition_connector_selections(
        &self,
        sub_ss: &SelectionSet,
        shape: &Shape,
        parent_type: &Name,
        source_subgraph: &Arc<str>,
        op_path: &SharedPath<Arc<OpPathElement>>,
        response_path: &SharedPath<FetchDataPathElement>,
        deferred: &mut Vec<DeferredConnectorSelection>,
    ) -> Result<Option<SelectionSet>, FederationError> {
        let ShapeCase::Object { fields, .. } = shape.case() else {
            return Ok(Some(sub_ss.clone()));
        };
        let mut kept = SelectionMap::new();
        for selection in sub_ss.selections.values() {
            match selection {
                Selection::Field(field_sel) => {
                    let field_name = field_sel.field.name();
                    if *field_name == TYPENAME_FIELD {
                        kept.insert(selection.clone());
                        continue;
                    }
                    let Some(child_shape) = fields.get(field_name.as_str()) else {
                        match self.connector_landing_node(source_subgraph, parent_type) {
                            Some(anchor_node) => deferred.push(DeferredConnectorSelection {
                                selection: selection.clone(),
                                parent_type: parent_type.clone(),
                                anchor_node,
                                op_path: op_path.clone(),
                                response_path: response_path.clone(),
                            }),
                            None => {
                                kept.insert(selection.clone());
                            }
                        }
                        continue;
                    };
                    match &field_sel.selection_set {
                        Some(child_ss) if !child_ss.is_empty() => {
                            let child_shape = unwrap_list_shape(child_shape);
                            if !matches!(child_shape.case(), ShapeCase::Object { .. }) {
                                kept.insert(selection.clone());
                                continue;
                            }
                            let field_def = field_sel
                                .field
                                .field_position
                                .get(field_sel.field.schema.schema())?;
                            let child_type = field_def.ty.inner_named_type().clone();
                            let child_op = op_path
                                .pushed(Arc::new(OpPathElement::Field(field_sel.field.clone())));
                            let mut child_response = response_path.clone();
                            for element in field_response_elements(&field_sel.field)? {
                                child_response = child_response.pushed(element);
                            }
                            let child_kept = self.partition_connector_selections(
                                child_ss,
                                child_shape,
                                &child_type,
                                source_subgraph,
                                &child_op,
                                &child_response,
                                deferred,
                            )?;
                            match child_kept {
                                Some(child_kept) if !child_kept.is_empty() => {
                                    kept.insert(Selection::Field(Arc::new(FieldSelection {
                                        field: field_sel.field.clone(),
                                        selection_set: Some(child_kept),
                                    })));
                                }
                                _ => {}
                            }
                        }
                        _ => {
                            kept.insert(selection.clone());
                        }
                    }
                }
                Selection::InlineFragment(fragment_sel) => {
                    let fragment_type = fragment_sel
                        .inline_fragment
                        .type_condition_position
                        .as_ref()
                        .map(|t| t.type_name().clone())
                        .unwrap_or_else(|| parent_type.clone());
                    let child_op = op_path.pushed(Arc::new(OpPathElement::InlineFragment(
                        fragment_sel.inline_fragment.clone(),
                    )));
                    let child_kept = self.partition_connector_selections(
                        &fragment_sel.selection_set,
                        shape,
                        &fragment_type,
                        source_subgraph,
                        &child_op,
                        response_path,
                        deferred,
                    )?;
                    if let Some(child_kept) = child_kept
                        && !child_kept.is_empty()
                    {
                        kept.insert(Selection::InlineFragment(Arc::new(
                            InlineFragmentSelection {
                                inline_fragment: fragment_sel.inline_fragment.clone(),
                                selection_set: child_kept,
                            },
                        )));
                    }
                }
            }
        }
        if kept.is_empty() {
            Ok(None)
        } else {
            Ok(Some(SelectionSet {
                schema: sub_ss.schema.clone(),
                type_position: sub_ss.type_position.clone(),
                selections: Arc::new(kept),
            }))
        }
    }

    /// The canonical query graph node for `type_name` within `source`,
    /// preferring one that is neither a `@provides` copy nor a root.
    fn connector_landing_node(&self, source: &str, type_name: &Name) -> Option<NodeIndex> {
        let qg = self.qg();
        let all_nodes = qg.nodes_for_type(type_name).ok()?;
        let source_nodes: Vec<NodeIndex> = all_nodes
            .iter()
            .copied()
            .filter(|node| {
                qg.node_weight(*node)
                    .map(|w| w.source.as_ref() == source)
                    .unwrap_or(false)
            })
            .collect();
        if source_nodes.is_empty() {
            return None;
        }
        source_nodes
            .iter()
            .copied()
            .find(|node| {
                qg.node_weight(*node)
                    .map(|weight| weight.provide_id.is_none() && weight.root_kind.is_none())
                    .unwrap_or(false)
            })
            .or_else(|| source_nodes.first().copied())
    }
}
