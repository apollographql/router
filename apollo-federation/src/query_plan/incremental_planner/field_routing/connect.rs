//! Connector commit logic for the BULB planner: creating connector fetch
//! groups, partitioning sub-selections against the connector's output
//! shape, and re-routing selections the connector does not provide.

use std::sync::Arc;

use apollo_compiler::Name;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use shape::Shape;
use tracing::trace;

use super::super::defer::extract_defer_label;
use super::super::defer::strip_defer_directive;
use super::super::fetch_graph::FetchGroupKind;
use super::super::fetch_graph::InputContribution;
use super::super::fetch_graph::InputRewriteInfo;
use super::super::shared_path::SharedPath;
use super::FieldRoutingSearchSpace;
use super::commit::field_response_elements;
use super::requires::condition_directives;
use super::requires::gated_input_path;
use super::requires::trailing_condition_fragments;
use super::requires::trailing_conditions_context;
use super::requires::unconditioned_input_path;
use super::routing::IntermediateKeyHop;
use super::routing::RoutingChoice;
use super::state::PendingSelection;
use super::state::PlanState;
use crate::connectors::ConnectorPosition;
use crate::connectors::EntityResolver;
use crate::connectors::index::IndexedConnector;
use crate::connectors::index::object_fields;
use crate::connectors::index::shape_provides;
use crate::connectors::index::split_selections;
use crate::error::FederationError;
use crate::operation::DirectiveList;
use crate::operation::FieldSelection;
use crate::operation::InlineFragment;
use crate::operation::InlineFragmentSelection;
use crate::operation::Selection;
use crate::operation::SelectionId;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::query_graph::QueryGraphEdgeTransition;
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
    /// The defer label the selection is under, including any @defer fragment
    /// crossed during partitioning.
    defer_ref: Option<String>,
}

/// The `deferred` selections, each `op_path_len` elements or more below the
/// connector fetch's root, as one selection set shaped like `selections`.
fn deferred_below(
    selections: &SelectionSet,
    op_path_len: usize,
    deferred: &[DeferredConnectorSelection],
) -> Result<SelectionSet, FederationError> {
    let mut below =
        SelectionSet::empty(selections.schema.clone(), selections.type_position.clone());
    for child in deferred {
        let mut path: Vec<_> = child.op_path.iter().skip(op_path_len).cloned().collect();
        let sub_selections = match &child.selection {
            Selection::Field(field_sel) => {
                path.push(Arc::new(OpPathElement::Field(field_sel.field.clone())));
                field_sel.selection_set.clone().map(Arc::new)
            }
            Selection::InlineFragment(fragment_sel) => {
                path.push(Arc::new(OpPathElement::InlineFragment(
                    fragment_sel.inline_fragment.clone(),
                )));
                Some(Arc::new(fragment_sel.selection_set.clone()))
            }
        };
        below.add_at_path_for_incremental_planner(&path, sub_selections.as_ref())?;
    }
    Ok(below)
}

/// `selection` re-wrapped in the type conditions and directives of
/// `fragments` (outermost first), e.g. `email` under `... @include(if: $x)`.
/// Fragments with neither, such as a stripped `@defer`, are dropped.
fn wrap_in_fragments(selection: Selection, fragments: &[Arc<OpPathElement>]) -> Selection {
    fragments
        .iter()
        .rev()
        .fold(selection, |selection, element| match element.as_ref() {
            OpPathElement::InlineFragment(fragment)
                if fragment.type_condition_position.is_some()
                    || !fragment.directives.is_empty() =>
            {
                Selection::InlineFragment(Arc::new(InlineFragmentSelection {
                    inline_fragment: fragment.clone(),
                    selection_set: SelectionSet::from_selection(fragment.casted_type(), selection),
                }))
            }
            _ => selection,
        })
}

/// What a connector fetch returns at a path within it.
enum ConnectorOutputAt {
    /// The connector's `selection` output at the path.
    Shape(Shape),
    /// The parent of a root connector's or field-level resolver's own field:
    /// the fetch returns only that field (and `__typename`) there.
    OwnFieldOnly(Name),
}

