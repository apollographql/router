use std::sync::Arc;

use ahash::HashMap;
use apollo_compiler::executable::ExecutableDocument;

use super::schema2::DemandControlledSchema;
use super::DemandControlError;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::query_planner::fetch::SubgraphOperation;
use crate::query_planner::DeferredNode;
use crate::query_planner::PlanNode;
use crate::query_planner::Primary;
use crate::query_planner::QueryPlan;

pub(crate) struct StaticCostCalculator {
    default_list_size: u32,
    supergraph_schema: Arc<DemandControlledSchema>,
    subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
}

impl StaticCostCalculator {
    pub(crate) fn new(
        supergraph_schema: Arc<DemandControlledSchema>,
        subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
        list_size: u32,
    ) -> Self {
        Self {
            default_list_size: list_size,
            supergraph_schema,
            subgraph_schemas,
        }
    }

    fn score_plan_node(
        &self,
        plan_node: &PlanNode,
        variables: &Object,
    ) -> Result<f64, DemandControlError> {
        match plan_node {
            PlanNode::Sequence { nodes } => self.summed_score_of_nodes(nodes, variables),
            PlanNode::Parallel { nodes } => self.summed_score_of_nodes(nodes, variables),
            PlanNode::Flatten(flatten_node) => self.score_plan_node(&flatten_node.node, variables),
            PlanNode::Condition {
                condition: _,
                if_clause,
                else_clause,
            } => self.max_score_of_nodes(if_clause, else_clause, variables),
            PlanNode::Defer { primary, deferred } => {
                self.summed_score_of_deferred_nodes(primary, deferred, variables)
            }
            PlanNode::Fetch(fetch_node) => self.estimated_cost_of_operation(
                &fetch_node.service_name,
                &fetch_node.operation,
                variables,
            ),
            PlanNode::Subscription { primary, rest: _ } => self.estimated_cost_of_operation(
                &primary.service_name,
                &primary.operation,
                variables,
            ),
        }
    }

