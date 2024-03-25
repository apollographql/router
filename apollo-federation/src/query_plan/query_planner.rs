use crate::error::FederationError;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Identity;
use crate::query_graph::build_federated_query_graph;
use crate::query_graph::QueryGraph;
use crate::query_plan::operation::normalize_operation;
use crate::query_plan::QueryPlan;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::ApiSchemaOptions;
use crate::Supergraph;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::NodeStr;
use indexmap::IndexMap;
use indexmap::IndexSet;
use std::sync::Arc;

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Default)]
pub struct QueryPlanIncrementalDeliveryConfig {
    /// Enables @defer support by the query planner.
    ///
    /// If set, then the query plan for queries having some @defer will contains some `DeferNode`
    /// (see `query_plan/mod.rs`).
    ///
    /// Defaults to false (meaning that the @defer are ignored).
    pub enable_defer: bool,
}

#[derive(Debug, Clone)]
pub struct QueryPlannerDebugConfig {
    /// If used and the supergraph is built from a single subgraph, then user queries do not go
    /// through the normal query planning and instead a fetch to the one subgraph is built directly
    /// from the input query.
    pub bypass_planner_for_single_subgraph: bool,

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
    // TODO: should there additionally be a max_evaluated_cost?
    pub max_evaluated_plans: u32,

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
    pub paths_limit: Option<u32>,
}

impl Default for QueryPlannerDebugConfig {
    fn default() -> Self {
        Self {
            bypass_planner_for_single_subgraph: false,
            max_evaluated_plans: 10_000,
            paths_limit: None,
        }
    }
}

pub struct QueryPlanner {
    config: QueryPlannerConfig,
    federated_query_graph: Arc<QueryGraph>,
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    subgraph_federation_spec_definitions: Arc<IndexMap<NodeStr, &'static FederationSpecDefinition>>,
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
    // last_generated_plan_statistics: QueryPlanningStatistics,
}

impl QueryPlanner {
    pub fn new(
        supergraph: &Supergraph,
        config: QueryPlannerConfig,
    ) -> Result<Self, FederationError> {
        let supergraph_schema = supergraph.schema.clone();
        let api_schema = supergraph.to_api_schema(ApiSchemaOptions {
            include_defer: config.incremental_delivery.enable_defer,
            ..Default::default()
        })?;
        let query_graph = build_federated_query_graph(
            supergraph_schema.clone(),
            api_schema.clone(),
            Some(true),
            Some(true),
        )?;

        let metadata = supergraph_schema.metadata().unwrap();

        let federation_link = metadata.for_identity(&Identity::federation_identity());
        let interface_object_directive =
            federation_link.map_or(FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC, |link| {
                link.directive_name_in_schema(&FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC)
            });

        let is_interface_object =
            |ty: &ExtendedType| ty.is_object() && ty.directives().has(&interface_object_directive);

        let interface_types_with_interface_objects = supergraph
            .schema
            .get_types()
            .filter_map(|position| match position {
                TypeDefinitionPosition::Interface(interface_position) => Some(interface_position),
                _ => None,
            })
            .filter(|position| {
                query_graph.sources().any(|schema| {
                    schema
                        .schema()
                        .types
                        .get(&position.type_name)
                        .is_some_and(is_interface_object)
                })
            })
            .collect::<IndexSet<_>>();

        let is_inconsistent = |position: AbstractTypeDefinitionPosition| {
            let mut sources = query_graph.sources().filter_map(|subgraph| {
                match subgraph.try_get_type(position.type_name().clone())? {
                    // This is only called for type names that are abstract in the supergraph, so it
                    // can only be an object in a subgraph if it is an `@interfaceObject`. And as `@interfaceObject`s
                    // "stand-in" for all possible runtime types, they don't create inconsistencies by themselves
                    // and we can ignore them.
                    TypeDefinitionPosition::Object(_) => None,
                    TypeDefinitionPosition::Interface(interface) => Some(
                        subgraph
                            .referencers()
                            .get_interface_type(&interface.type_name)
                            .ok()?
                            .object_types
                            .clone(),
                    ),
                    TypeDefinitionPosition::Union(union_) => Some(
                        union_
                            .try_get(subgraph.schema())?
                            .members
                            .iter()
                            .map(|member| ObjectTypeDefinitionPosition::new(member.name.clone()))
                            .collect(),
                    ),
                    _ => None,
                }
            });

            let Some(expected_runtimes) = sources.next() else {
                return false;
            };
            sources.all(|runtimes| runtimes == expected_runtimes)
        };

        let abstract_types_with_inconsistent_runtime_types = supergraph
            .schema
            .get_types()
            .filter_map(|position| AbstractTypeDefinitionPosition::try_from(position).ok())
            .filter(|position| is_inconsistent(position.clone()))
            .collect::<IndexSet<_>>();

        // PORT_NOTE: JS prepares a map of override conditions here, which is
        // a map where the keys are all `@join__field(overrideLabel:)` argument values
        // and the values are all initialised to `false`. Instead of doing that, we should
        // be able to use a Set where presence means `true` and absence means `false`.

        Ok(Self {
            config,
            federated_query_graph: Arc::new(query_graph),
            supergraph_schema,
            api_schema,
            // TODO(@goto-bus-stop): not sure how this is going to be used,
            // keeping empty for the moment
            subgraph_federation_spec_definitions: Default::default(),
            interface_types_with_interface_objects,
            abstract_types_with_inconsistent_runtime_types,
        })
    }

