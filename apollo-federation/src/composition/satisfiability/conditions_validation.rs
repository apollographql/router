use std::sync::Arc;

use petgraph::graph::EdgeIndex;

use crate::bail;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_graph::QueryGraph;
use crate::query_graph::condition_resolver::CachingConditionResolver;
use crate::query_graph::condition_resolver::ConditionResolution;
use crate::query_graph::condition_resolver::ConditionResolverCache;
use crate::query_graph::graph_path::ExcludedConditions;
use crate::query_graph::graph_path::ExcludedDestinations;
use crate::query_graph::graph_path::GraphPathWeightCounter;
use crate::query_graph::graph_path::operation::OpGraphPath;
use crate::query_graph::graph_path::operation::OpGraphPathContext;
use crate::query_graph::graph_path::operation::OpenBranch;
use crate::query_graph::graph_path::operation::OpenBranchAndSelections;
use crate::query_graph::graph_path::operation::SimultaneousPaths;
use crate::query_graph::graph_path::operation::SimultaneousPathsWithLazyIndirectPaths;

/// A simple condition resolver that only validates that the condition can be satisfied, but
/// without trying compare/evaluate the potential various ways to validate said conditions.
/// Concretely, the `ConditionResolution` values returned by the create resolver will never contain
/// a `pathTree` (or an `unsatisfiedConditionReason` for that matter) and the cost will always
/// default to 1 if the conditions are satisfied.
// PORT_NOTE: This ports the `simpleValidationConditionResolver` function from JS. In JS
//            version, the function creates a closure. In Rust, `ConditionValidationTraversal`
//            implements `CachingConditionResolver` trait, similarly to how it was ported with
//            `QueryPlanningTraversal`. Also, the JS version has a `withCaching` argument to
//            control whether to use caching. Non-cached case is only used in tests. So, Rust
//            version is simplified to always use caching.
// Note: Analogous to `resolve_condition_plan` method of the QueryPlanningTraversal struct.
pub(super) fn resolve_condition_plan(
    query_graph: Arc<QueryGraph>,
    edge: EdgeIndex,
    context: &OpGraphPathContext,
    excluded_destinations: &ExcludedDestinations,
    excluded_conditions: &ExcludedConditions,
    extra_conditions: Option<&SelectionSet>,
    graph_path_weight_counter: Arc<GraphPathWeightCounter>,
) -> Result<ConditionResolution, FederationError> {
    let edge_weight = query_graph.edge_weight(edge)?;
    let conditions = match (extra_conditions, &edge_weight.conditions) {
        (Some(extra_conditions), None) => extra_conditions,
        (None, Some(edge_conditions)) => edge_conditions,
        (Some(_), Some(_)) => bail!("Both extra_conditions and edge conditions are set"),
        (None, None) => bail!("Both extra_conditions and edge conditions are None"),
    };
    let excluded_conditions = excluded_conditions.add_item(conditions);
    let head = query_graph.edge_endpoints(edge)?.0;
    let initial_path =
        OpGraphPath::new(query_graph.clone(), head, graph_path_weight_counter.clone())?;
    let initial_option = SimultaneousPathsWithLazyIndirectPaths::new(
        SimultaneousPaths(vec![Arc::new(initial_path)]),
        context.clone(),
        excluded_destinations.clone(),
        excluded_conditions,
    );
    let mut traversal = ConditionValidationTraversal::new(
        query_graph.clone(),
        initial_option,
        conditions.iter().cloned(),
        graph_path_weight_counter,
    );
    traversal.find_resolution()
}

struct ConditionValidationTraversal {
    /// The federated query graph for the supergraph schema.
    query_graph: Arc<QueryGraph>,
    /// The cache for condition resolution.
    condition_resolver_cache: ConditionResolverCache,
    /// The stack of open branches left to plan, along with state indicating the next selection to
    /// plan for them.
    // PORT_NOTE: This implementation closely follows the way `QueryPlanningTraversal` was ported.
    open_branches: Vec<OpenBranchAndSelections>,
    /// Counter to track/limit the number of in-memory paths (weighted by path size).
    graph_path_weight_counter: Arc<GraphPathWeightCounter>,
}

impl ConditionValidationTraversal {
    fn new(
        query_graph: Arc<QueryGraph>,
        initial_option: SimultaneousPathsWithLazyIndirectPaths,
        selections: impl IntoIterator<Item = Selection>,
        graph_path_weight_counter: Arc<GraphPathWeightCounter>,
    ) -> Self {
        Self {
            query_graph,
            condition_resolver_cache: ConditionResolverCache::new(),
            open_branches: vec![OpenBranchAndSelections {
                selections: selections.into_iter().collect(),
                open_branch: OpenBranch(vec![initial_option]),
            }],
            graph_path_weight_counter,
        }
    }