    fn estimated_cost_of_operation(
        &self,
        subgraph: &str,
        operation: &SubgraphOperation,
        variables: &Object,
    ) -> Result<f64, DemandControlError> {
        tracing::debug!("On subgraph {}, scoring operation: {}", subgraph, operation);

        let schema = self.subgraph_schemas.get(subgraph).ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Query planner did not provide a schema for service {}",
                subgraph
            ))
        })?;

        let operation = operation
            .as_parsed()
            .map_err(DemandControlError::SubgraphOperationNotInitialized)?;
        self.estimated(operation, schema, variables, false)
    }

    fn max_score_of_nodes(
        &self,
        left: &Option<Box<PlanNode>>,
        right: &Option<Box<PlanNode>>,
        variables: &Object,
    ) -> Result<f64, DemandControlError> {
        match (left, right) {
            (None, None) => Ok(0.0),
            (None, Some(right)) => self.score_plan_node(right, variables),
            (Some(left), None) => self.score_plan_node(left, variables),
            (Some(left), Some(right)) => {
                let left_score = self.score_plan_node(left, variables)?;
                let right_score = self.score_plan_node(right, variables)?;
                Ok(left_score.max(right_score))
            }
        }
    }

    fn summed_score_of_deferred_nodes(
        &self,
        primary: &Primary,
        deferred: &Vec<DeferredNode>,
        variables: &Object,
    ) -> Result<f64, DemandControlError> {
        let mut score = 0.0;
        if let Some(node) = &primary.node {
            score += self.score_plan_node(node, variables)?;
        }
        for d in deferred {
            if let Some(node) = &d.node {
                score += self.score_plan_node(node, variables)?;
            }
        }
        Ok(score)
    }

    fn summed_score_of_nodes(
        &self,
        nodes: &Vec<PlanNode>,
        variables: &Object,
    ) -> Result<f64, DemandControlError> {
        let mut sum = 0.0;
        for node in nodes {
            sum += self.score_plan_node(node, variables)?;
        }
        Ok(sum)
    }

    pub(crate) fn estimated(
        &self,
        query: &ExecutableDocument,
        schema: &DemandControlledSchema,
        variables: &Object,
        _should_estimate_requires: bool,
    ) -> Result<f64, DemandControlError> {
        schema.score_request(query, variables, self.default_list_size)
    }

    pub(crate) fn planned(
        &self,
        query_plan: &QueryPlan,
        variables: &Object,
    ) -> Result<f64, DemandControlError> {
        self.score_plan_node(&query_plan.root, variables)
    }

    pub(crate) fn actual(
        &self,
        request: &ExecutableDocument,
        response: &Response,
        variables: &Object,
    ) -> Result<f64, DemandControlError> {
        self.supergraph_schema
            .score_response(request, response, variables, self.default_list_size)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ahash::HashMapExt;
    use apollo_federation::query_plan::query_planner::QueryPlanner;
    use bytes::Bytes;
    use router_bridge::planner::PlanOptions;
    use serde_json_bytes::Value;
    use test_log::test;
    use tower::Service;

    use super::*;
    use crate::introspection::IntrospectionCache;
    use crate::plugins::authorization::CacheKeyMetadata;
    use crate::query_planner::BridgeQueryPlanner;
    use crate::services::layers::query_analysis::ParsedDocument;
    use crate::services::QueryPlannerContent;
    use crate::services::QueryPlannerRequest;
    use crate::spec;
    use crate::spec::Query;
    use crate::Configuration;
    use crate::Context;

    impl StaticCostCalculator {
        fn rust_planned(
            &self,
            query_plan: &apollo_federation::query_plan::QueryPlan,
            variables: &Object,
        ) -> Result<f64, DemandControlError> {
            let js_planner_node: PlanNode = query_plan.node.as_ref().unwrap().into();
            self.score_plan_node(&js_planner_node, variables)
        }
    }

    fn parse_schema_and_operation(
        schema_str: &str,
        query_str: &str,
        config: &Configuration,
    ) -> (spec::Schema, ParsedDocument) {
        let schema = spec::Schema::parse(schema_str, config).unwrap();
        let query = Query::parse_document(query_str, None, &schema, config).unwrap();
        (schema, query)
    }

    /// Estimate cost of an operation executed on a supergraph.
    fn estimated_cost(schema_str: &str, query_str: &str, variables_str: &str) -> f64 {
        let (schema, query) =
            parse_schema_and_operation(schema_str, query_str, &Default::default());
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let schema =
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap();
        let calculator = StaticCostCalculator::new(Arc::new(schema), Default::default(), 100);

        calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &variables,
                true,
            )
            .unwrap()
    }

    /// Estimate cost of an operation on a plain, non-federated schema.
    fn basic_estimated_cost(schema_str: &str, query_str: &str, variables_str: &str) -> f64 {
        let schema =
            apollo_compiler::Schema::parse_and_validate(schema_str, "schema.graphqls").unwrap();
        let query = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            query_str,
            "query.graphql",
        )
        .unwrap();
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let schema = DemandControlledSchema::new(Arc::new(schema)).unwrap();
        let calculator = StaticCostCalculator::new(Arc::new(schema), Default::default(), 100);

        calculator
            .estimated(&query, &calculator.supergraph_schema, &variables, true)
            .unwrap()
    }

    async fn planned_cost_js(schema_str: &str, query_str: &str, variables_str: &str) -> f64 {
        let config: Arc<Configuration> = Arc::new(Default::default());
        let (schema, query) = parse_schema_and_operation(schema_str, query_str, &config);
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let supergraph_schema = schema.supergraph_schema().clone();

        let mut planner = BridgeQueryPlanner::new(
            schema.into(),
            config.clone(),
            None,
            None,
            Arc::new(IntrospectionCache::new(&config)),
        )
        .await
        .unwrap();

        let ctx = Context::new();
        ctx.extensions()
            .with_lock(|mut lock| lock.insert::<ParsedDocument>(query.clone()));

        let planner_res = planner
            .call(QueryPlannerRequest::new(
                query_str.to_string(),
                None,
                query,
                CacheKeyMetadata::default(),
                PlanOptions::default(),
            ))
            .await
            .unwrap();
        let query_plan = match planner_res.content.unwrap() {
            QueryPlannerContent::Plan { plan } => plan,
            _ => panic!("Query planner returned unexpected non-plan content"),
        };

        let schema = DemandControlledSchema::new(Arc::new(supergraph_schema)).unwrap();
        let mut demand_controlled_subgraph_schemas = HashMap::new();
        for (subgraph_name, subgraph_schema) in planner.subgraph_schemas().iter() {
            let demand_controlled_subgraph_schema =
                DemandControlledSchema::new(subgraph_schema.clone()).unwrap();
            demand_controlled_subgraph_schemas
                .insert(subgraph_name.to_string(), demand_controlled_subgraph_schema);
        }

        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Arc::new(demand_controlled_subgraph_schemas),
            100,
        );

        calculator.planned(&query_plan, &variables).unwrap()
    }

    fn planned_cost_rust(schema_str: &str, query_str: &str, variables_str: &str) -> f64 {
        let config: Arc<Configuration> = Arc::new(Default::default());
        let (schema, query) = parse_schema_and_operation(schema_str, query_str, &config);
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();

        let planner =
            QueryPlanner::new(schema.federation_supergraph(), Default::default()).unwrap();

        let query_plan = planner
            .build_query_plan(&query.executable, None, Default::default())
            .unwrap();

        let schema =
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap();
        let mut demand_controlled_subgraph_schemas = HashMap::new();
        for (subgraph_name, subgraph_schema) in planner.subgraph_schemas().iter() {
            let demand_controlled_subgraph_schema =
                DemandControlledSchema::new(Arc::new(subgraph_schema.schema().clone())).unwrap();
            demand_controlled_subgraph_schemas
                .insert(subgraph_name.to_string(), demand_controlled_subgraph_schema);
        }

        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Arc::new(demand_controlled_subgraph_schemas),
            100,
        );

        calculator.rust_planned(&query_plan, &variables).unwrap()
    }

    fn actual_cost(
        schema_str: &str,
        query_str: &str,
        variables_str: &str,
        response_bytes: &'static [u8],
    ) -> f64 {
        let (schema, query) =
            parse_schema_and_operation(schema_str, query_str, &Default::default());
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let response = Response::from_bytes("test", Bytes::from(response_bytes)).unwrap();
        let schema =
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap();
        StaticCostCalculator::new(Arc::new(schema), Default::default(), 100)
            .actual(&query.executable, &response, &variables)
            .unwrap()
    }

    /// Actual cost of an operation on a plain, non-federated schema.
    fn basic_actual_cost(
        schema_str: &str,
        query_str: &str,
        variables_str: &str,
        response_bytes: &'static [u8],
    ) -> f64 {
        let schema =
            apollo_compiler::Schema::parse_and_validate(schema_str, "schema.graphqls").unwrap();
        let query = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            query_str,
            "query.graphql",
        )
        .unwrap();
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let response = Response::from_bytes("test", Bytes::from(response_bytes)).unwrap();

        let schema = DemandControlledSchema::new(Arc::new(schema)).unwrap();
        StaticCostCalculator::new(Arc::new(schema), Default::default(), 100)
            .actual(&query, &response, &variables)
            .unwrap()
    }

    #[test]
    fn query_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 0.0)
    }

    #[test]
    fn mutation_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_mutation.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 10.0)
    }

    #[test]
    fn object_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_object_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 1.0)
    }

    #[test]
    fn interface_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_interface_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 1.0)
    }

    #[test]
    fn union_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_union_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 1.0)
    }

    #[test]
    fn list_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_object_list_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 100.0)
    }

    #[test]
    fn scalar_list_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_scalar_list_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 0.0)
    }

    #[test]
    fn nested_object_lists() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_nested_list_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 10100.0)
    }

    #[test]
    fn input_object_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_input_object_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 4.0)
    }

    #[test]
    fn input_object_cost_with_returned_objects() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_input_object_query_2.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/basic_input_object_response.json");

        assert_eq!(basic_estimated_cost(schema, query, variables), 104.0);
        // The cost of the arguments from the query should be included when scoring the response
        assert_eq!(basic_actual_cost(schema, query, variables, response), 7.0);
    }

    #[test]
    fn skip_directive_excludes_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_skipped_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 0.0)
    }

    #[test]
    fn include_directive_excludes_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_excluded_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 0.0)
    }

    #[test(tokio::test)]
    async fn fragments_cost() {
        let schema = include_str!("./fixtures/basic_supergraph_schema.graphql");
        let query = include_str!("./fixtures/basic_fragments_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 102.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 102.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 102.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_name() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_named_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_named_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 100.0);
        assert_eq!(actual_cost(schema, query, variables, response), 2.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_requires() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_required_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_required_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 10200.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 10400.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 10400.0);
        assert_eq!(actual_cost(schema, query, variables, response), 2.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_fragments() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_fragment_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_fragment_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 300.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 400.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 400.0);
        assert_eq!(actual_cost(schema, query, variables, response), 6.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_inline_fragments() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_inline_fragment_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_fragment_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 300.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 400.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 400.0);
        assert_eq!(actual_cost(schema, query, variables, response), 6.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_defer() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_deferred_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_deferred_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 10200.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 10400.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 10400.0);
        assert_eq!(actual_cost(schema, query, variables, response), 2.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_adjustable_list_cost() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_deferred_query.graphql");
        let (schema, query) = parse_schema_and_operation(schema, query, &Default::default());
        let schema = Arc::new(
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap(),
        );

        let calculator = StaticCostCalculator::new(schema.clone(), Default::default(), 100);
        let conservative_estimate = calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &Default::default(),
                true,
            )
            .unwrap();

        let calculator = StaticCostCalculator::new(schema.clone(), Default::default(), 5);
        let narrow_estimate = calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &Default::default(),
                true,
            )
            .unwrap();

        assert_eq!(conservative_estimate, 10200.0);
        assert_eq!(narrow_estimate, 35.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query_asdf() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query = include_str!("./fixtures/custom_cost_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/custom_cost_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 127.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 127.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 127.0);
        assert_eq!(actual_cost(schema, query, variables, response), 125.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query_with_renamed_directives() {
        let schema = include_str!("./fixtures/custom_cost_schema_with_renamed_directives.graphql");
        let query = include_str!("./fixtures/custom_cost_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/custom_cost_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 127.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 127.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 127.0);
        assert_eq!(actual_cost(schema, query, variables, response), 125.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query_with_default_slicing_argument() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query =
            include_str!("./fixtures/custom_cost_query_with_default_slicing_argument.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/custom_cost_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 132.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 132.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 132.0);
        assert_eq!(actual_cost(schema, query, variables, response), 125.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query_with_variable_slicing_argument() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query =
            include_str!("./fixtures/custom_cost_query_with_variable_slicing_argument.graphql");
        let variables = r#"{"costlyInput": {"somethingWithCost": 10}, "fieldCountVar": 5}"#;
        let response = include_bytes!("./fixtures/custom_cost_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 127.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 127.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 127.0);
        assert_eq!(actual_cost(schema, query, variables, response), 125.0);
    }

    #[test]
    fn arbitrary_json_as_custom_scalar_in_variables() {
        let schema = include_str!("./fixtures/arbitrary_json_schema.graphql");
        let query = r#"
            query FetchData($myJsonValue: ArbitraryJson) {
                fetch(args: {
                    json: $myJsonValue
                })
            }
        "#;
        let variables = r#"
            {
                "myJsonValue": {
                    "field.with.dots": 1
                }
            }
        "#;

        assert_eq!(estimated_cost(schema, query, variables), 1.0);
    }
}
