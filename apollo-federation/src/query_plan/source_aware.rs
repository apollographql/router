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
//!    it via `QueryPlanner::from_query_graph`, the spike seam that accepts a
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
        // NOT WIRED IN: `connect_graph::apply_connector_parent_conditions` would
        // graft each implicit connector's `$this` reads onto its field edge here,
        // which does make the planner fetch them. It is left out because the
        // resulting plan is not justifiable against the raw supergraph — see that
        // function's docs and `plans_this_variable_reads_as_fetch_inputs`.
        //
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

    /// **Nested positions.** One connector reaching one type at two *sibling*
    /// positions with different sub-selections is the case a top-level-only read
    /// of the output shape cannot represent. `Query.users` selects
    /// `id manager { id name } reports { id }`, so `Person` arrives under
    /// `manager` with `{id, name}` and under `reports` with `{id}` only.
    ///
    /// `{ users { manager { name } } }` must therefore stay a single fetch (the
    /// connector does return `name` there), while
    /// `{ users { reports { name } } }` must gain an `_entities` fetch to the
    /// `Query.person` resolver (it does not return `name` there). Before the
    /// recursive restriction, both planned as a single fetch and the second
    /// returned a silent `null` — with nothing prunable among `User`'s own
    /// fields, the pass did not fire at all.
    #[test]
    fn plans_entity_fetch_per_nested_position() {
        let sdl =
            include_str!("../connectors/expand/tests/schemas/expand/sibling_positions.graphql");
        let planner = SourceAwareQueryPlanner::new(sdl, QueryPlannerConfig::default()).unwrap();
        let plan_for = |query: &str| {
            let doc = ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                query,
                "q.graphql",
            )
            .unwrap();
            planner.plan(&doc).unwrap()
        };

        // `manager` provides `name` — one fetch, no entity jump.
        let plan = plan_for("{ users { manager { name } } }");
        assert_eq!(
            fetch_stamps(&plan).len(),
            1,
            "manager provides name, so this stays a single fetch, got {plan}"
        );

        // `reports` does not provide `name` — it must be resolved by re-entry.
        let plan = plan_for("{ users { reports { name } } }");
        let stamps = fetch_stamps(&plan);
        assert_eq!(
            stamps.len(),
            2,
            "reports does not provide name, so it needs a root fetch + entity fetch, got {plan}"
        );
        assert!(
            stamps[0]
                .1
                .as_deref()
                .is_some_and(|c| c.contains("Query.users")),
            "root fetch stamped Query.users, got {stamps:?}"
        );
        assert!(
            stamps[1]
                .1
                .as_deref()
                .is_some_and(|c| c.contains("Query.person")),
            "entity fetch stamped with the Query.person resolver, got {stamps:?}"
        );
        let rendered = plan.to_string();
        let flatten = rendered
            .find("Flatten(path: \"users.@.reports.@\")")
            .unwrap_or_else(|| {
                panic!("expected an entity fetch flattened at the reports position in {plan}")
            });
        // `__typename` contains "name", so strip it before asking about the field.
        assert!(
            !rendered[..flatten]
                .replace("__typename", "")
                .contains("name"),
            "name must not remain in the root fetch: {plan}"
        );
        assert!(
            rendered[flatten..].contains("name"),
            "name must be selected by the entity fetch: {plan}"
        );

        // `title` is provided at *neither* position, so it needs the re-entry
        // from `manager` too — the case that distinguishes a genuinely
        // per-position restriction from one that only fixed `reports`.
        let plan = plan_for("{ users { manager { title } } }");
        assert_eq!(
            fetch_stamps(&plan).len(),
            2,
            "title is provided at neither position, so manager needs a re-entry too, got {plan}"
        );
    }

    /// **A recursive output shape** — `User.friends: [User]`, the shape
    /// connectors validation rejects with `CIRCULAR_REFERENCE` and the one
    /// customers keep asking for. `Query.users` selects `id name friends { id }`,
    /// so `User` is reached at the root with `{id, name, friends}` and under
    /// `friends` with `{id}`: the *same type* at two depths of one path, with
    /// different field sets.
    ///
    /// The recursion terminates because it walks the finite output *shape*, not
    /// the cyclic type graph — a selection can only nest finitely — so a
    /// self-referential type needs no special handling beyond the memo key
    /// distinguishing `User{id, name, friends}` from `User{id}`.
    ///
    /// This test says nothing about whether such a schema *composes*; it is here
    /// to record that the planning half is not what blocks it.
    #[test]
    fn plans_recursive_output_shape() {
        let sdl =
            include_str!("../connectors/expand/tests/schemas/expand/recursive_output.graphql");
        let planner = SourceAwareQueryPlanner::new(sdl, QueryPlannerConfig::default()).unwrap();
        let plan_for = |query: &str| {
            let doc = ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                query,
                "q.graphql",
            )
            .unwrap();
            planner.plan(&doc).unwrap()
        };

        // Provided at the root: one fetch.
        let plan = plan_for("{ users { id name } }");
        assert_eq!(
            fetch_stamps(&plan).len(),
            1,
            "root-provided fields stay a single fetch, got {plan}"
        );

        // `friends` returns only `id`, so `name` one level down needs the
        // re-entry even though `name` *is* provided at the root. This is the
        // recursive case: the same type restricted differently by depth.
        let plan = plan_for("{ users { friends { name } } }");
        let stamps = fetch_stamps(&plan);
        assert_eq!(
            stamps.len(),
            2,
            "name under friends is not provided there, so it needs a re-entry, got {plan}"
        );
        assert!(
            stamps[1]
                .1
                .as_deref()
                .is_some_and(|c| c.contains("Query.user")),
            "the re-entry is stamped to the Query.user entity resolver, got {stamps:?}"
        );
        assert!(
            plan.to_string()
                .contains("Flatten(path: \"users.@.friends.@\")"),
            "the entity fetch is flattened at the friends position, got {plan}"
        );
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

    /// **KNOWN GAP — a connector's `$this` reads are not carried by the raw
    /// graph.** A connector's mapping expressions can reference sibling fields
    /// of its own parent type, and those reads are a real data dependency: the
    /// fetch cannot be dispatched correctly until they are resolved.
    ///
    /// Expansion carries them structurally. It does not read
    /// `@join__field(requires:)` at all — [`Connector::resolvable_key`] builds
    /// the synthetic subgraph's `@key` from
    /// [`variable_references`](Connector::variable_references), which spans the
    /// transport *and* the selection. `simple.graphql`'s `User.d` is
    /// `GET /{$this.c}/d` with `body: "with_b: $this.b"` and declares only
    /// `requires: "c"`, and it expands to `key: "b c"` — the `b` comes from the
    /// mapping, not from the declaration.
    ///
    /// Source-aware plans over the raw graph, where nothing encodes the `$this`
    /// reads, so it satisfies only the declared `requires`. The `User.d` fetch
    /// is therefore dispatched without `b`, and `with_b` in the request body has
    /// no value to map. Note the parity oracle cannot see this:
    /// `correctness::check_plan` validates a plan against the schemas, and `b`
    /// is not a declared requirement in the raw schema, so
    /// `raw_vs_expanded_plan_diff` classifies this operation `Equivalent`.
    ///
    /// **Why this is not a graph-only fix.** Grafting the reads onto the field
    /// edge as conditions does make the planner fetch them —
    /// [`apply_connector_parent_conditions`](crate::query_graph::connect_graph::apply_connector_parent_conditions)
    /// is written and does exactly that, and with it wired in this test passes.
    /// But the resulting plan is then rejected by `correctness::check_plan`:
    ///
    /// ```text
    /// check_require: no matching require condition found (@key didn't match)
    /// * plan requires:      { __typename  b  c  id }
    /// * @key field set:     { id  __typename }
    /// * @requires field set:{ c }
    /// ```
    ///
    /// and that rejection is correct. `@requires` names **external** fields, and
    /// in the raw supergraph `c` is `@join__field(graph: CONNECTORS, external:
    /// true)` while `b` is an ordinary `CONNECTORS` field. A connector cannot
    /// declare `requires: "b"`, because `b` is not external — it lives in the
    /// same subgraph. So federation's vocabulary has no way to state "this field
    /// needs that *sibling* field of the same subgraph," and the raw supergraph
    /// therefore cannot justify the fetch the connector actually needs.
    ///
    /// Expansion never hits this because splitting each connector into its own
    /// synthetic subgraph makes the sibling dependency *cross*-subgraph, at which
    /// point it is expressible — as the `key: "b c"` on `connectors_User_d_0`.
    /// The synthetic-subgraph split is not only a cost; it is what buys the
    /// vocabulary. Closing this needs a decision about where intra-`connectors`
    /// field dependencies are represented, not another graph pass.
    #[test]
    #[ignore = "known gap: a connector's $this reads on a same-subgraph sibling are not expressible in the raw supergraph (see doc comment)"]
    fn plans_this_variable_reads_as_fetch_inputs() {
        use crate::query_plan::requires_selection::Selection;

        let sdl = include_str!("../connectors/expand/tests/schemas/expand/simple.graphql");
        let planner = SourceAwareQueryPlanner::new(sdl, QueryPlannerConfig::default()).unwrap();

        // What the `User.d` connector actually needs, per the connector itself:
        // the same field set expansion would turn into its synthetic `@key`.
        let d_connector = planner
            .connectors()
            .iter()
            .find(|c| c.id.coordinate().contains("User.d"))
            .expect("a connector for User.d");
        let key = d_connector
            .resolvable_key(planner.planner.supergraph_schema().schema())
            .expect("resolvable_key")
            .expect("User.d resolves an implicit entity, so it has a key");
        let mut needed: Vec<String> = key
            .selection_set
            .selections
            .iter()
            .filter_map(|s| s.as_field().map(|f| f.name.to_string()))
            .collect();
        needed.sort();
        assert_eq!(
            needed,
            vec!["b".to_string(), "c".to_string()],
            "User.d reads $this.b and $this.c, so both are required"
        );

        // What the source-aware plan actually supplies as that fetch's inputs.
        let doc = ExecutableDocument::parse_and_validate(
            planner.api_schema().schema(),
            "{ users { d } }",
            "q.graphql",
        )
        .unwrap();
        let plan = planner.plan(&doc).unwrap();

        fn find_d_fetch_inputs(node: &PlanNode, out: &mut Vec<String>) {
            match node {
                PlanNode::Fetch(f) => {
                    if f.connector.as_deref().is_some_and(|c| c.contains("User.d")) {
                        collect_field_names(&f.requires, out);
                    }
                }
                PlanNode::Sequence(s) => s.nodes.iter().for_each(|n| find_d_fetch_inputs(n, out)),
                PlanNode::Parallel(p) => p.nodes.iter().for_each(|n| find_d_fetch_inputs(n, out)),
                PlanNode::Flatten(fl) => find_d_fetch_inputs(&fl.node, out),
                PlanNode::Defer(_) | PlanNode::Condition(_) => {}
            }
        }
        fn collect_field_names(selections: &[Selection], out: &mut Vec<String>) {
            for selection in selections {
                match selection {
                    Selection::Field(f) => out.push(f.name.to_string()),
                    Selection::InlineFragment(frag) => collect_field_names(&frag.selections, out),
                }
            }
        }

        let mut supplied = Vec::new();
        match &plan.node {
            Some(TopLevelPlanNode::Fetch(f)) => {
                if f.connector.as_deref().is_some_and(|c| c.contains("User.d")) {
                    collect_field_names(&f.requires, &mut supplied);
                }
            }
            Some(TopLevelPlanNode::Sequence(s)) => s
                .nodes
                .iter()
                .for_each(|n| find_d_fetch_inputs(n, &mut supplied)),
            Some(TopLevelPlanNode::Parallel(p)) => p
                .nodes
                .iter()
                .for_each(|n| find_d_fetch_inputs(n, &mut supplied)),
            Some(TopLevelPlanNode::Flatten(fl)) => find_d_fetch_inputs(&fl.node, &mut supplied),
            _ => panic!("expected a plan with a User.d fetch, got {plan}"),
        }
        assert!(
            !supplied.is_empty(),
            "expected to find the User.d fetch's inputs in {plan}"
        );

        for field in &needed {
            assert!(
                supplied.iter().any(|s| s == field),
                "the User.d fetch must be given `{field}` (it reads $this.{field}); \
                 inputs were {supplied:?} in {plan}"
            );
        }
    }
}