    // PORT_NOTE: this receives an `Operation` object in JS which is a concept that doesn't exist in apollo-rs.
    pub fn build_query_plan(
        &self,
        document: &Valid<ExecutableDocument>,
        operation_name: Option<&str>,
    ) -> Result<QueryPlan, FederationError> {
        let operation = document
            .get_operation(operation_name)
            // TODO(@goto-bus-stop) this is not an internal error
            .map_err(|_| FederationError::internal("requested operation does not exist"))?;

        // TODO(@goto-bus-stop) this isn't valid GraphQL so it should never happen given a
        // `Valid<ExecutableDocument>`?
        // The JS code checks this *again* post-normalization. That should be enough
        if operation.selection_set.selections.is_empty() {
            return Ok(QueryPlan::default());
        }

        if self.config.debug.bypass_planner_for_single_subgraph {
            todo!("return a single fetch node for the whole operation");
        }

        let normalized_operation = normalize_operation(
            operation,
            &document.fragments,
            &self.api_schema,
            &self.interface_types_with_interface_objects,
        )?;

        // TODO(@goto-bus-stop): some defer stuff

        if normalized_operation.selection_set.selections.is_empty() {
            return Ok(QueryPlan::default());
        }

        todo!("the rest of the owl")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SUPERGRAPH: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
{
  query: Query
}

directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

type Book implements Product
  @join__implements(graph: PRODUCTS, interface: "Product")
  @join__implements(graph: REVIEWS, interface: "Product")
  @join__type(graph: PRODUCTS, key: "id")
  @join__type(graph: REVIEWS, key: "id")
{
  id: ID!
  price: Price @join__field(graph: PRODUCTS)
  title: String @join__field(graph: PRODUCTS)
  vendor: User @join__field(graph: PRODUCTS)
  pages: Int @join__field(graph: PRODUCTS)
  avg_rating: Int @join__field(graph: PRODUCTS, requires: "reviews { rating }")
  reviews: [Review] @join__field(graph: PRODUCTS, external: true) @join__field(graph: REVIEWS)
}

enum Currency
  @join__type(graph: PRODUCTS)
{
  USD
  EUR
}

scalar join__FieldSet

enum join__Graph {
  ACCOUNTS @join__graph(name: "accounts", url: "")
  PRODUCTS @join__graph(name: "products", url: "")
  REVIEWS @join__graph(name: "reviews", url: "")
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

type Movie implements Product
  @join__implements(graph: PRODUCTS, interface: "Product")
  @join__implements(graph: REVIEWS, interface: "Product")
  @join__type(graph: PRODUCTS, key: "id")
  @join__type(graph: REVIEWS, key: "id")
{
  id: ID!
  price: Price @join__field(graph: PRODUCTS)
  title: String @join__field(graph: PRODUCTS)
  vendor: User @join__field(graph: PRODUCTS)
  length_minutes: Int @join__field(graph: PRODUCTS)
  avg_rating: Int @join__field(graph: PRODUCTS, requires: "reviews { rating }")
  reviews: [Review] @join__field(graph: PRODUCTS, external: true) @join__field(graph: REVIEWS)
}

type Price
  @join__type(graph: PRODUCTS)
{
  value: Int
  currency: Currency
}

interface Product
  @join__type(graph: PRODUCTS)
  @join__type(graph: REVIEWS)
{
  id: ID!
  price: Price @join__field(graph: PRODUCTS)
  vendor: User @join__field(graph: PRODUCTS)
  avg_rating: Int @join__field(graph: PRODUCTS)
  reviews: [Review] @join__field(graph: REVIEWS)
}

type Query
  @join__type(graph: ACCOUNTS)
  @join__type(graph: PRODUCTS)
  @join__type(graph: REVIEWS)
{
  userById(id: ID!): User @join__field(graph: ACCOUNTS)
  me: User! @join__field(graph: ACCOUNTS) @join__field(graph: REVIEWS)
  productById(id: ID!): Product @join__field(graph: PRODUCTS)
  search(filter: SearchFilter): [Product] @join__field(graph: PRODUCTS)
  bestRatedProducts(limit: Int): [Product] @join__field(graph: REVIEWS)
}

type Review
  @join__type(graph: PRODUCTS)
  @join__type(graph: REVIEWS)
{
  rating: Int @join__field(graph: PRODUCTS, external: true) @join__field(graph: REVIEWS)
  product: Product @join__field(graph: REVIEWS)
  author: User @join__field(graph: REVIEWS)
  text: String @join__field(graph: REVIEWS)
}

input SearchFilter
  @join__type(graph: PRODUCTS)
{
  pattern: String!
  vendorName: String
}

type User
  @join__type(graph: ACCOUNTS, key: "id")
  @join__type(graph: PRODUCTS, key: "id", resolvable: false)
  @join__type(graph: REVIEWS, key: "id")
{
  id: ID!
  name: String @join__field(graph: ACCOUNTS)
  email: String @join__field(graph: ACCOUNTS)
  password: String @join__field(graph: ACCOUNTS)
  nickname: String @join__field(graph: ACCOUNTS, override: "reviews")
  reviews: [Review] @join__field(graph: REVIEWS)
}
    "#;

    #[test]
    fn it_does_not_crash() {
        let supergraph = Supergraph::new(TEST_SUPERGRAPH).unwrap();
        let _planner = QueryPlanner::new(&supergraph, Default::default()).unwrap();

        let _document = ExecutableDocument::parse_and_validate(
            supergraph
                .to_api_schema(Default::default())
                .unwrap()
                .schema(),
            "{ userById(id: 1) { name email } }",
            "operation.graphql",
        )
        .unwrap();
        // This part does crash :)
        // let _plan = planner.build_query_plan(&document, None).unwrap();
    }
}
