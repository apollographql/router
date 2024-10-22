use std::sync::Arc;

use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::VariableDefinition;
use apollo_compiler::Name;
use apollo_compiler::Node;

use super::conditions::ConditionKind;
use super::query_planner::SubgraphOperationCompression;
use super::QueryPathElement;
use crate::error::FederationError;
use crate::operation::DirectiveList;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::OpPathElement;
use crate::query_graph::QueryGraph;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::fetch_dependency_graph::DeferredInfo;
use crate::query_plan::fetch_dependency_graph::FetchDependencyGraphNode;
use crate::query_plan::ConditionNode;
use crate::query_plan::DeferNode;
use crate::query_plan::DeferredDeferBlock;
use crate::query_plan::DeferredDependency;
use crate::query_plan::ParallelNode;
use crate::query_plan::PlanNode;
use crate::query_plan::PrimaryDeferBlock;
use crate::query_plan::QueryPlanCost;
use crate::query_plan::SequenceNode;

/// Constant used during query plan cost computation to account for the base cost of doing a fetch,
/// that is the fact any fetch imply some networking cost, request serialization/deserialization,
/// validation, ...
///
/// The number is a little bit arbitrary,
/// but insofar as we roughly assign a cost of 1 to a single field queried
/// (see `selectionCost` method),
/// this can be though of as saying that resolving a single field is in general
/// a tiny fraction of the actual cost of doing a subgraph fetch.
const FETCH_COST: QueryPlanCost = 1000.0;

/// Constant used during query plan cost computation
/// as a multiplier to the cost of fetches made in sequences.
///
/// This means that if 3 fetches are done in sequence,
/// the cost of 1nd one is multiplied by this number,
/// the 2nd by twice this number, and the 3rd one by thrice this number.
/// The goal is to heavily favor query plans with the least amount of sequences,
/// since this affect overall latency directly.
/// The exact number is a tad  arbitrary however.
const PIPELINING_COST: QueryPlanCost = 100.0;

pub(crate) struct FetchDependencyGraphToQueryPlanProcessor {
    variable_definitions: Arc<Vec<Node<VariableDefinition>>>,
    operation_directives: DirectiveList,
    operation_compression: SubgraphOperationCompression,
    operation_name: Option<Name>,
    assigned_defer_labels: Option<IndexSet<String>>,
    counter: u32,
}

/// Computes the cost of a Plan.
///
/// A plan is essentially some mix of sequences and parallels of fetches. And the plan cost
/// is about minimizing both:
///  1. The expected total latency of executing the plan. Typically, doing 2 fetches in
///     parallel will most likely have much better latency then executing those exact same
///     fetches in sequence, and so the cost of the latter must be greater than that of
///     the former.
///  2. The underlying use of resources. For instance, if we query 2 fields and we have
///     the choice between getting those 2 fields from a single subgraph in 1 fetch, or
///     get each from a different subgraph with 2 fetches in parallel, then we want to
///     favor the former as just doing a fetch in and of itself has a cost in terms of
///     resources consumed.
///
/// Do note that at the moment, this cost is solely based on the "shape" of the plan and has
/// to make some conservative assumption regarding concrete runtime behaviour. In particular,
/// it assumes that:
///  - all fields have the same cost (all resolvers take the same time).
///  - that field cost is relative small compare to actually doing a subgraph fetch. That is,
///    it assumes that the networking and other query processing costs are much higher than
///    the cost of resolving a single field. Or to put it more concretely, it assumes that
///    a fetch of 5 fields is probably not too different from than of 2 fields.
#[derive(Clone, Copy)]
pub(crate) struct FetchDependencyGraphToCostProcessor;

/// Generic interface for "processing" a (reduced) dependency graph of fetch dependency nodes
/// (a `FetchDependencyGraph`).
///
/// The processor methods will be called in a way that "respects" the dependency graph.
/// More precisely, a reduced fetch dependency graph can be expressed
/// as an alternance of parallel branches and sequences of nodes
/// (the roots needing to be either parallel or
/// sequential depending on whether we represent a `query` or a `mutation`),
/// and the processor will be called on nodes in such a way.
pub(crate) trait FetchDependencyGraphProcessor<TProcessed, TDeferred> {
    fn on_node(
        &mut self,
        query_graph: &QueryGraph,
        node: &mut FetchDependencyGraphNode,
        handled_conditions: &Conditions,
    ) -> Result<TProcessed, FederationError>;
    fn on_conditions(&mut self, conditions: &Conditions, value: TProcessed) -> TProcessed;
    fn reduce_parallel(&mut self, values: impl IntoIterator<Item = TProcessed>) -> TProcessed;
    fn reduce_sequence(&mut self, values: impl IntoIterator<Item = TProcessed>) -> TProcessed;
    fn reduce_deferred(
        &mut self,
        defer_info: &DeferredInfo,
        value: TProcessed,
    ) -> Result<TDeferred, FederationError>;
    fn reduce_defer(
        &mut self,
        main: TProcessed,
        sub_selection: &SelectionSet,
        deferred_blocks: Vec<TDeferred>,
    ) -> Result<TProcessed, FederationError>;
}

