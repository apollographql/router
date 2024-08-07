use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentSpread;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use serde_json_bytes::Value;

use super::directives::get_apollo_directive_names;
use super::directives::IncludeDirective;
use super::directives::RequiresDirective;
use super::directives::SkipDirective;
use super::DemandControlError;
use crate::graphql::Response;
use crate::graphql::ResponseVisitor;
use crate::plugins::demand_control::cost_calculator::directives::CostDirective;
use crate::plugins::demand_control::cost_calculator::directives::ListSizeDirective;
use crate::query_planner::fetch::SubgraphOperation;
use crate::query_planner::fetch::SubgraphSchemas;
use crate::query_planner::DeferredNode;
use crate::query_planner::PlanNode;
use crate::query_planner::Primary;
use crate::query_planner::QueryPlan;

pub(crate) struct StaticCostCalculator {
    list_size: u32,
    subgraph_schemas: Arc<SubgraphSchemas>,
    directive_name_map: HashMap<Name, Name>,
}

impl StaticCostCalculator {
    pub(crate) fn new(
        supergraph_schema: &Valid<Schema>,
        subgraph_schemas: Arc<SubgraphSchemas>,
        list_size: u32,
    ) -> Self {
        let directive_name_map = get_apollo_directive_names(supergraph_schema).unwrap_or_default();

        Self {
            list_size,
            subgraph_schemas,
            directive_name_map,
        }
    }