/// The op path of a connector entity fetch for `pending`: the entity root plus
/// the `@skip`/`@include` conditions the pending sits under, so that the
/// fetch is gated on them.
fn with_trailing_conditions(
    entity_root: SharedPath<Arc<OpPathElement>>,
    pending: &PendingSelection,
) -> SharedPath<Arc<OpPathElement>> {
    trailing_condition_fragments(&pending.op_path)
        .into_iter()
        .fold(entity_root, |path, element| path.pushed(element))
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
        entry: &Arc<IndexedConnector>,
        choice: &RoutingChoice,
    ) -> Result<(), FederationError> {
        let connector = &entry.connector;
        let source_subgraph = &entry.source_subgraph;
        trace!(
            source_subgraph = %source_subgraph,
            coordinate = %entry.coordinate,
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
            if let Some(key_conditions) = choice.connector_key_conditions() {
                // Entity-resolver connector: key fields ride the parent fetch;
                // the connector group merges at the pending's response path.
                let source = self.node_source(pending.query_graph_node)?;
                let key_locally_resolvable =
                    self.can_resolve_in_place(pending.query_graph_node, key_conditions, &source)?;
                let (local_key, routed_key) =
                    self.split_anchor_key(state, pending, key_conditions, key_locally_resolvable)?;
                // The connector fetch is gated on the trailing conditions
                // (`with_trailing_conditions` below), so its inputs keep them,
                // and so does its representation, in case the fetch merges
                // with a differently gated one.
                self.append_entity_inputs_at(
                    state,
                    pending.fetch_node,
                    &gated_input_path(&pending.op_path),
                    local_key.as_ref(),
                    &source,
                );
                let needed_under = trailing_conditions_context(&pending.op_path)?;

                let merge_at = self.pending_merge_at(state, pending);

                let new_group = state.graph.add_connector_entity_group(
                    source_subgraph,
                    merge_at,
                    connector.clone(),
                    child_defer_ref,
                );
                // The entity type as the connector subgraph knows it. It
                // differs from the source type when the source is an
                // implementation of an @interfaceObject.
                let entity_type = key_conditions.type_position.clone();
                let inputs = vec![InputContribution::Key {
                    source_type_name: source.type_pos.type_name().clone(),
                    conditions: key_conditions.clone(),
                    rewrite_info: InputRewriteInfo {
                        dest_type: entity_type.clone(),
                        dest_subgraph: source_subgraph.clone(),
                    },
                    needed_under: needed_under.clone(),
                }];
                state
                    .graph
                    .add_dependency(pending.fetch_node, new_group, inputs);

                if let Some(routed_key) = &routed_key {
                    self.push_condition_pendings(state, pending, routed_key, new_group)?;
                }

                let entity_base_path = self.entity_root_path(entity_type.type_name())?;
                (
                    new_group,
                    with_trailing_conditions(entity_base_path, pending).pushed(field_op_element),
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
                    with_trailing_conditions(entity_base_path, pending).pushed(field_op_element),
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
            let output = self
                .connector_index
                .output_shape(&entry.coordinate)
                .and_then(|shape| Some((shape, object_fields(shape)?)));
            let kept = match &output {
                Some((shape, fields)) => {
                    // Entity-resolver connectors describe the entity-level
                    // output (e.g. User), but we are partitioning the
                    // sub-selections of the committed field (e.g.
                    // newsletterSubscriptions). Drill into the field's
                    // sub-shape so the partition checks the right level.
                    let effective_shape = fields
                        .get(field_sel.field.name().as_str())
                        .filter(|child_shape| object_fields(child_shape).is_some())
                        .unwrap_or(shape);
                    self.partition_connector_selections(
                        &restored,
                        effective_shape,
                        &landing_type,
                        source_subgraph,
                        &child_op_path,
                        &child_response_path,
                        &defer_for_children,
                        &mut deferred,
                    )?
                }
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
                    coordinate = %entry.coordinate,
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
                        .with_defer(deferred_selection.defer_ref)
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
        defer_ref: &Option<String>,
        deferred: &mut Vec<DeferredConnectorSelection>,
    ) -> Result<Option<SelectionSet>, FederationError> {
        let Some(fields) = object_fields(shape) else {
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
                                defer_ref: defer_ref.clone(),
                            }),
                            None => {
                                kept.insert(selection.clone());
                            }
                        }
                        continue;
                    };
                    match &field_sel.selection_set {
                        Some(child_ss) if !child_ss.is_empty() => {
                            if object_fields(child_shape).is_none() {
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
                            let mut child_deferred = Vec::new();
                            let child_kept = self.partition_connector_selections(
                                child_ss,
                                child_shape,
                                &child_type,
                                source_subgraph,
                                &child_op,
                                &child_response,
                                defer_ref,
                                &mut child_deferred,
                            )?;
                            // Nothing routes a selection on a value type the
                            // connector subgraph can't re-enter, so the
                            // field's missing part is fetched from its parent.
                            let lift_anchor = (!child_deferred.is_empty()
                                && child_deferred
                                    .iter()
                                    .all(|child| child.defer_ref == *defer_ref)
                                && !self.connector_reenters(source_subgraph, &child_type))
                            .then(|| self.connector_landing_node(source_subgraph, parent_type))
                            .flatten();
                            match lift_anchor {
                                Some(anchor_node) => {
                                    let missing =
                                        deferred_below(child_ss, child_op.len(), &child_deferred)?;
                                    deferred.push(DeferredConnectorSelection {
                                        selection: Selection::Field(Arc::new(FieldSelection {
                                            field: field_sel.field.clone(),
                                            selection_set: Some(missing),
                                        })),
                                        parent_type: parent_type.clone(),
                                        anchor_node,
                                        op_path: op_path.clone(),
                                        response_path: response_path.clone(),
                                        defer_ref: defer_ref.clone(),
                                    });
                                }
                                None => match self
                                    .connector_landing_node(source_subgraph, parent_type)
                                {
                                    Some(parent_anchor) => self.lift_unkeyed_child_selections(
                                        field_sel,
                                        child_ss,
                                        child_shape,
                                        child_op.len(),
                                        child_deferred,
                                        parent_type,
                                        parent_anchor,
                                        op_path,
                                        response_path,
                                        deferred,
                                    )?,
                                    None => deferred.extend(child_deferred),
                                },
                            }
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
                    // The connector cannot resolve a type condition its subgraph
                    // does not define (e.g. an implementation of an
                    // @interfaceObject), so regular routing takes the fragment
                    // from the parent's node.
                    if fragment_type != *parent_type
                        && self
                            .connector_landing_node(source_subgraph, &fragment_type)
                            .is_none()
                        && let Some(anchor_node) =
                            self.connector_landing_node(source_subgraph, parent_type)
                    {
                        deferred.push(DeferredConnectorSelection {
                            selection: selection.clone(),
                            parent_type: parent_type.clone(),
                            anchor_node,
                            op_path: op_path.clone(),
                            response_path: response_path.clone(),
                            defer_ref: defer_ref.clone(),
                        });
                        continue;
                    }
                    let child_op = op_path.pushed(Arc::new(OpPathElement::InlineFragment(
                        strip_defer_directive(&fragment_sel.inline_fragment),
                    )));
                    let child_defer = extract_defer_label(&fragment_sel.inline_fragment)
                        .or_else(|| defer_ref.clone());
                    let child_kept = self.partition_connector_selections(
                        &fragment_sel.selection_set,
                        shape,
                        &fragment_type,
                        source_subgraph,
                        &child_op,
                        response_path,
                        &child_defer,
                        deferred,
                    )?;
                    if let Some(child_kept) = child_kept
                        && !child_kept.is_empty()
                    {
                        kept.insert(Selection::InlineFragment(Arc::new(
                            InlineFragmentSelection {
                                inline_fragment: strip_defer_directive(
                                    &fragment_sel.inline_fragment,
                                ),
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

    /// Re-fetch `field_sel` from `parent_type` with the `child_deferred`
    /// selections no key the connector returns at the child can resolve, e.g.
    /// `author { email }` on `Post` when the connector returns `author` with
    /// no `id`. One re-fetch per defer label; the other selections stay
    /// deferred at the child.
    #[allow(clippy::too_many_arguments)]
    fn lift_unkeyed_child_selections(
        &self,
        field_sel: &FieldSelection,
        child_ss: &SelectionSet,
        child_shape: &Shape,
        child_op_len: usize,
        child_deferred: Vec<DeferredConnectorSelection>,
        parent_type: &Name,
        parent_anchor: NodeIndex,
        op_path: &SharedPath<Arc<OpPathElement>>,
        response_path: &SharedPath<FetchDataPathElement>,
        deferred: &mut Vec<DeferredConnectorSelection>,
    ) -> Result<(), FederationError> {
        // Per defer label: the lifted selections and the @skip/@include all
        // of them are under.
        let mut lifted: Vec<(Option<String>, SelectionSet, DirectiveList)> = Vec::new();
        for item in child_deferred {
            // Fragments between the child and the item
            // (`... @include(if: $x) { email }`) keep it at the child's
            // object level.
            let fragments: Vec<_> = item.op_path.iter().skip(child_op_len).cloned().collect();
            let at_child_level = fragments
                .iter()
                .all(|element| matches!(element.as_ref(), OpPathElement::InlineFragment(_)));
            if !at_child_level
                || self.shape_provides_some_key(
                    &item.parent_type,
                    item.anchor_node,
                    child_shape,
                    &item.selection,
                )
            {
                deferred.push(item);
                continue;
            }
            let item_conditions: DirectiveList = fragments
                .iter()
                .flat_map(|element| match element.as_ref() {
                    OpPathElement::InlineFragment(fragment) => {
                        condition_directives(&fragment.directives)
                            .iter()
                            .cloned()
                            .collect()
                    }
                    OpPathElement::Field(_) => Vec::new(),
                })
                .collect();
            let index = match lifted.iter().position(|(r, ..)| *r == item.defer_ref) {
                Some(index) => {
                    let shared = &mut lifted[index].2;
                    *shared = shared
                        .iter()
                        .filter(|condition| item_conditions.iter().any(|c| c == *condition))
                        .cloned()
                        .collect();
                    index
                }
                None => {
                    lifted.push((
                        item.defer_ref,
                        SelectionSet::empty(
                            child_ss.schema.clone(),
                            child_ss.type_position.clone(),
                        ),
                        item_conditions,
                    ));
                    lifted.len() - 1
                }
            };
            // Merged, so that items under the same fragment share one entry.
            lifted[index]
                .1
                .add_local_selection(&wrap_in_fragments(item.selection, &fragments))?;
        }
        for (defer_ref, selection_set, shared_conditions) in lifted {
            // The re-fetch is needed only when the @skip/@include every item
            // is under hold, so the field is lifted under them, and the key
            // hop gates the re-fetch on them. The items keep their own
            // fragments, which gate the fetches below the field.
            let op_path = if shared_conditions.is_empty() {
                op_path.clone()
            } else {
                op_path.pushed(Arc::new(OpPathElement::InlineFragment(InlineFragment {
                    schema: field_sel.field.schema.clone(),
                    parent_type_position: field_sel.field.field_position.parent(),
                    type_condition_position: None,
                    directives: shared_conditions,
                    selection_id: SelectionId::new(),
                })))
            };
            deferred.push(DeferredConnectorSelection {
                selection: Selection::Field(Arc::new(FieldSelection {
                    field: field_sel.field.clone(),
                    selection_set: Some(selection_set),
                })),
                parent_type: parent_type.clone(),
                anchor_node: parent_anchor,
                op_path,
                response_path: response_path.clone(),
                defer_ref,
            });
        }
        Ok(())
    }

    /// What the connector backing `fetch_node` returns at `op_path` (relative
    /// to the fetch). `None` when the fetch is not a connector fetch, or the
    /// path leaves the indexed shape.
    fn connector_output_at(
        &self,
        state: &PlanState,
        fetch_node: NodeIndex,
        op_path: &SharedPath<Arc<OpPathElement>>,
    ) -> Option<ConnectorOutputAt> {
        let node = state.graph.node(fetch_node);
        let connector = node.connector.as_ref()?;
        let mut shape = self
            .connector_index
            .output_shape(&connector.id.coordinate())?
            .clone();
        let mut path_fields = op_path.iter().filter_map(|element| match element.as_ref() {
            OpPathElement::Field(field) => Some(field),
            OpPathElement::InlineFragment(_) => None,
        });
        // A root connector, or a field-level entity resolver, describes the
        // value of its own field, which is the first field on the path.
        // Type-level and `entity: true` resolvers describe the entity.
        let describes_own_field =
            matches!(
                node.kind,
                FetchGroupKind::Root { .. } | FetchGroupKind::RootHop { .. }
            ) || matches!(connector.entity_resolver, Some(EntityResolver::Implicit));
        if describes_own_field && path_fields.next().is_none() {
            // The path stops at the own field's parent.
            let ConnectorPosition::Field(pos) = &connector.id.directive else {
                return None;
            };
            return Some(ConnectorOutputAt::OwnFieldOnly(
                pos.field.field_name().clone(),
            ));
        }
        for field in path_fields {
            let child = object_fields(&shape)?.get(field.name().as_str())?.clone();
            shape = child;
        }
        Some(ConnectorOutputAt::Shape(shape))
    }

    /// The top-level fields of `conditions` the fetch at `fetch_node` does not
    /// return at `op_path`, paired with the ones it does (`None` when that part
    /// is empty). A connector fetch returns only what its `selection` maps,
    /// whatever the subgraph schema declares. `None` when the fetch returns
    /// every field, which a GraphQL fetch always does.
    pub(super) fn connector_unprovided(
        &self,
        state: &PlanState,
        fetch_node: NodeIndex,
        op_path: &SharedPath<Arc<OpPathElement>>,
        conditions: &SelectionSet,
    ) -> Option<(Option<Arc<SelectionSet>>, Arc<SelectionSet>)> {
        let output =
            self.connector_output_at(state, fetch_node, &unconditioned_input_path(op_path))?;
        let (provided, missing) = match output {
            ConnectorOutputAt::Shape(shape) => {
                split_selections(conditions, |selection| shape_provides(&shape, selection))
            }
            ConnectorOutputAt::OwnFieldOnly(own_field) => {
                split_selections(conditions, |selection| {
                    selection.is_typename_field()
                        || matches!(selection, Selection::Field(field_sel)
                            if *field_sel.field.name() == own_field)
                })
            }
        };
        Some((provided.map(Arc::new), Arc::new(missing?)))
    }

    /// The entity-resolver connector a chain enters `hop`'s connector subgraph
    /// through: one keyed on the hop's entry key, preferring one whose output
    /// returns the key leaving the hop.
    pub(super) fn chain_hop_resolver(
        &self,
        hop: &IntermediateKeyHop,
        exit_key: Option<&Arc<SelectionSet>>,
    ) -> Result<Arc<IndexedConnector>, FederationError> {
        let node = self.qg().node_weight(hop.target_node)?;
        let type_pos: CompositeTypeDefinitionPosition = node.type_.clone().try_into()?;
        let keyed: Vec<&Arc<IndexedConnector>> = self
            .connector_index
            .entity_resolvers(type_pos.type_name())
            .into_iter()
            .flatten()
            .filter(|entry| {
                entry.source_subgraph == node.source
                    && hop.entry_key.as_ref().is_some_and(|entry_key| {
                        self.connector_index
                            .key_conditions(&entry.coordinate)
                            .is_some_and(|key| key == entry_key)
                    })
            })
            .collect();
        let returns_exit_key = |entry: &&Arc<IndexedConnector>| {
            exit_key.is_none_or(|exit_key| {
                exit_key.selections.values().all(|selection| {
                    self.connector_index
                        .resolver_provides(&entry.coordinate, selection)
                })
            })
        };
        keyed
            .iter()
            .find(|entry| returns_exit_key(entry))
            .or_else(|| keyed.first())
            .map(|entry| (*entry).clone())
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "no entity-resolver connector enters {} at {}",
                    node.source,
                    type_pos.type_name(),
                ))
            })
    }

    /// Whether a selection on `type_name` within `source` can be routed on
    /// its own: through an entity-resolver connector for the type, or a key
    /// leaving its node.
    fn connector_reenters(&self, source: &str, type_name: &Name) -> bool {
        if self
            .connector_index
            .entity_resolvers(type_name)
            .is_some_and(|resolvers| !resolvers.is_empty())
        {
            return true;
        }
        self.connector_landing_node(source, type_name)
            .is_some_and(|node| {
                self.qg().out_edges(node).into_iter().any(|edge| {
                    matches!(
                        edge.weight().transition,
                        QueryGraphEdgeTransition::KeyResolution
                    )
                })
            })
    }

    /// Entity-resolver connectors for `type_name`, including those on the
    /// interfaces it implements (an `@interfaceObject` in a connector
    /// subgraph resolves its implementations).
    fn entity_resolvers_for<'a>(
        &'a self,
        type_name: &'a Name,
    ) -> impl Iterator<Item = &'a Arc<IndexedConnector>> {
        let interfaces = self
            .supergraph_schema
            .schema()
            .get_object(type_name)
            .into_iter()
            .flat_map(|object| object.implements_interfaces.iter().map(|itf| &**itf));
        std::iter::once(type_name)
            .chain(interfaces)
            .filter_map(|ty| self.connector_index.entity_resolvers(ty))
            .flatten()
    }

    /// Whether the entity-resolver connector `entry` returns the field
    /// `selection` with a key into the field's type that resolves some of its
    /// sub-selection. A hop through `entry` then makes progress by reaching
    /// that key, as when a field is re-fetched from its parent because its
    /// own connector returned no key (`author { email }` through a `Post`
    /// resolver that returns `author { id }`).
    pub(super) fn resolver_returns_child_key(
        &self,
        entry: &IndexedConnector,
        selection: &Selection,
    ) -> bool {
        let Selection::Field(field_sel) = selection else {
            return false;
        };
        let Some(child_ss) = &field_sel.selection_set else {
            return false;
        };
        let Some(child_shape) = self
            .connector_index
            .resolver_field_shape(&entry.coordinate, field_sel.field.name().as_str())
        else {
            return false;
        };
        let Ok(field_def) = field_sel
            .field
            .field_position
            .get(field_sel.field.schema.schema())
        else {
            return false;
        };
        let child_type = field_def.ty.inner_named_type();
        let Some(child_anchor) = self.connector_landing_node(&entry.source_subgraph, child_type)
        else {
            return false;
        };
        child_ss.selections.values().any(|sub| {
            !sub.is_typename_field()
                && self.shape_provides_some_key(child_type, child_anchor, &child_shape, sub)
        })
    }

    /// Whether some key that enters `type_name` from `anchor_node` can be
    /// read from `shape`: a connector entity resolver's key, or a key edge
    /// into a GraphQL subgraph.
    fn shape_provides_some_key(
        &self,
        type_name: &Name,
        anchor_node: NodeIndex,
        shape: &Shape,
        selection: &Selection,
    ) -> bool {
        let provides_key = |key: &SelectionSet| {
            key.selections
                .values()
                .all(|key_selection| shape_provides(shape, key_selection))
        };
        // Only a key into something that resolves the deferred selection
        // counts, by the same test `push_entity_resolver_options` routes it
        // with: a resolver that returns `friend { id }` resolves
        // `friend { email }` through that key.
        let resolver_key = self
            .entity_resolvers_for(type_name)
            .filter(|entry| {
                self.connector_index
                    .resolver_provides_any(&entry.coordinate, selection)
                    || self.resolver_returns_child_key(entry, selection)
            })
            .filter_map(|entry| self.connector_index.key_conditions(&entry.coordinate))
            .any(|key| provides_key(key));
        if resolver_key {
            return true;
        }
        let field = match selection {
            Selection::Field(field_sel) => Some(&field_sel.field),
            Selection::InlineFragment(_) => None,
        };
        let qg = self.qg();
        qg.out_edges(anchor_node).into_iter().any(|edge| {
            let weight = edge.weight();
            matches!(weight.transition, QueryGraphEdgeTransition::KeyResolution)
                && weight.conditions.as_ref().is_some_and(|key| {
                    qg.node_weight(edge.target()).is_ok_and(|target| {
                        !self.connector_index.is_connector_subgraph(&target.source)
                    }) && field.is_none_or(|field| {
                        self.cached_query_graph
                            .edge_for_field(edge.target(), field)
                            .is_some()
                    }) && provides_key(key)
                })
        })
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