// So you can use `&mut processor` as an `impl Processor`.
impl<TProcessed, TDeferred, T> FetchDependencyGraphProcessor<TProcessed, TDeferred> for &mut T
where
    T: FetchDependencyGraphProcessor<TProcessed, TDeferred>,
{
    fn on_node(
        &mut self,
        query_graph: &QueryGraph,
        node: &mut FetchDependencyGraphNode,
        handled_conditions: &Conditions,
    ) -> Result<TProcessed, FederationError> {
        (*self).on_node(query_graph, node, handled_conditions)
    }
    fn on_conditions(&mut self, conditions: &Conditions, value: TProcessed) -> TProcessed {
        (*self).on_conditions(conditions, value)
    }
    fn reduce_parallel(&mut self, values: impl IntoIterator<Item = TProcessed>) -> TProcessed {
        (*self).reduce_parallel(values)
    }
    fn reduce_sequence(&mut self, values: impl IntoIterator<Item = TProcessed>) -> TProcessed {
        (*self).reduce_sequence(values)
    }
    fn reduce_deferred(
        &mut self,
        defer_info: &DeferredInfo,
        value: TProcessed,
    ) -> Result<TDeferred, FederationError> {
        (*self).reduce_deferred(defer_info, value)
    }
    fn reduce_defer(
        &mut self,
        main: TProcessed,
        sub_selection: &SelectionSet,
        deferred_blocks: Vec<TDeferred>,
    ) -> Result<TProcessed, FederationError> {
        (*self).reduce_defer(main, sub_selection, deferred_blocks)
    }
}

impl FetchDependencyGraphProcessor<QueryPlanCost, QueryPlanCost>
    for FetchDependencyGraphToCostProcessor
{
    /// The cost of a fetch roughly proportional to how many fields it fetches
    /// (but see `selectionCost` for more details)
    /// plus some constant "premium" to account for the fact than doing each fetch is costly
    /// (and that fetch cost often dwarfted the actual cost of fields resolution).
    fn on_node(
        &mut self,
        _query_graph: &QueryGraph,
        node: &mut FetchDependencyGraphNode,
        _handled_conditions: &Conditions,
    ) -> Result<QueryPlanCost, FederationError> {
        Ok(FETCH_COST + node.cost()?)
    }

    /// We don't take conditions into account in costing for now
    /// as they don't really know anything on the condition
    /// and this shouldn't really play a role in picking a plan over another.
    fn on_conditions(&mut self, _conditions: &Conditions, value: QueryPlanCost) -> QueryPlanCost {
        value
    }

    /// We sum the cost of nodes in parallel.
    /// Note that if we were only concerned about expected latency,
    /// we could instead take the `max` of the values,
    /// but as we also try to minimize general resource usage,
    /// we want 2 parallel fetches with cost 1000 to be more costly
    /// than one with cost 1000 and one with cost 10,
    /// so suming is a simple option.
    fn reduce_parallel(
        &mut self,
        values: impl IntoIterator<Item = QueryPlanCost>,
    ) -> QueryPlanCost {
        parallel_cost(values)
    }

    /// For sequences, we want to heavily favor "shorter" pipelines of fetches
    /// as this directly impact the expected latency of the overall plan.
    ///
    /// To do so, each "stage" of a sequence/pipeline gets an additional multiplier
    /// on the intrinsic cost of that stage.
    fn reduce_sequence(
        &mut self,
        values: impl IntoIterator<Item = QueryPlanCost>,
    ) -> QueryPlanCost {
        sequence_cost(values)
    }

    /// This method exists so we can inject the necessary information for deferred block when
    /// genuinely creating plan nodes. It's irrelevant to cost computation however and we just
    /// return the cost of the block unchanged.
    fn reduce_deferred(
        &mut self,
        _defer_info: &DeferredInfo,
        value: QueryPlanCost,
    ) -> Result<QueryPlanCost, FederationError> {
        Ok(value)
    }

    /// It is unfortunately a bit difficult to properly compute costs for defers because in theory
    /// some of the deferred blocks (the costs in `deferredValues`) can be started _before_ the full
    /// `nonDeferred` part finishes (more precisely, the "structure" of query plans express the fact
    /// that there is a non-deferred part and other deferred parts, but the complete dependency of
    /// when a deferred part can be start is expressed through the `FetchNode.id` field, and as
    /// this cost function is currently mainly based on the "structure" of query plans, we don't
    /// have easy access to this info).
    ///
    /// Anyway, the approximation we make here is that all the deferred starts strictly after the
    /// non-deferred one, and that all the deferred parts can be done in parallel.
    fn reduce_defer(
        &mut self,
        main: QueryPlanCost,
        _sub_selection: &SelectionSet,
        deferred_blocks: Vec<QueryPlanCost>,
    ) -> Result<QueryPlanCost, FederationError> {
        Ok(sequence_cost([main, parallel_cost(deferred_blocks)]))
    }
}