    /// Scores a field within a GraphQL operation, handling some expected cases where
    /// directives change how the query is fetched. In the case of the federation
    /// directive `@requires`, the cost of the required selection is added to the
    /// cost of the current field. There's a chance this double-counts the cost of
    /// a selection if two fields require the same thing, or if a field is selected
    /// along with a field that it requires.
    ///
    /// ```graphql
    /// type Query {
    ///     foo: Foo @external
    ///     bar: Bar @requires(fields: "foo")
    ///     baz: Baz @requires(fields: "foo")
    /// }
    /// ```
    ///
    /// This should be okay, as we don't want this implementation to have to know about
    /// any deduplication happening in the query planner, and we're estimating an upper
    /// bound for cost anyway.
    fn score_field(
        &self,
        field: &Field,
        parent_type: &NamedType,
        schema: &Valid<Schema>,
        executable: &ExecutableDocument,
        should_estimate_requires: bool,
        list_size_from_upstream: Option<i32>,
    ) -> Result<f64, DemandControlError> {
        if StaticCostCalculator::skipped_by_directives(field) {
            return Ok(0.0);
        }

        // We need to look up the `FieldDefinition` from the supergraph schema instead of using `field.definition`
        // because `field.definition` was generated from the API schema, which strips off the directives we need.
        let definition = schema.type_field(parent_type, &field.name)?;
        let ty = field.inner_type_def(schema).ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Field {} was found in query, but its type is missing from the schema.",
                field.name
            ))
        })?;

        let list_size_directive =
            ListSizeDirective::from_field(&self.directive_name_map, field, definition)?;
        let instance_count = if !field.ty().is_list() {
            1
        } else if let Some(value) = list_size_from_upstream {
            // This is a sized field whose length is defined by the `@listSize` directive on the parent field
            value
        } else if let Some(expected_size) = list_size_directive
            .as_ref()
            .and_then(|list_size| list_size.expected_size)
        {
            expected_size
        } else {
            self.list_size as i32
        };

        // Determine the cost for this particular field. Scalars are free, non-scalars are not.
        // For fields with selections, add in the cost of the selections as well.
        let mut type_cost = if let Some(cost_directive) =
            CostDirective::from_field(&self.directive_name_map, definition)
                .or(CostDirective::from_type(&self.directive_name_map, ty))
        {
            cost_directive.weight()
        } else if ty.is_interface() || ty.is_object() || ty.is_union() {
            1.0
        } else {
            0.0
        };
        type_cost += self.score_selection_set(
            &field.selection_set,
            field.ty().inner_named_type(),
            schema,
            executable,
            should_estimate_requires,
            list_size_directive.as_ref(),
        )?;

        for argument in &definition.arguments {
            type_cost += self.score_argument(argument, schema)?;
        }

        let mut requirements_cost = 0.0;
        if should_estimate_requires {
            // If the field is marked with `@requires`, the required selection may not be included
            // in the query's selection. Adding that requirement's cost to the field ensures it's
            // accounted for.
            let requirements =
                RequiresDirective::from_field(field, parent_type, schema)?.map(|d| d.fields);
            if let Some(selection_set) = requirements {
                requirements_cost = self.score_selection_set(
                    &selection_set,
                    parent_type,
                    schema,
                    executable,
                    should_estimate_requires,
                    list_size_directive.as_ref(),
                )?;
            }
        }

        let cost = (instance_count as f64) * type_cost + requirements_cost;
        tracing::debug!(
            "Field {} cost breakdown: (count) {} * (type cost) {} + (requirements) {} = {}",
            field.name,
            instance_count,
            type_cost,
            requirements_cost,
            cost
        );

        Ok(cost)
    }

    fn score_argument(
        &self,
        argument: &InputValueDefinition,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        let cost_directive = CostDirective::from_argument(&self.directive_name_map, argument);
        if let Some(ty) = schema.types.get(argument.ty.inner_named_type().as_str()) {
            match ty {
                apollo_compiler::schema::ExtendedType::InputObject(inner_arguments) => {
                    let mut cost = cost_directive.map_or(1.0, |cost| cost.weight());
                    for inner_argument in inner_arguments.fields.values() {
                        cost += self.score_argument(inner_argument, schema)?;
                    }
                    Ok(cost)
                }

                apollo_compiler::schema::ExtendedType::Scalar(_)
                | apollo_compiler::schema::ExtendedType::Enum(_) => Ok(cost_directive.map_or(0.0, |cost| cost.weight())),

                apollo_compiler::schema::ExtendedType::Object(_)
                | apollo_compiler::schema::ExtendedType::Interface(_)
                | apollo_compiler::schema::ExtendedType::Union(_) => {
                    Err(DemandControlError::QueryParseFailure(
                        format!("Argument {} has type {}, but objects, interfaces, and unions are disallowed in this position", argument.name, argument.ty.inner_named_type())
                    ))
                }
            }
        } else {
            Err(DemandControlError::QueryParseFailure(format!(
                "Argument {} was found in query, but its type ({}) was not found in the schema",
                argument.name,
                argument.ty.inner_named_type()
            )))
        }
    }

    fn score_fragment_spread(
        &self,
        fragment_spread: &FragmentSpread,
        parent_type: &NamedType,
        schema: &Valid<Schema>,
        executable: &ExecutableDocument,
        should_estimate_requires: bool,
        list_size_directive: Option<&ListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        let fragment = fragment_spread.fragment_def(executable).ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Parsed operation did not have a definition for fragment {}",
                fragment_spread.fragment_name
            ))
        })?;
        self.score_selection_set(
            &fragment.selection_set,
            parent_type,
            schema,
            executable,
            should_estimate_requires,
            list_size_directive,
        )
    }

    fn score_inline_fragment(
        &self,
        inline_fragment: &InlineFragment,
        parent_type: &NamedType,
        schema: &Valid<Schema>,
        executable: &ExecutableDocument,
        should_estimate_requires: bool,
        list_size_directive: Option<&ListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        self.score_selection_set(
            &inline_fragment.selection_set,
            parent_type,
            schema,
            executable,
            should_estimate_requires,
            list_size_directive,
        )
    }

    fn score_operation(
        &self,
        operation: &Operation,
        schema: &Valid<Schema>,
        executable: &ExecutableDocument,
        should_estimate_requires: bool,
    ) -> Result<f64, DemandControlError> {
        let mut cost = if operation.is_mutation() { 10.0 } else { 0.0 };

        let Some(root_type_name) = schema.root_operation(operation.operation_type) else {
            return Err(DemandControlError::QueryParseFailure(format!(
                "Cannot cost {} operation because the schema does not support this root type",
                operation.operation_type
            )));
        };

        cost += self.score_selection_set(
            &operation.selection_set,
            root_type_name,
            schema,
            executable,
            should_estimate_requires,
            None,
        )?;

        Ok(cost)
    }

    fn score_selection(
        &self,
        selection: &Selection,
        parent_type: &NamedType,
        schema: &Valid<Schema>,
        executable: &ExecutableDocument,
        should_estimate_requires: bool,
        list_size_directive: Option<&ListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        match selection {
            Selection::Field(f) => self.score_field(
                f,
                parent_type,
                schema,
                executable,
                should_estimate_requires,
                list_size_directive.and_then(|dir| dir.size_of(f)),
            ),
            Selection::FragmentSpread(s) => self.score_fragment_spread(
                s,
                parent_type,
                schema,
                executable,
                should_estimate_requires,
                list_size_directive,
            ),
            Selection::InlineFragment(i) => self.score_inline_fragment(
                i,
                i.type_condition.as_ref().unwrap_or(parent_type),
                schema,
                executable,
                should_estimate_requires,
                list_size_directive,
            ),
        }
    }

    fn score_selection_set(
        &self,
        selection_set: &SelectionSet,
        parent_type_name: &NamedType,
        schema: &Valid<Schema>,
        executable: &ExecutableDocument,
        should_estimate_requires: bool,
        list_size_directive: Option<&ListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        for selection in selection_set.selections.iter() {
            cost += self.score_selection(
                selection,
                parent_type_name,
                schema,
                executable,
                should_estimate_requires,
                list_size_directive,
            )?;
        }
        Ok(cost)
    }

    fn skipped_by_directives(field: &Field) -> bool {
        let include_directive = IncludeDirective::from_field(field);
        if let Ok(Some(IncludeDirective { is_included: false })) = include_directive {
            return true;
        }

        let skip_directive = SkipDirective::from_field(field);
        if let Ok(Some(SkipDirective { is_skipped: true })) = skip_directive {
            return true;
        }

        false
    }

    fn score_plan_node(&self, plan_node: &PlanNode) -> Result<f64, DemandControlError> {
        match plan_node {
            PlanNode::Sequence { nodes } => self.summed_score_of_nodes(nodes),
            PlanNode::Parallel { nodes } => self.summed_score_of_nodes(nodes),
            PlanNode::Flatten(flatten_node) => self.score_plan_node(&flatten_node.node),
            PlanNode::Condition {
                condition: _,
                if_clause,
                else_clause,
            } => self.max_score_of_nodes(if_clause, else_clause),
            PlanNode::Defer { primary, deferred } => {
                self.summed_score_of_deferred_nodes(primary, deferred)
            }
            PlanNode::Fetch(fetch_node) => {
                self.estimated_cost_of_operation(&fetch_node.service_name, &fetch_node.operation)
            }
            PlanNode::Subscription { primary, rest: _ } => {
                self.estimated_cost_of_operation(&primary.service_name, &primary.operation)
            }
        }
    }

    fn estimated_cost_of_operation(
        &self,
        subgraph: &str,
        operation: &SubgraphOperation,
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
        self.estimated(operation, schema, false)
    }

    fn max_score_of_nodes(
        &self,
        left: &Option<Box<PlanNode>>,
        right: &Option<Box<PlanNode>>,
    ) -> Result<f64, DemandControlError> {
        match (left, right) {
            (None, None) => Ok(0.0),
            (None, Some(right)) => self.score_plan_node(right),
            (Some(left), None) => self.score_plan_node(left),
            (Some(left), Some(right)) => {
                let left_score = self.score_plan_node(left)?;
                let right_score = self.score_plan_node(right)?;
                Ok(left_score.max(right_score))
            }
        }
    }

    fn summed_score_of_deferred_nodes(
        &self,
        primary: &Primary,
        deferred: &Vec<DeferredNode>,
    ) -> Result<f64, DemandControlError> {
        let mut score = 0.0;
        if let Some(node) = &primary.node {
            score += self.score_plan_node(node)?;
        }
        for d in deferred {
            if let Some(node) = &d.node {
                score += self.score_plan_node(node)?;
            }
        }
        Ok(score)
    }

    fn summed_score_of_nodes(&self, nodes: &Vec<PlanNode>) -> Result<f64, DemandControlError> {
        let mut sum = 0.0;
        for node in nodes {
            sum += self.score_plan_node(node)?;
        }
        Ok(sum)
    }

    pub(crate) fn estimated(
        &self,
        query: &ExecutableDocument,
        schema: &Valid<Schema>,
        should_estimate_requires: bool,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        if let Some(op) = &query.operations.anonymous {
            cost += self.score_operation(op, schema, query, should_estimate_requires)?;
        }
        for (_name, op) in query.operations.named.iter() {
            cost += self.score_operation(op, schema, query, should_estimate_requires)?;
        }
        Ok(cost)
    }

    pub(crate) fn planned(&self, query_plan: &QueryPlan) -> Result<f64, DemandControlError> {
        self.score_plan_node(&query_plan.root)
    }

    pub(crate) fn actual(
        &self,
        request: &ExecutableDocument,
        response: &Response,
    ) -> Result<f64, DemandControlError> {
        let mut visitor = ResponseCostCalculator::new();
        visitor.visit(request, response);
        Ok(visitor.cost)
    }
}

