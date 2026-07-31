//! Source-aware Phase 1: the **federation entry point** for raw-graph,
//! coordinate-stamped query planning.
//!
//! This consolidates the flow the spike proved piecewise (see
//! [`connector_stamp`](super::connector_stamp)) into one reusable seam so the
//! router pipeline can build a source-aware planner once and plan many
//! operations against it:
//!
//! 1. Build a query graph that treats `connectors` as **one ordinary subgraph**
//!    (no expansion into synthetic per-connector subgraphs), and a planner over
//!    it via [`QueryPlanner::from_query_graph`] — the spike seam that accepts a
//!    pre-built graph.
//! 2. Extract the **ground-truth connector set** from the raw subgraphs
//!    ([`Connector::from_schema`]), so coordinates are determined authoritatively
//!    from `@connect` metadata rather than recovered heuristically.
//! 3. Plan an operation and [`stamp_connector_coordinates`] onto the result, so
//!    each connector fetch carries its coordinate on the B-1 identity channel.
//!
//! `from_query_graph` and `extract_subgraphs_from_supergraph` remain
//! `pub(crate)`: this purpose-named `pub` entry point is the only widened
//! surface, per the pipeline handoff's "prefer a small entry point over opening
//! internals broadly" guidance.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;

use crate::ApiSchemaOptions;
use crate::Supergraph;
use crate::connectors::Connector;
use crate::error::FederationError;
use crate::query_graph::build_federated_query_graph;
use crate::query_graph::connect_graph::restrict_connector_reachability;
use crate::query_plan::QueryPlan;
use crate::query_plan::connector_stamp::stamp_connector_coordinates;
use crate::query_plan::query_planner::QueryPlanner;
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::supergraph::extract_subgraphs_from_supergraph;

/// A raw-graph query planner for a connector supergraph, paired with the
/// ground-truth connector set for coordinate stamping.
///
/// Built once from a supergraph SDL; [`plan`](Self::plan) may then be called for
/// many operations. Every plan it produces carries connector identity
/// (`FetchNode.connector`) on each connector fetch.
pub struct SourceAwareQueryPlanner {
    planner: QueryPlanner,
    connectors: Vec<Connector>,
}

impl SourceAwareQueryPlanner {
    /// Build a source-aware planner from a connector supergraph SDL.
    ///
    /// The planner plans over the **raw** (non-expanded) graph; the connector
    /// set is extracted from the raw subgraphs for authoritative stamping.
    pub fn new(supergraph_sdl: &str, config: QueryPlannerConfig) -> Result<Self, FederationError> {
        let supergraph = Supergraph::new_with_router_specs(supergraph_sdl)?;
        let api_schema = supergraph.to_api_schema(ApiSchemaOptions {
            include_defer: config.incremental_delivery.enable_defer,
            ..Default::default()
        })?;

        // Ground-truth connectors, built directly from the raw subgraphs — the
        // authoritative source for coordinate stamping and for the reachability
        // restriction below.
        let fed = FederationSchema::new(Schema::parse(supergraph_sdl, "supergraph.graphql")?)?;
        let subgraphs = extract_subgraphs_from_supergraph(&fed, Some(false))?;
        let connectors: Vec<Connector> = subgraphs
            .subgraphs
            .values()
            .flat_map(|sg| Connector::from_schema(sg.schema.schema(), &sg.name).unwrap_or_default())
            .collect();

        // Raw graph: connectors are one ordinary subgraph, not expanded.
        let mut query_graph = build_federated_query_graph(
            supergraph.schema.clone(),
            api_schema.clone(),
            Some(false),
            Some(true),
        )?;
        // Restrictive-provides pass: prune each connector's landing-type copy to
        // the fields its `selection` returns, so the planner emits `_entities`
        // fetches (via entity-resolver connectors) instead of over-merging
        // fields into a fetch the entry connector cannot serve.
        restrict_connector_reachability(&mut query_graph, &connectors)?;
        let planner = QueryPlanner::from_query_graph(
            config,
            query_graph,
            supergraph.schema.clone(),
            api_schema,
        )?;

        Ok(Self {
            planner,
            connectors,
        })
    }