fn parallel_cost(values: impl IntoIterator<Item = QueryPlanCost>) -> QueryPlanCost {
    values.into_iter().sum()
}

fn sequence_cost(values: impl IntoIterator<Item = QueryPlanCost>) -> QueryPlanCost {
    values
        .into_iter()
        .enumerate()
        .map(|(i, stage)| stage * (1.0f64).max(i as QueryPlanCost * PIPELINING_COST))
        .sum()
}

impl FetchDependencyGraphToQueryPlanProcessor {
    pub(crate) fn new(
        variable_definitions: Arc<Vec<Node<VariableDefinition>>>,
        operation_directives: DirectiveList,
        operation_compression: SubgraphOperationCompression,
        operation_name: Option<Name>,
        assigned_defer_labels: Option<IndexSet<String>>,
    ) -> Self {
        Self {
            variable_definitions,
            operation_directives,
            operation_compression,
            operation_name,
            assigned_defer_labels,
            counter: 0,
        }
    }
}

impl FetchDependencyGraphProcessor<Option<PlanNode>, DeferredDeferBlock>
    for FetchDependencyGraphToQueryPlanProcessor
{
    fn on_node(
        &mut self,
        query_graph: &QueryGraph,
        node: &mut FetchDependencyGraphNode,
        handled_conditions: &Conditions,
    ) -> Result<Option<PlanNode>, FederationError> {
        let op_name = self.operation_name.as_ref().map(|name| {
            let counter = self.counter;
            self.counter += 1;
            let subgraph = to_valid_graphql_name(&node.subgraph_name).unwrap_or("".into());
            // `name` was already a valid name so this concatenation should be too
            Name::new(&format!("{name}__{subgraph}__{counter}")).unwrap()
        });
        node.to_plan_node(
            query_graph,
            handled_conditions,
            &self.variable_definitions,
            &self.operation_directives,
            &mut self.operation_compression,
            op_name,
        )
    }

    fn on_conditions(
        &mut self,
        conditions: &Conditions,
        value: Option<PlanNode>,
    ) -> Option<PlanNode> {
        let mut value = value?;
        match conditions {
            Conditions::Boolean(condition) => {
                // Note that currently `ConditionNode` only works for variables
                // (`ConditionNode.condition` is expected to be a variable name and nothing else).
                // We could change that, but really, why have a trivial `ConditionNode`
                // when we can optimise things righ away.
                condition.then_some(value)
            }
            Conditions::Variables(variables) => {
                for (name, kind) in variables.iter() {
                    let (if_clause, else_clause) = match kind {
                        ConditionKind::Skip => (None, Some(Box::new(value))),
                        ConditionKind::Include => (Some(Box::new(value)), None),
                    };
                    value = PlanNode::from(ConditionNode {
                        condition_variable: name.clone(),
                        if_clause,
                        else_clause,
                    });
                }
                Some(value)
            }
        }
    }

    fn reduce_parallel(
        &mut self,
        values: impl IntoIterator<Item = Option<PlanNode>>,
    ) -> Option<PlanNode> {
        flat_wrap_nodes(NodeKind::Parallel, values)
    }

    fn reduce_sequence(
        &mut self,
        values: impl IntoIterator<Item = Option<PlanNode>>,
    ) -> Option<PlanNode> {
        flat_wrap_nodes(NodeKind::Sequence, values)
    }

    fn reduce_deferred(
        &mut self,
        defer_info: &DeferredInfo,
        node: Option<PlanNode>,
    ) -> Result<DeferredDeferBlock, FederationError> {
        /// Produce a query path with only the relevant elements: fields and type conditions.
        fn op_path_to_query_path(
            path: &[Arc<OpPathElement>],
        ) -> Result<Vec<QueryPathElement>, FederationError> {
            path.iter()
                .map(
                    |element| -> Result<Option<QueryPathElement>, FederationError> {
                        match &**element {
                            OpPathElement::Field(field) => {
                                Ok(Some(QueryPathElement::Field(field.try_into()?)))
                            }
                            OpPathElement::InlineFragment(inline) => {
                                match &inline.type_condition_position {
                                    Some(_) => Ok(Some(QueryPathElement::InlineFragment(
                                        inline.try_into()?,
                                    ))),
                                    None => Ok(None),
                                }
                            }
                        }
                    },
                )
                .filter_map(|result| result.transpose())
                .collect::<Result<Vec<_>, _>>()
        }

        Ok(DeferredDeferBlock {
            depends: defer_info
                .dependencies
                .iter()
                .cloned()
                .map(|id| DeferredDependency { id })
                .collect(),
            label: if self
                .assigned_defer_labels
                .as_ref()
                .is_some_and(|set| set.contains(&defer_info.label))
            {
                None
            } else {
                Some(defer_info.label.clone())
            },
            query_path: op_path_to_query_path(&defer_info.path.full_path)?,
            // Note that if the deferred block has nested @defer,
            // then the `value` is going to be a `DeferNode`
            // and we'll use it's own `subselection`, so we don't need it here.
            sub_selection: if defer_info.deferred.is_empty() {
                defer_info
                    .sub_selection
                    .without_empty_branches()?
                    .map(|filtered| filtered.as_ref().try_into())
                    .transpose()?
            } else {
                None
            },
            node: node.map(Box::new),
        })
    }

    fn reduce_defer(
        &mut self,
        main: Option<PlanNode>,
        sub_selection: &SelectionSet,
        deferred: Vec<DeferredDeferBlock>,
    ) -> Result<Option<PlanNode>, FederationError> {
        Ok(Some(PlanNode::Defer(DeferNode {
            primary: PrimaryDeferBlock {
                sub_selection: sub_selection
                    .without_empty_branches()?
                    .map(|filtered| filtered.as_ref().try_into())
                    .transpose()?,
                node: main.map(Box::new),
            },
            deferred,
        })))
    }
}