pub(crate) struct ResponseCostCalculator {
    pub(crate) cost: f64,
}

impl ResponseCostCalculator {
    pub(crate) fn new() -> Self {
        Self { cost: 0.0 }
    }
}

impl ResponseVisitor for ResponseCostCalculator {
    fn visit_field(
        &mut self,
        request: &ExecutableDocument,
        _ty: &NamedType,
        field: &Field,
        value: &Value,
    ) {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            Value::Array(items) => {
                for item in items {
                    self.visit_field(request, field.ty().inner_named_type(), field, item);
                }
            }
            Value::Object(children) => {
                self.cost += 1.0;
                self.visit_selections(request, &field.selection_set, children);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_federation::query_plan::query_planner::QueryPlanner;
    use bytes::Bytes;
    use test_log::test;

    use super::*;
    use crate::services::layers::query_analysis::ParsedDocument;
    use crate::spec;
    use crate::spec::Query;
    use crate::Configuration;

    impl StaticCostCalculator {
        fn rust_planned(
            &self,
            query_plan: &apollo_federation::query_plan::QueryPlan,
        ) -> Result<f64, DemandControlError> {
            let js_planner_node: PlanNode = query_plan.node.as_ref().unwrap().into();
            self.score_plan_node(&js_planner_node)
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
    fn estimated_cost(schema_str: &str, query_str: &str) -> f64 {
        let (schema, query) =
            parse_schema_and_operation(schema_str, query_str, &Default::default());
        StaticCostCalculator::new(schema.supergraph_schema(), Default::default(), 100)
            .estimated(&query.executable, schema.supergraph_schema(), true)
            .unwrap()
    }

    /// Estimate cost of an operation on a plain, non-federated schema.
    fn basic_estimated_cost(schema_str: &str, query_str: &str) -> f64 {
        let schema =
            apollo_compiler::Schema::parse_and_validate(schema_str, "schema.graphqls").unwrap();
        let query = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            query_str,
            "query.graphql",
        )
        .unwrap();
        StaticCostCalculator::new(&schema, Default::default(), 100)
            .estimated(&query, &schema, true)
            .unwrap()
    }

    async fn planned_cost(schema_str: &str, query_str: &str) -> f64 {
        let config: Arc<Configuration> = Arc::new(Default::default());
        let (schema, query) = parse_schema_and_operation(schema_str, query_str, &config);

        let planner =
            QueryPlanner::new(schema.federation_supergraph(), Default::default()).unwrap();

        let query_plan = planner.build_query_plan(&query.executable, None).unwrap();

        let calculator = StaticCostCalculator::new(
            schema.supergraph_schema(),
            Arc::new(
                planner
                    .subgraph_schemas()
                    .iter()
                    .map(|(k, v)| (k.to_string(), Arc::new(v.schema().clone())))
                    .collect(),
            ),
            100,
        );

        calculator.rust_planned(&query_plan).unwrap()
    }

    fn actual_cost(schema_str: &str, query_str: &str, response_bytes: &'static [u8]) -> f64 {
        let (schema, query) =
            parse_schema_and_operation(schema_str, query_str, &Default::default());
        let response = Response::from_bytes("test", Bytes::from(response_bytes)).unwrap();
        StaticCostCalculator::new(schema.supergraph_schema(), Default::default(), 100)
            .actual(&query.executable, &response)
            .unwrap()
    }

    #[test]
    fn query_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 0.0)
    }

    #[test]
    fn mutation_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_mutation.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 10.0)
    }

    #[test]
    fn object_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_object_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 1.0)
    }

    #[test]
    fn interface_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_interface_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 1.0)
    }

    #[test]
    fn union_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_union_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 1.0)
    }

    #[test]
    fn list_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_object_list_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 100.0)
    }

    #[test]
    fn scalar_list_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_scalar_list_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 0.0)
    }

    #[test]
    fn nested_object_lists() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_nested_list_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 10100.0)
    }

    #[test]
    fn input_object_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_input_object_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 2.0)
    }

    #[test]
    fn skip_directive_excludes_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_skipped_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 0.0)
    }

    #[test]
    fn include_directive_excludes_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_excluded_query.graphql");

        assert_eq!(basic_estimated_cost(schema, query), 0.0)
    }

    #[test(tokio::test)]
    async fn federated_query_with_name() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_named_query.graphql");
        let response = include_bytes!("./fixtures/federated_ships_named_response.json");

        assert_eq!(estimated_cost(schema, query), 100.0);
        assert_eq!(actual_cost(schema, query, response), 2.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_requires() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_required_query.graphql");
        let response = include_bytes!("./fixtures/federated_ships_required_response.json");

        assert_eq!(estimated_cost(schema, query), 10200.0);
        assert_eq!(planned_cost(schema, query).await, 10400.0);
        assert_eq!(actual_cost(schema, query, response), 2.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_fragments() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_fragment_query.graphql");
        let response = include_bytes!("./fixtures/federated_ships_fragment_response.json");

        assert_eq!(estimated_cost(schema, query), 300.0);
        assert_eq!(planned_cost(schema, query).await, 400.0);
        assert_eq!(actual_cost(schema, query, response), 6.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_inline_fragments() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_inline_fragment_query.graphql");
        let response = include_bytes!("./fixtures/federated_ships_fragment_response.json");

        assert_eq!(estimated_cost(schema, query), 300.0);
        assert_eq!(planned_cost(schema, query).await, 400.0);
        assert_eq!(actual_cost(schema, query, response), 6.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_defer() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_deferred_query.graphql");
        let response = include_bytes!("./fixtures/federated_ships_deferred_response.json");

        assert_eq!(estimated_cost(schema, query), 10200.0);
        assert_eq!(planned_cost(schema, query).await, 10400.0);
        assert_eq!(actual_cost(schema, query, response), 2.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_adjustable_list_cost() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_deferred_query.graphql");
        let (schema, query) = parse_schema_and_operation(schema, query, &Default::default());

        let conservative_estimate =
            StaticCostCalculator::new(schema.supergraph_schema(), Default::default(), 100)
                .estimated(&query.executable, schema.supergraph_schema(), true)
                .unwrap();
        let narrow_estimate =
            StaticCostCalculator::new(schema.supergraph_schema(), Default::default(), 5)
                .estimated(&query.executable, schema.supergraph_schema(), true)
                .unwrap();

        assert_eq!(conservative_estimate, 10200.0);
        assert_eq!(narrow_estimate, 35.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query = include_str!("./fixtures/custom_cost_query.graphql");

        assert_eq!(estimated_cost(schema, query), 127.0);
        assert_eq!(planned_cost(schema, query).await, 127.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query_with_renamed_directives() {
        let schema = include_str!("./fixtures/custom_cost_schema_with_renamed_directives.graphql");
        let query = include_str!("./fixtures/custom_cost_query.graphql");

        assert_eq!(estimated_cost(schema, query), 127.0);
        assert_eq!(planned_cost(schema, query).await, 127.0);
    }
}