    /// The API schema, for parsing/validating operations to plan.
    pub fn api_schema(&self) -> &ValidFederationSchema {
        self.planner.api_schema()
    }

    /// Decompose into the underlying planner and the connector set. Use this
    /// when the caller needs to drive `build_query_plan` with its own options
    /// (cancellation, limits) and then stamp the result with
    /// [`stamp_connector_coordinates`] itself — e.g. the router's planner
    /// service, which cannot call the `pub(crate)` `from_query_graph` directly.
    pub fn into_parts(self) -> (QueryPlanner, Vec<Connector>) {
        (self.planner, self.connectors)
    }

    /// The ground-truth connector set used for stamping.
    pub fn connectors(&self) -> &[Connector] {
        &self.connectors
    }

    /// Plan `operation` over the raw graph and stamp each connector fetch with
    /// its coordinate (B-2a). The returned plan carries connector identity on
    /// its fetch nodes; non-connector fetches are left unstamped.
    pub fn plan(
        &self,
        operation: &Valid<ExecutableDocument>,
    ) -> Result<QueryPlan, FederationError> {
        let mut plan = self
            .planner
            .build_query_plan(operation, None, Default::default())?;
        stamp_connector_coordinates(
            &mut plan,
            &self.connectors,
            self.planner.supergraph_schema().schema(),
        );
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;

    use super::*;
    use crate::query_plan::PlanNode;
    use crate::query_plan::TopLevelPlanNode;

    /// Collect `(subgraph, connector_coordinate_option)` for every fetch in a
    /// plan, in traversal order.
    fn fetch_stamps(plan: &QueryPlan) -> Vec<(String, Option<String>)> {
        fn walk(node: &PlanNode, out: &mut Vec<(String, Option<String>)>) {
            match node {
                PlanNode::Fetch(f) => out.push((f.subgraph_name.to_string(), f.connector.clone())),
                PlanNode::Sequence(s) => s.nodes.iter().for_each(|n| walk(n, out)),
                PlanNode::Parallel(p) => p.nodes.iter().for_each(|n| walk(n, out)),
                PlanNode::Flatten(fl) => walk(&fl.node, out),
                PlanNode::Defer(_) | PlanNode::Condition(_) => {}
            }
        }
        let mut out = Vec::new();
        match &plan.node {
            Some(TopLevelPlanNode::Fetch(f)) => {
                out.push((f.subgraph_name.to_string(), f.connector.clone()))
            }
            Some(TopLevelPlanNode::Sequence(s)) => s.nodes.iter().for_each(|n| walk(n, &mut out)),
            Some(TopLevelPlanNode::Parallel(p)) => p.nodes.iter().for_each(|n| walk(n, &mut out)),
            Some(TopLevelPlanNode::Flatten(fl)) => walk(&fl.node, &mut out),
            _ => {}
        }
        out
    }

    /// The restrictive-provides pass (`restrict_connector_reachability`) makes
    /// the planner emit a proper `_entities` fetch for a field the entry
    /// connector doesn't provide: `username` is absent from the `Query.users`
    /// connector's `selection`, so `{ users { username } }` must plan as
    /// `Sequence[ users-fetch, Flatten(_entities username) ]` — with the entity
    /// fetch stamped to the `Query.user` entity-resolver connector — instead of
    /// over-merging `username` into the root fetch (which returned `null` at
    /// execution). Queries the entry connector *can* serve alone must keep
    /// planning as a single fetch.
    #[test]
    fn plans_entity_resolver_fetch_for_unprovided_field() {
        let sdl = include_str!("../connectors/expand/tests/schemas/expand/steelthread.graphql");
        let planner = SourceAwareQueryPlanner::new(sdl, QueryPlannerConfig::default()).unwrap();

        // The over-merge shape: username must get its own entity fetch.
        {
            let doc = ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                "{ users { username } }",
                "q.graphql",
            )
            .unwrap();
            let plan = planner.plan(&doc).unwrap();
            let stamps = fetch_stamps(&plan);
            assert_eq!(stamps.len(), 2, "root fetch + entity fetch, got {plan}");
            assert!(
                stamps[0].0 == "connectors"
                    && stamps[0]
                        .1
                        .as_deref()
                        .is_some_and(|c| c.contains("Query.users")),
                "root fetch stamped Query.users, got {stamps:?}"
            );
            assert!(
                stamps[1].0 == "connectors"
                    && stamps[1]
                        .1
                        .as_deref()
                        .is_some_and(|c| c.contains("Query.user[")),
                "entity fetch stamped with the Query.user entity resolver, got {stamps:?}"
            );
            // The username selection really moved into the follow-up entity
            // fetch (rendered `{ ... on User { __typename id } } => { ... on
            // User { username } }` under a Flatten), and out of the root fetch.
            let rendered = plan.to_string();
            assert!(
                rendered.contains("Flatten(path: \"users.@\")"),
                "expected a flattened entity fetch in {plan}"
            );
            let root_fetch_end = rendered.find("Flatten").unwrap();
            assert!(
                !rendered[..root_fetch_end].contains("username"),
                "username must not remain in the root fetch: {plan}"
            );
            assert!(
                rendered[root_fetch_end..].contains("username"),
                "username must be selected by the entity fetch: {plan}"
            );
        }

        // Fields the entry connector provides still plan as one fetch — the
        // restriction must not manufacture unnecessary entity fetches.
        {
            let doc = ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                "{ users { id name } }",
                "q.graphql",
            )
            .unwrap();
            let plan = planner.plan(&doc).unwrap();
            let stamps = fetch_stamps(&plan);
            assert_eq!(
                stamps.len(),
                1,
                "provided-only selection stays a single fetch, got {plan}"
            );
        }
    }

    /// The entry point produces a stamped raw-graph plan end to end: build once
    /// from SDL, plan operations, and every connector fetch carries its
    /// coordinate. This is the same evidence as
    /// `connector_stamp::stamps_connector_coordinates_over_raw_graph_plans`, but
    /// exercising the consolidated `SourceAwareQueryPlanner` seam the router
    /// pipeline will call.
    #[test]
    fn plans_and_stamps_end_to_end() {
        let sdl = include_str!("../connectors/expand/tests/schemas/expand/steelthread.graphql");
        let planner = SourceAwareQueryPlanner::new(sdl, QueryPlannerConfig::default()).unwrap();
        assert!(
            !planner.connectors().is_empty(),
            "steelthread has connectors"
        );

        // Root-field query → the single connectors fetch stamped with Query.users.
        {
            let doc = ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                "{ users { id name } }",
                "q.graphql",
            )
            .unwrap();
            let plan = planner.plan(&doc).unwrap();
            let stamps = fetch_stamps(&plan);
            assert!(
                stamps.iter().any(|(sg, c)| sg == "connectors"
                    && c.as_deref().is_some_and(|c| c.contains("Query.users"))),
                "root-field connectors fetch stamped with Query.users, got {stamps:?}"
            );
        }

        // Entity + @requires query → connectors root fetch Query.user, connectors
        // entity fetch User.d, graphql fetch (c) left unstamped.
        {
            let doc = ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                "{ user(id: \"1\") { name d } }",
                "q.graphql",
            )
            .unwrap();
            let plan = planner.plan(&doc).unwrap();
            let stamps = fetch_stamps(&plan);
            for (sg, coord) in &stamps {
                if sg == "graphql" {
                    assert_eq!(
                        coord, &None,
                        "graphql fetch must not be stamped: {stamps:?}"
                    );
                }
            }
            assert!(
                stamps.iter().any(|(sg, c)| sg == "connectors"
                    && c.as_deref().is_some_and(|c| c.contains("User.d"))),
                "connector entity fetch stamped with User.d, got {stamps:?}"
            );
            assert!(
                stamps.iter().any(|(sg, c)| sg == "connectors"
                    && c.as_deref().is_some_and(|c| c.contains("Query.user["))),
                "connectors root fetch stamped with Query.user, got {stamps:?}"
            );
        }
    }
}
