//! Materializing the winning fetch graph into a query plan: a topological
//! sort into depth layers, one subgraph operation per fetch group, entity
//! representations from edge inputs, and @defer partitioning.

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::executable;
use apollo_compiler::executable::VariableDefinition;
use indexmap::IndexMap;
use petgraph::Direction;
use petgraph::algo::toposort;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use petgraph::visit::NodeIndexable;

use super::super::defer::DeferInfo;
use super::FETCH_COST;
use super::FetchGraph;
use super::FetchGroupKind;
use super::PIPELINING_COST;
use crate::error::FederationError;
use crate::operation::DirectiveList;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::operation::VariableCollector;
use crate::query_graph::QueryGraph;
use crate::query_graph::graph_path::operation::OpGraphPathContext;
use crate::query_plan::DeferNode;
use crate::query_plan::DeferredDeferBlock;
use crate::query_plan::DeferredDependency;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::PlanNode;
use crate::query_plan::PrimaryDeferBlock;
use crate::query_plan::QueryPlanCost;
use crate::query_plan::conditions::ConditionKind;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::conditions::remove_conditions_from_selection_set;
use crate::query_plan::fetch_dependency_graph::compute_input_rewrites_on_key_fetch;
use crate::query_plan::fetch_dependency_graph::operation_for_entities_fetch;
use crate::query_plan::fetch_dependency_graph::operation_for_query_fetch;
use crate::query_plan::fetch_dependency_graph::wrap_input_selections;
use crate::query_plan::fetch_dependency_graph_processor::to_valid_graphql_name;
use crate::query_plan::query_planner::SubgraphOperationCompression;
use crate::query_plan::requires_selection;
use crate::query_plan::serializable_document::SerializableDocument;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;

/// Everything plan generation needs from the surrounding operation.
pub(crate) struct PlanBuildContext<'a> {
    pub(crate) supergraph_schema: &'a ValidFederationSchema,
    pub(crate) query_graph: &'a Arc<QueryGraph>,
    pub(crate) root_kind: SchemaRootDefinitionKind,
    pub(crate) variable_definitions: &'a [Node<VariableDefinition>],
    pub(crate) operation_directives: &'a DirectiveList,
    pub(crate) operation_name: &'a Option<Name>,
    pub(crate) operation_compression: &'a mut SubgraphOperationCompression,
    /// Numbers generated subgraph operations (`{name}__{subgraph}__{n}`).
    pub(crate) operation_counter: u32,
}

/// Stamp a fetch ID on the innermost FetchNode (bare or Flatten-wrapped).
fn stamp_fetch_id(plan_node: &mut PlanNode, id: u64) {
    match plan_node {
        PlanNode::Fetch(fetch) => {
            fetch.id = Some(id);
        }
        PlanNode::Flatten(flatten) => {
            stamp_fetch_id(&mut flatten.node, id);
        }
        _ => {}
    }
}

