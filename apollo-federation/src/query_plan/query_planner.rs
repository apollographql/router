use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::query_graph::QueryGraph;
use crate::schema::position::{AbstractTypeDefinitionPosition, InterfaceTypeDefinitionPosition};
use crate::schema::ValidFederationSchema;
use apollo_compiler::NodeStr;
use indexmap::{IndexMap, IndexSet};
use std::sync::Arc;

pub struct QueryPlannerConfig {
    /// Whether the query planner should try to reused the named fragments of the planned query in
    /// subgraph fetches.
    ///
    /// This is often a good idea as it can prevent very large subgraph queries in some cases (named
    /// fragments can make some relatively small queries (using said fragments) expand to a very large
    /// query if all the spreads are inline). However, due to architecture of the query planner, this
    /// optimization is done as an additional pass on the subgraph queries of the generated plan and
    /// can thus increase the latency of building a plan. As long as query plans are sufficiently
    /// cached, this should not be a problem, which is why this option is enabled by default, but if
    /// the distribution of inbound queries prevents efficient caching of query plans, this may become
    /// an undesirable trade-off and can be disabled in that case.
    ///
    /// Defaults to true.
    pub reuse_query_fragments: bool,

    /// Whether to run GraphQL validation against the extracted subgraph schemas. Recommended in
    /// non-production settings or when debugging.
    ///
    /// Defaults to false.
    pub subgraph_graphql_validation: bool,

    // Side-note: implemented as an object instead of single boolean because we expect to add more
    // to this soon enough. In particular, once defer-passthrough to subgraphs is implemented, the
    // idea would be to add a new `passthrough_subgraphs` option that is the list of subgraphs to
    // which we can pass-through some @defer (and it would be empty by default). Similarly, once we
    // support @stream, grouping the options here will make sense too.
    pub incremental_delivery: QueryPlanIncrementalDeliveryConfig,

    /// A sub-set of configurations that are meant for debugging or testing. All the configurations
    /// in this sub-set are provided without guarantees of stability (they may be dangerous) or
    /// continued support (they may be removed without warning).
    pub debug: QueryPlannerDebugConfig,
}

impl Default for QueryPlannerConfig {
    fn default() -> Self {
        Self {
            reuse_query_fragments: true,
            subgraph_graphql_validation: false,
            incremental_delivery: Default::default(),
            debug: Default::default(),
        }
    }
}

#[derive(Default)]
pub struct QueryPlanIncrementalDeliveryConfig {
    /// Enables @defer support by the query planner.
    ///
    /// If set, then the query plan for queries having some @defer will contains some `DeferNode`
    /// (see `query_plan/mod.rs`).
    ///
    /// Defaults to false (meaning that the @defer are ignored).
    enable_defer: bool,
}

pub struct QueryPlannerDebugConfig {
    /// If used and the supergraph is built from a single subgraph, then user queries do not go
    /// through the normal query planning and instead a fetch to the one subgraph is built directly
    /// from the input query.
    bypass_planner_for_single_subgraph: bool,

    /// Query planning is an exploratory process. Depending on the specificities and feature used by
    /// subgraphs, there could exist may different theoretical valid (if not always efficient) plans
    /// for a given query, and at a high level, the query planner generates those possible choices,
    /// evaluates them, and return the best one. In some complex cases however, the number of
    /// theoretically possible plans can be very large, and to keep query planning time acceptable,
    /// the query planner caps the maximum number of plans it evaluates. This config allows to
    /// configure that cap. Note if planning a query hits that cap, then the planner will still
    /// always return a "correct" plan, but it may not return _the_ optimal one, so this config can
    /// be considered a trade-off between the worst-time for query planning computation processing,
    /// and the risk of having non-optimal query plans (impacting query runtimes).
    ///
    /// This value currently defaults to 10000, but this default is considered an implementation
    /// detail and is subject to change. We do not recommend setting this value unless it is to
    /// debug a specific issue (with unexpectedly slow query planning for instance). Remember that
    /// setting this value too low can negatively affect query runtime (due to the use of
    /// sub-optimal query plans).
    max_evaluated_plans: u32,

    /// Before creating query plans, for each path of fields in the query we compute all the
    /// possible options to traverse that path via the subgraphs. Multiple options can arise because
    /// fields in the path can be provided by multiple subgraphs, and abstract types (i.e. unions
    /// and interfaces) returned by fields sometimes require the query planner to traverse through
    /// each constituent object type. The number of options generated in this computation can grow
    /// large if the schema or query are sufficiently complex, and that will increase the time spent
    /// planning.
    ///
    /// This config allows specifying a per-path limit to the number of options considered. If any
    /// path's options exceeds this limit, query planning will abort and the operation will fail.
    ///
    /// The default value is None, which specifies no limit.
    paths_limit: Option<u32>,
}

impl Default for QueryPlannerDebugConfig {
    fn default() -> Self {
        Self {
            bypass_planner_for_single_subgraph: false,
            max_evaluated_plans: 10000,
            paths_limit: None,
        }
    }
}

pub struct QueryPlanner {
    config: Arc<QueryPlannerConfig>,
    federated_query_graph: Arc<QueryGraph>,
    supergraph_schema: Arc<ValidFederationSchema>,
    api_schema: Arc<ValidFederationSchema>,
    subgraph_federation_spec_definitions: IndexMap<NodeStr, &'static FederationSpecDefinition>,
    /// A set of the names of interface types for which at least one subgraph use an
    /// @interfaceObject to abstract that interface.
    interface_types_with_interface_objects: IndexSet<InterfaceTypeDefinitionPosition>,
    /// A set of the names of interface or union types that have inconsistent "runtime types" across
    /// subgraphs.
    // PORT_NOTE: Named `inconsistentAbstractTypesRuntimes` in the JS codebase, which was slightly
    // confusing.
    abstract_types_with_inconsistent_runtime_types: IndexSet<AbstractTypeDefinitionPosition>,
    // TODO: Port _lastGeneratedPlanStatistics from the JS codebase in a way that keeps QueryPlanner
    // immutable.
}