    // Analogous to `find_best_plan_inner` of QueryPlanningTraversal.
    fn find_resolution(&mut self) -> Result<ConditionResolution, FederationError> {
        while let Some(mut current_branch) = self.open_branches.pop() {
            let Some(current_selection) = current_branch.selections.pop() else {
                bail!("Sub-stack unexpectedly empty during validation traversal",);
            };
            let (terminate_planning, new_branch) =
                self.handle_open_branch(&current_selection, &mut current_branch.open_branch.0)?;
            if terminate_planning {
                return Ok(ConditionResolution::unsatisfied_conditions());
            }
            if !current_branch.selections.is_empty() {
                self.open_branches.push(current_branch);
            }
            if let Some(new_branch) = new_branch {
                self.open_branches.push(new_branch);
            }
        }
        // If we exhaust the stack, it means we've been able to find "some" path for every possible
        // selection in the condition, so the condition is validated. Note that we use a cost of 1
        // for all conditions as we don't care about efficiency.
        Ok(ConditionResolution::Satisfied {
            cost: 1.0f64,
            path_tree: None,
            context_map: None,
        })
    }

    // Analogous to `handle_open_branch` of QueryPlanningTraversal.
    fn handle_open_branch(
        &mut self,
        selection: &Selection,
        options: &mut [SimultaneousPathsWithLazyIndirectPaths],
    ) -> Result<(bool, Option<OpenBranchAndSelections>), FederationError> {
        let mut new_options = Vec::new();
        for paths in options.iter_mut() {
            let options = paths.advance_with_operation_element(
                self.query_graph.supergraph_schema()?.clone(),
                &selection.element(),
                self,
                // In this particular case, we're traversing the selections of a FieldSet. By
                // providing _no_ overrides here, it'll ensure that we don't incorrectly validate
                // any cases where overridden fields are in a FieldSet, it's just disallowed
                // completely.
                &Default::default(),
                &never_cancel,
                &Default::default(),
            )?;
            let Some(options) = options else {
                continue;
            };
            new_options.extend(options);
        }
        if new_options.is_empty() {
            // If we got no options, it means that particular selection of the conditions cannot be
            // satisfied, so the overall condition cannot.
            return Ok((true, None));
        }

        if let Some(selection_set) = selection.selection_set() {
            // If the selection has a selection set, we need to continue traversing it.
            let new_branch = OpenBranchAndSelections {
                open_branch: OpenBranch(new_options),
                selections: selection_set.iter().cloned().collect(),
            };
            Ok((false, Some(new_branch)))
        } else {
            Ok((false, None))
        }
    }
}

// `advance_with_operation_element` method is cancelable, but composition doesn't need to be
// cancelable at the moment. So, this `never_cancel` function is passed to it for now.
pub(crate) fn never_cancel() -> Result<(), SingleFederationError> {
    Ok(())
}

impl CachingConditionResolver for ConditionValidationTraversal {
    fn query_graph(&self) -> &QueryGraph {
        &self.query_graph
    }

    fn resolver_cache(&mut self) -> &mut ConditionResolverCache {
        &mut self.condition_resolver_cache
    }

    fn resolve_without_cache(
        &self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
    ) -> Result<ConditionResolution, FederationError> {
        resolve_condition_plan(
            self.query_graph.clone(),
            edge,
            context,
            excluded_destinations,
            excluded_conditions,
            extra_conditions,
            self.graph_path_weight_counter.clone(),
        )
    }
}

#[cfg(test)]
mod simple_condition_resolver_tests {
    use super::*;
    use crate::Supergraph;
    use crate::query_graph::build_federated_query_graph;

    const TEST_SUPERGRAPH: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
{
  query: Query
}

directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

input join__ContextArgument {
  name: String!
  type: String!
  context: String!
  selection: join__FieldValue!
}

scalar join__DirectiveArguments

scalar join__FieldSet

scalar join__FieldValue

enum join__Graph {
  A @join__graph(name: "A", url: "http://A")
  B @join__graph(name: "B", url: "http://B")
  C @join__graph(name: "C", url: "http://C")
}

scalar link__Import

enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY

  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
  @join__type(graph: C)
{
  start: T! @join__field(graph: A)
}

type T
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: C, key: "id")
{
  id: ID!
  onlyInA: Int! @join__field(graph: A)
  onlyInB: Int! @join__field(graph: B) @join__field(graph: C, external: true)
  onlyInC: Int! @join__field(graph: C, requires: "onlyInB")
}
    "#;

    #[test]
    fn test_simple_condition_resolver_basic() {
        let supergraph = Supergraph::new_with_router_specs(TEST_SUPERGRAPH).unwrap();
        let query_graph = build_federated_query_graph(
            supergraph.schema.clone(),
            supergraph
                .to_api_schema(Default::default())
                .unwrap()
                .clone(),
            Some(true),
            Some(true),
        )
        .unwrap();
        let query_graph = Arc::new(query_graph);

        for edge in query_graph.graph().edge_indices() {
            let edge_weight = query_graph.edge_weight(edge).unwrap();
            if edge_weight.conditions.is_none() {
                continue; // Skip edges without conditions.
            }
            let result = resolve_condition_plan(
                query_graph.clone(),
                edge,
                &Default::default(),
                &Default::default(),
                &Default::default(),
                None,
                Default::default(),
            )
            .unwrap();
            // All edges are expected to be satisfiable.
            assert!(matches!(result, ConditionResolution::Satisfied { .. }));
        }
    }
}