impl FetchGraph {
    /// Generate a PlanNode tree, wrapped in a DeferNode when defer info is
    /// provided.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn to_query_plan_with_defer(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        defer_info: Option<&DeferInfo>,
    ) -> Result<(Option<PlanNode>, QueryPlanCost), FederationError> {
        // Step 1: Topological sort and depth computation.
        let sorted = toposort(&self.graph, None).map_err(|cycle| {
            let node = cycle.node_id();
            let subgraph = &self.graph[node].subgraph;
            FederationError::internal(format!(
                "cycle in FetchGraph at node {:?} ({})",
                node, subgraph,
            ))
        })?;

        if sorted.is_empty() {
            return Ok((None, 0.0));
        }

        let mut depth = vec![0u32; self.graph.node_bound()];
        let mut max_depth: u32 = 0;
        for &node in &sorted {
            let d = self
                .graph
                .edges_directed(node, Direction::Incoming)
                .map(|e| depth[e.source().index()] + 1)
                .max()
                .unwrap_or(0);
            depth[node.index()] = d;
            if d > max_depth {
                max_depth = d;
            }
        }

        // A Defer wrapper is needed whenever the operation has @defer
        // blocks, even if no fetch node carries a defer_ref: a deferred
        // selection whose data rides the primary fetches still needs a
        // data-only block so the router delivers it as a separate chunk.
        let has_defer_blocks = defer_info.is_some_and(|di| !di.blocks.is_empty());

        if !has_defer_blocks {
            return self.build_plan_for_nodes(ctx, &sorted, &depth, max_depth, None);
        }

        let defer_info = defer_info.unwrap();

        // Step 2: Partition nodes by defer_ref.
        let mut primary_nodes: Vec<NodeIndex> = Vec::new();
        let mut deferred_nodes: IndexMap<String, Vec<NodeIndex>> = IndexMap::new();
        for &node_idx in &sorted {
            match &self.graph[node_idx].defer_ref {
                None => primary_nodes.push(node_idx),
                Some(label) => {
                    deferred_nodes
                        .entry(label.clone())
                        .or_default()
                        .push(node_idx);
                }
            }
        }

        let mut fetch_id_counter = 0u64;

        // Step 3: Assign fetch IDs to nodes that parent a node with a
        // different defer_ref (None->Some and Some->Some alike), covering
        // primary-to-deferred and deferred-to-nested-deferred edges.
        let mut node_fetch_ids: HashMap<NodeIndex, u64> = HashMap::new();
        for &node_idx in &sorted {
            let this_defer = self.graph[node_idx].defer_ref.as_deref();
            for edge in self.graph.edges_directed(node_idx, Direction::Outgoing) {
                let child_defer = self.graph[edge.target()].defer_ref.as_deref();
                if child_defer != this_defer {
                    node_fetch_ids.entry(node_idx).or_insert_with(|| {
                        let id = fetch_id_counter;
                        fetch_id_counter += 1;
                        id
                    });
                    break;
                }
            }
        }

        // Every label in the operation gets a block, even with no fetch
        // nodes of its own: data riding an enclosing fetch still needs a
        // data-only block (node: None). Labels with fetch nodes keep their
        // deterministic (commit-order) position; node-less labels are
        // appended in sorted order.
        let all_labels: Vec<String> = {
            let mut labels: Vec<String> = deferred_nodes.keys().cloned().collect();
            let mut node_less: Vec<&String> = defer_info
                .blocks
                .keys()
                .filter(|label| !deferred_nodes.contains_key(*label))
                .collect();
            node_less.sort();
            labels.extend(node_less.into_iter().cloned());
            labels
        };
        let top_level_labels: Vec<String> = all_labels
            .iter()
            .filter(|label| {
                defer_info
                    .blocks
                    .get(label.as_str())
                    .and_then(|bi| bi.parent_label.as_ref())
                    .is_none()
            })
            .cloned()
            .collect();

        // Build child label index: parent_label -> [child_labels]
        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        for label in &all_labels {
            if let Some(parent) = defer_info
                .blocks
                .get(label.as_str())
                .and_then(|bi| bi.parent_label.as_ref())
            {
                children_of
                    .entry(parent.clone())
                    .or_default()
                    .push(label.clone());
            }
        }

        // Step 4: Build primary plan.
        let (primary_plan, total_cost) = self.build_plan_for_nodes(
            ctx,
            &primary_nodes,
            &depth,
            max_depth,
            Some(&node_fetch_ids),
        )?;

        // Step 5: Recursively build deferred blocks.
        let deferred_blocks = self.build_deferred_blocks(
            ctx,
            &top_level_labels,
            &deferred_nodes,
            defer_info,
            &node_fetch_ids,
            &children_of,
            &depth,
            max_depth,
        )?;

        let primary_sub_selection = defer_info
            .primary_sub_selection
            .as_deref()
            .map(|s| s.to_owned());

        let defer_node = PlanNode::Defer(DeferNode {
            primary: PrimaryDeferBlock {
                sub_selection: primary_sub_selection,
                node: primary_plan.map(Box::new),
            },
            deferred: deferred_blocks,
        });

        Ok((Some(defer_node), total_cost))
    }

    /// Build a plan from a subset of nodes (shared by primary and deferred
    /// block plans).
    #[allow(clippy::too_many_arguments)]
    fn build_plan_for_nodes(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        nodes: &[NodeIndex],
        depth: &[u32],
        max_depth: u32,
        fetch_ids: Option<&HashMap<NodeIndex, u64>>,
    ) -> Result<(Option<PlanNode>, QueryPlanCost), FederationError> {
        let handled_conditions = Conditions::Boolean(true);
        let mut sequence: Vec<PlanNode> = Vec::new();
        let mut cost_sequence: Vec<QueryPlanCost> = Vec::new();

        for d in 0..=max_depth {
            let mut parallel: Vec<PlanNode> = Vec::new();
            let mut parallel_cost: QueryPlanCost = 0.0;

            for &node_idx in nodes {
                if depth[node_idx.index()] != d {
                    continue;
                }
                if let Some((mut plan_node, node_cost)) =
                    self.node_to_plan_node(ctx, node_idx, &handled_conditions)?
                {
                    // Fetch IDs are used for defer dependency tracking.
                    if let Some(ids) = fetch_ids
                        && let Some(&fetch_id) = ids.get(&node_idx)
                    {
                        stamp_fetch_id(&mut plan_node, fetch_id);
                    }
                    parallel.push(plan_node);
                    parallel_cost += node_cost;
                }
            }

            match parallel.len() {
                0 => {}
                1 => sequence.push(parallel.pop().unwrap()),
                _ => sequence.push(PlanNode::Parallel(crate::query_plan::ParallelNode {
                    nodes: parallel,
                })),
            }
            if parallel_cost > 0.0 {
                cost_sequence.push(parallel_cost);
            }
        }

        let total_cost: QueryPlanCost = cost_sequence
            .iter()
            .enumerate()
            .map(|(i, &stage)| stage * (1.0f64).max(i as f64 * PIPELINING_COST))
            .sum();

        let plan = match sequence.len() {
            0 => None,
            1 => Some(sequence.pop().unwrap()),
            _ => Some(PlanNode::Sequence(crate::query_plan::SequenceNode {
                nodes: sequence,
            })),
        };

        Ok((plan, total_cost))
    }

    /// Recursively build `DeferredDeferBlock`s for a set of labels; a label
    /// with children (nested @defer) wraps its fetch nodes and child blocks
    /// in a nested `DeferNode`.
    #[allow(clippy::too_many_arguments)]
    fn build_deferred_blocks(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        labels: &[String],
        deferred_nodes: &IndexMap<String, Vec<NodeIndex>>,
        defer_info: &DeferInfo,
        node_fetch_ids: &HashMap<NodeIndex, u64>,
        children_of: &HashMap<String, Vec<String>>,
        depth: &[u32],
        max_depth: u32,
    ) -> Result<Vec<DeferredDeferBlock>, FederationError> {
        let mut blocks: Vec<DeferredDeferBlock> = Vec::new();

        for label in labels {
            // A label with no fetch nodes (data rides an enclosing fetch)
            // is emitted with node: None; the router delivers the chunk
            // from already-fetched data via the block's sub_selection.
            let nodes = deferred_nodes.get(label).map(Vec::as_slice).unwrap_or(&[]);
            let block_info = defer_info.blocks.get(label.as_str());
            let child_labels = children_of.get(label);

            // Dependencies: parent-scope nodes feeding this label's nodes.
            let mut depends: Vec<DeferredDependency> = Vec::new();
            for &deferred_idx in nodes {
                for edge in self.graph.edges_directed(deferred_idx, Direction::Incoming) {
                    let parent_idx = edge.source();
                    if let Some(&fetch_id) = node_fetch_ids.get(&parent_idx)
                        && !depends.iter().any(|d| d.id == fetch_id.to_string())
                    {
                        depends.push(DeferredDependency {
                            id: fetch_id.to_string(),
                        });
                    }
                }
            }

            let query_path = block_info
                .map(|bi| bi.query_path.clone())
                .unwrap_or_default();

            let visible_label = if defer_info.assigned_labels.contains(label.as_str()) {
                None
            } else {
                Some(label.clone())
            };

            let node_plan = if let Some(child_labels) = child_labels
                && !child_labels.is_empty()
            {
                let (inner_plan, _cost) =
                    self.build_plan_for_nodes(ctx, nodes, depth, max_depth, Some(node_fetch_ids))?;
                let inner_deferred = self.build_deferred_blocks(
                    ctx,
                    child_labels,
                    deferred_nodes,
                    defer_info,
                    node_fetch_ids,
                    children_of,
                    depth,
                    max_depth,
                )?;

                // The outer block's sub_selection describes the whole chunk;
                // the nested DeferNode's primary carries none (its deferred
                // children re-select their pieces via their blocks).
                let nested_defer = PlanNode::Defer(DeferNode {
                    primary: PrimaryDeferBlock {
                        sub_selection: None,
                        node: inner_plan.map(Box::new),
                    },
                    deferred: inner_deferred,
                });
                Some(Box::new(nested_defer))
            } else if nodes.is_empty() {
                None
            } else {
                let (deferred_plan, _cost) =
                    self.build_plan_for_nodes(ctx, nodes, depth, max_depth, None)?;
                deferred_plan.map(Box::new)
            };

            // For nested DeferNodes, sub_selection was consumed above;
            // leaf blocks use it directly.
            let sub_selection = if child_labels.is_some_and(|c| !c.is_empty()) {
                None
            } else {
                block_info
                    .and_then(|bi| bi.sub_selection.as_deref())
                    .map(|s| s.to_owned())
            };

            blocks.push(DeferredDeferBlock {
                depends,
                label: visible_label,
                query_path,
                sub_selection,
                node: node_plan,
            });
        }

        Ok(blocks)
    }

    /// Convert a single FetchGraph node into a PlanNode.
    fn node_to_plan_node(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        node_idx: NodeIndex,
        handled_conditions: &Conditions,
    ) -> Result<Option<(PlanNode, QueryPlanCost)>, FederationError> {
        let node = &self.graph[node_idx];
        let is_entity = matches!(node.kind, FetchGroupKind::Entity { .. });
        let subgraph_schema = ctx.query_graph.schema_by_source(&node.subgraph)?;

        // Parent type for the selection set; selections are materialized
        // against the subgraph schema (add_at_path rebases supergraph
        // OpPathElements onto it).
        let parent_type: CompositeTypeDefinitionPosition = match &node.kind {
            FetchGroupKind::Root { root_type } | FetchGroupKind::RootHop { root_type, .. } => {
                root_type.clone()
            }
            FetchGroupKind::Entity { .. } => subgraph_schema
                .entity_type()?
                .ok_or_else(|| {
                    FederationError::internal(format!(
                        "Subgraph `{}` has no entities defined",
                        node.subgraph
                    ))
                })?
                .into(),
        };

        // 1. Materialize selection from the builder.
        let mut selection_set = SelectionSet::empty(subgraph_schema.clone(), parent_type.clone());
        for entry in node.selection_builder.entries() {
            let path_vec = entry.path().to_vec();
            selection_set.add_at_path(&path_vec, entry.selections())?;
        }

        if selection_set.selections.is_empty() {
            return Ok(None);
        }

        let selection_cost = selection_set.cost(1.0);
        let node_cost = FETCH_COST + selection_cost;

        // Group-level @skip/@include: when every selection is gated by the
        // same variable conditions, hoist them out of the operation and
        // gate the fetch itself (ConditionNodes below). Execution can then
        // skip the fetch entirely.
        let group_conditions = selection_set.conditions()?.update_with(handled_conditions);
        if let Conditions::Boolean(false) = group_conditions {
            return Ok(None);
        }

        // 2. Finalize selection: strip conditions, flatten, add __typename
        //    and aliases.
        let (finalized_selection, output_rewrites) = Self::finalize_selection(
            &selection_set,
            &group_conditions,
            is_entity,
            &parent_type,
            subgraph_schema,
            ctx.variable_definitions,
        )?;

        // 3. Materialize entity inputs from incoming edges.
        let (requires_selection, input_rewrites) = if is_entity {
            let (sel, rewrites) =
                self.materialize_entity_inputs(ctx, node_idx, &parent_type, handled_conditions)?;
            (Some(sel), rewrites)
        } else {
            (None, Vec::new())
        };

        // 4. Collect variable definitions narrowed to those actually used.
        let (variable_definitions, variable_usages) = Self::collect_used_variable_definitions(
            ctx.variable_definitions,
            ctx.operation_directives,
            &finalized_selection,
        );

        // 5. Build the subgraph operation.
        let subgraph_schema = ctx.query_graph.schema_by_source(&node.subgraph)?;
        let op_name = ctx.operation_name.as_ref().map(|name| {
            let c = ctx.operation_counter;
            ctx.operation_counter += 1;
            let subgraph = to_valid_graphql_name(&node.subgraph).unwrap_or("".into());
            Name::new(&format!("{name}__{subgraph}__{c}")).unwrap()
        });
        let operation = if is_entity {
            operation_for_entities_fetch(
                subgraph_schema,
                finalized_selection,
                variable_definitions,
                ctx.operation_directives,
                &op_name,
            )?
        } else {
            operation_for_query_fetch(
                subgraph_schema,
                ctx.root_kind,
                finalized_selection,
                variable_definitions,
                ctx.operation_directives,
                &op_name,
            )?
        };
        let operation_document = ctx.operation_compression.compress(operation)?;

        // 6. Build requires (trim to the router-expected format).
        let requires = requires_selection
            .as_ref()
            .map(executable::SelectionSet::try_from)
            .transpose()?
            .map(|ss| trim_requires(&ss))
            .unwrap_or_default();

        // 7. Construct FetchNode.
        let fetch_node = PlanNode::Fetch(Box::new(crate::query_plan::FetchNode {
            subgraph_name: node.subgraph.clone(),
            id: None,
            variable_usages,
            requires,
            operation_document: SerializableDocument::from_parsed(operation_document),
            operation_name: op_name,
            operation_kind: ctx.root_kind.into(),
            input_rewrites: Arc::new(input_rewrites),
            output_rewrites,
            context_rewrites: Default::default(),
        }));

        // 8. Wrap entity/root-hop fetches in FlattenNode.
        let mut plan_node = match &node.kind {
            FetchGroupKind::Entity { merge_at } | FetchGroupKind::RootHop { merge_at, .. } => {
                PlanNode::Flatten(crate::query_plan::FlattenNode {
                    path: merge_at.clone(),
                    node: Box::new(fetch_node),
                })
            }
            FetchGroupKind::Root { .. } => fetch_node,
        };

        // 9. Gate the fetch on its hoisted group conditions.
        if let Conditions::Variables(variables) = &group_conditions {
            for (name, kind) in variables.iter() {
                let (if_clause, else_clause) = match kind {
                    ConditionKind::Skip => (None, Some(Box::new(plan_node))),
                    ConditionKind::Include => (Some(Box::new(plan_node)), None),
                };
                plan_node = PlanNode::Condition(Box::new(crate::query_plan::ConditionNode {
                    condition_variable: name.clone(),
                    if_clause,
                    else_clause,
                }));
            }
        }
        Ok(Some((plan_node, node_cost)))
    }

    /// Strip @skip/@include conditions, flatten unnecessary fragments, add
    /// `__typename` on abstract types, and alias non-merging fields. Entity
    /// fetches keep their top-level inline fragments (which delimit entity
    /// cases 1:1 with requires items), flattening only within each fragment.
    fn finalize_selection(
        selection_set: &SelectionSet,
        group_conditions: &Conditions,
        is_entity: bool,
        parent_type: &CompositeTypeDefinitionPosition,
        subgraph_schema: &ValidFederationSchema,
        variable_definitions: &[Node<VariableDefinition>],
    ) -> Result<(SelectionSet, Vec<Arc<FetchDataRewrite>>), FederationError> {
        let stripped = remove_conditions_from_selection_set(selection_set, group_conditions)?;
        let selection_without_conditions = if is_entity {
            let mut selections = SelectionMap::new();
            for selection in stripped.selections.values() {
                match selection {
                    crate::operation::Selection::InlineFragment(frag_sel) => {
                        let casted = frag_sel.inline_fragment.casted_type();
                        let flattened = frag_sel
                            .selection_set
                            .flatten_unnecessary_fragments(&casted, subgraph_schema)?;
                        selections.insert(crate::operation::Selection::InlineFragment(Arc::new(
                            crate::operation::InlineFragmentSelection::new(
                                frag_sel.inline_fragment.clone(),
                                flattened,
                            ),
                        )));
                    }
                    other => {
                        selections.insert(other.clone());
                    }
                }
            }
            SelectionSet {
                schema: stripped.schema.clone(),
                type_position: stripped.type_position.clone(),
                selections: Arc::new(selections),
            }
        } else {
            stripped.flatten_unnecessary_fragments(parent_type, subgraph_schema)?
        };
        let selection_with_typenames =
            selection_without_conditions.add_typename_field_for_abstract_types(None)?;
        let (finalized_selection, output_rewrites) =
            selection_with_typenames.add_aliases_for_non_merging_fields()?;
        finalized_selection.validate(variable_definitions)?;
        Ok((finalized_selection, output_rewrites))
    }

    /// Collect entity representation inputs from an entity group's incoming
    /// edges: the merged requires `SelectionSet` plus the `FetchDataRewrite`
    /// entries for typename and alias rewrites.
    fn materialize_entity_inputs(
        &self,
        ctx: &PlanBuildContext<'_>,
        node_idx: NodeIndex,
        parent_type: &CompositeTypeDefinitionPosition,
        handled_conditions: &Conditions,
    ) -> Result<(SelectionSet, Vec<Arc<FetchDataRewrite>>), FederationError> {
        let mut per_type: IndexMap<CompositeTypeDefinitionPosition, SelectionSet> =
            IndexMap::default();
        let mut rewrites: Vec<Arc<FetchDataRewrite>> = Vec::new();

        for edge in self.graph.edges_directed(node_idx, Direction::Incoming) {
            for input in &edge.weight().inputs {
                let input_type: CompositeTypeDefinitionPosition = ctx
                    .supergraph_schema
                    .get_type(&input.source_type_name)?
                    .try_into()?;
                let mut input_sel = SelectionSet::for_composite_type(
                    ctx.supergraph_schema.clone(),
                    input_type.clone(),
                );
                input_sel.add_selection_set(&input.conditions)?;
                let wrapped = wrap_input_selections(
                    ctx.supergraph_schema,
                    &input_type,
                    input_sel,
                    &OpGraphPathContext::default(),
                );
                let entry = per_type
                    .entry(wrapped.type_position.clone())
                    .or_insert_with(|| {
                        SelectionSet::empty(
                            ctx.supergraph_schema.clone(),
                            wrapped.type_position.clone(),
                        )
                    });
                entry.add_local_selection_set(&wrapped)?;

                if let Some(info) = &input.rewrite_info {
                    let dest_schema = ctx.query_graph.schema_by_source(&info.dest_subgraph)?;
                    if let Some(r) = compute_input_rewrites_on_key_fetch(
                        &input.source_type_name,
                        &info.dest_type,
                        dest_schema,
                    )? {
                        rewrites.extend(r);
                    }
                }
                for (alias, original_name) in &input.condition_alias_rewrites {
                    rewrites.push(Arc::new(FetchDataRewrite::KeyRenamer(
                        crate::query_plan::FetchDataKeyRenamer {
                            path: vec![FetchDataPathElement::Key(
                                alias.clone(),
                                Default::default(),
                            )],
                            rename_key_to: original_name.clone(),
                        },
                    )));
                }
            }
        }

        let mut merged_selections = SelectionMap::new();
        for selection_set in per_type.values() {
            let cleaned = remove_conditions_from_selection_set(selection_set, handled_conditions)?;
            cleaned.validate(ctx.variable_definitions)?;
            merged_selections.extend_ref(&cleaned.selections);
        }
        let result = SelectionSet {
            schema: ctx.supergraph_schema.clone(),
            type_position: parent_type.clone(),
            selections: Arc::new(merged_selections),
        };
        Ok((result, rewrites))
    }

    /// Filter the operation's variable definitions to those actually
    /// referenced by the finalized selection and operation directives.
    fn collect_used_variable_definitions(
        operation_variable_definitions: &[Node<VariableDefinition>],
        operation_directives: &DirectiveList,
        finalized_selection: &SelectionSet,
    ) -> (Vec<Node<VariableDefinition>>, Vec<Name>) {
        let variable_definitions = {
            let mut collector = VariableCollector::new();
            collector.visit_directive_list(operation_directives);
            collector.visit_selection_set(finalized_selection);
            let used = collector.into_inner();
            operation_variable_definitions
                .iter()
                .filter(|v| used.contains(&v.name))
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut variable_usages: Vec<Name> = variable_definitions
            .iter()
            .map(|v| v.name.clone())
            .collect();
        variable_usages.sort();
        (variable_definitions, variable_usages)
    }
}

/// Trim a `SelectionSet` from `apollo_compiler::executable` down to the
/// router's `requires_selection` format, discarding fragment spreads.
fn trim_requires(selection_set: &executable::SelectionSet) -> Vec<requires_selection::Selection> {
    selection_set
        .selections
        .iter()
        .filter_map(|s| match s {
            executable::Selection::Field(field) => Some(requires_selection::Selection::Field(
                requires_selection::Field {
                    alias: field.alias.clone(),
                    name: field.name.clone(),
                    selections: trim_requires(&field.selection_set),
                },
            )),
            executable::Selection::InlineFragment(inline) => Some(
                requires_selection::Selection::InlineFragment(requires_selection::InlineFragment {
                    type_condition: inline.type_condition.clone(),
                    selections: trim_requires(&inline.selection_set),
                }),
            ),
            executable::Selection::FragmentSpread(_) => None,
        })
        .collect()
}