/// Returns `None` if `subgraph_name` contains no character in [-_A-Za-z0-9]
///
/// Add `.unwrap_or("".into())` to get an empty string in that case.
/// The empty string is not a valid name by itself but work if concatenating with something else.
pub(crate) fn to_valid_graphql_name(subgraph_name: &str) -> Option<String> {
    // We have almost no limitations on subgraph names, so we cannot use them inside query names
    // without some cleaning up. GraphQL names can only be: [_A-Za-z][_0-9A-Za-z]*.
    // To do so, we:
    //  1. replace '-' by '_' because the former is not allowed but it's probably pretty
    //   common and using the later should be fairly readable.
    //  2. remove any character in what remains that is not allowed.
    //  3. Unsure the first character is not a number, and if it is, add a leading `_`.
    // Note that this could theoretically lead to substantial changes to the name but should
    // work well in practice (and if it's a huge problem for someone, we can change it).
    let mut chars = subgraph_name.chars().filter_map(|c| {
        if let '-' | '_' = c {
            Some('_')
        } else {
            c.is_ascii_alphanumeric().then_some(c)
        }
    });
    let first = chars.next()?;
    let mut sanitized = String::with_capacity(subgraph_name.len() + 1);
    if first.is_ascii_digit() {
        sanitized.push('_')
    }
    sanitized.push(first);
    sanitized.extend(chars);
    Some(sanitized)
}

#[derive(Clone, Copy)]
enum NodeKind {
    Parallel,
    Sequence,
}

/// Wraps the given nodes in a ParallelNode or SequenceNode, unless there's only
/// one node, in which case it is returned directly. Any nodes of the same kind
/// in the given list have their sub-nodes flattened into the list: ie,
/// flatWrapNodes('Sequence', [a, flatWrapNodes('Sequence', b, c), d]) returns a SequenceNode
/// with four children.
fn flat_wrap_nodes(
    kind: NodeKind,
    nodes: impl IntoIterator<Item = Option<PlanNode>>,
) -> Option<PlanNode> {
    let mut iter = nodes.into_iter().flatten();
    let first = iter.next()?;
    let Some(second) = iter.next() else {
        return Some(first.clone());
    };
    let mut nodes = Vec::new();
    for node in [first, second].into_iter().chain(iter) {
        match (kind, node) {
            (NodeKind::Parallel, PlanNode::Parallel(inner)) => {
                nodes.extend(inner.nodes.iter().cloned())
            }
            (NodeKind::Sequence, PlanNode::Sequence(inner)) => {
                nodes.extend(inner.nodes.iter().cloned())
            }
            (_, node) => nodes.push(node),
        }
    }
    Some(match kind {
        NodeKind::Parallel => PlanNode::Parallel(ParallelNode { nodes }),
        NodeKind::Sequence => PlanNode::Sequence(SequenceNode { nodes }),
    })
}
