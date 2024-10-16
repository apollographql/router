use std::sync::Arc;

use ahash::HashMap;
use apollo_compiler::ast;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentSpread;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Node;
use serde_json_bytes::Value;

use super::directives::IncludeDirective;
use super::directives::SkipDirective;
use super::schema::DemandControlledSchema;
use super::DemandControlError;
use crate::graphql::Response;
use crate::graphql::ResponseVisitor;
use crate::json_ext::Object;
use crate::json_ext::ValueExt;
use crate::plugins::demand_control::cost_calculator::directives::CostDirective;
use crate::plugins::demand_control::cost_calculator::directives::ListSizeDirective;
use crate::query_planner::fetch::SubgraphOperation;
use crate::query_planner::DeferredNode;
use crate::query_planner::PlanNode;
use crate::query_planner::Primary;
use crate::query_planner::QueryPlan;

pub(crate) struct StaticCostCalculator {
    list_size: u32,
    supergraph_schema: Arc<DemandControlledSchema>,
    subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
}

struct ScoringContext<'a> {
    schema: &'a DemandControlledSchema,
    query: &'a ExecutableDocument,
    variables: &'a Object,
    should_estimate_requires: bool,
}

fn score_argument(
    argument: &apollo_compiler::ast::Value,
    argument_definition: &Node<InputValueDefinition>,
    schema: &DemandControlledSchema,
    variables: &Object,
) -> Result<f64, DemandControlError> {
    let cost_directive =
        CostDirective::from_argument(schema.directive_name_map(), argument_definition);
    let ty = schema
        .types
        .get(argument_definition.ty.inner_named_type())
        .ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Argument {} was found in query, but its type ({}) was not found in the schema",
                argument_definition.name,
                argument_definition.ty.inner_named_type()
            ))
        })?;

    match (argument, ty) {
        (_, ExtendedType::Interface(_))
        | (_, ExtendedType::Object(_))
        | (_, ExtendedType::Union(_)) => Err(DemandControlError::QueryParseFailure(
            format!(
                "Argument {} has type {}, but objects, interfaces, and unions are disallowed in this position",
                argument_definition.name,
                argument_definition.ty.inner_named_type()
            )
        )),

        (ast::Value::Object(inner_args), ExtendedType::InputObject(inner_arg_defs)) => {
            let mut cost = cost_directive.map_or(1.0, |cost| cost.weight());
            for (arg_name, arg_val) in inner_args {
                let arg_def = inner_arg_defs.fields.get(arg_name).ok_or_else(|| {
                    DemandControlError::QueryParseFailure(format!(
                        "Argument {} was found in query, but its type ({}) was not found in the schema",
                        argument_definition.name,
                        argument_definition.ty.inner_named_type()
                    ))
                })?;
                cost += score_argument(arg_val, arg_def, schema, variables,)?;
            }
            Ok(cost)
        }
        (ast::Value::List(inner_args), _) => {
            let mut cost = cost_directive.map_or(0.0, |cost| cost.weight());
            for arg_val in inner_args {
                cost += score_argument(arg_val, argument_definition, schema, variables)?;
            }
            Ok(cost)
        }
        (ast::Value::Variable(name), _) => {
            // We make a best effort attempt to score the variable, but some of these may not exist in the variables
            // sent on the supergraph request, such as `$representations`.
            if let Some(variable) = variables.get(name.as_str()) {
                score_argument(&variable.to_ast(), argument_definition, schema, variables)
            } else {
                Ok(0.0)
            }
        }
        (ast::Value::Null, _) => Ok(0.0),
        _ => Ok(cost_directive.map_or(0.0, |cost| cost.weight()))
    }
}

impl StaticCostCalculator {
    pub(crate) fn new(
        supergraph_schema: Arc<DemandControlledSchema>,
        subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
        list_size: u32,
    ) -> Self {
        Self {
            list_size,
            supergraph_schema,
            subgraph_schemas,
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
        ctx: &ScoringContext,
        field: &Field,
        parent_type: &NamedType,
        list_size_from_upstream: Option<i32>,
    ) -> Result<f64, DemandControlError> {
        if StaticCostCalculator::skipped_by_directives(field) {
            return Ok(0.0);
        }

        // We need to look up the `FieldDefinition` from the supergraph schema instead of using `field.definition`
        // because `field.definition` was generated from the API schema, which strips off the directives we need.
        let definition = ctx.schema.type_field(parent_type, &field.name)?;
        let ty = field.inner_type_def(ctx.schema).ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Field {} was found in query, but its type is missing from the schema.",
                field.name
            ))
        })?;

        let list_size_directive = match ctx
            .schema
            .type_field_list_size_directive(parent_type, &field.name)
        {
            Some(dir) => dir.with_field_and_variables(field, ctx.variables).map(Some),
            None => Ok(None),
        }?;
        let instance_count = if !field.ty().is_list() {
            1
        } else if let Some(value) = list_size_from_upstream {
            // This is a sized field whose length is defined by the `@listSize` directive on the parent field
            value
        } else if let Some(expected_size) = list_size_directive
            .as_ref()
            .and_then(|dir| dir.expected_size)
        {
            expected_size
        } else {
            self.list_size as i32
        };

        // Determine the cost for this particular field. Scalars are free, non-scalars are not.
        // For fields with selections, add in the cost of the selections as well.
        let mut type_cost = if let Some(cost_directive) = ctx
            .schema
            .type_field_cost_directive(parent_type, &field.name)
        {
            cost_directive.weight()
        } else if ty.is_interface() || ty.is_object() || ty.is_union() {
            1.0
        } else {
            0.0
        };
        type_cost += self.score_selection_set(
            ctx,
            &field.selection_set,
            field.ty().inner_named_type(),
            list_size_directive.as_ref(),
        )?;

        let mut arguments_cost = 0.0;
        for argument in &field.arguments {
            let argument_definition =
                definition.argument_by_name(&argument.name).ok_or_else(|| {
                    DemandControlError::QueryParseFailure(format!(
                        "Argument {} of field {} is missing a definition in the schema",
                        argument.name, field.name
                    ))
                })?;
            arguments_cost += score_argument(
                &argument.value,
                argument_definition,
                ctx.schema,
                ctx.variables,
            )?;
        }

        let mut requirements_cost = 0.0;
        if ctx.should_estimate_requires {
            // If the field is marked with `@requires`, the required selection may not be included
            // in the query's selection. Adding that requirement's cost to the field ensures it's
            // accounted for.
            let requirements = ctx
                .schema
                .type_field_requires_directive(parent_type, &field.name)
                .map(|d| &d.fields);
            if let Some(selection_set) = requirements {
                requirements_cost = self.score_selection_set(
                    ctx,
                    selection_set,
                    parent_type,
                    list_size_directive.as_ref(),
                )?;
            }
        }

        let cost = (instance_count as f64) * type_cost + arguments_cost + requirements_cost;
        tracing::debug!(
            "Field {} cost breakdown: (count) {} * (type cost) {} + (arguments) {} + (requirements) {} = {}",
            field.name,
            instance_count,
            type_cost,
            arguments_cost,
            requirements_cost,
            cost
        );

        Ok(cost)
    }

    fn score_fragment_spread(
        &self,
        ctx: &ScoringContext,
        fragment_spread: &FragmentSpread,
        parent_type: &NamedType,
        list_size_directive: Option<&ListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        let fragment = fragment_spread.fragment_def(ctx.query).ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Parsed operation did not have a definition for fragment {}",
                fragment_spread.fragment_name
            ))
        })?;
        self.score_selection_set(
            ctx,
            &fragment.selection_set,
            parent_type,
            list_size_directive,
        )
    }

    fn score_inline_fragment(
        &self,
        ctx: &ScoringContext,
        inline_fragment: &InlineFragment,
        parent_type: &NamedType,
        list_size_directive: Option<&ListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        self.score_selection_set(
            ctx,
            &inline_fragment.selection_set,
            parent_type,
            list_size_directive,
        )
    }

    fn score_operation(
        &self,
        operation: &Operation,
        ctx: &ScoringContext,
    ) -> Result<f64, DemandControlError> {
        let mut cost = if operation.is_mutation() { 10.0 } else { 0.0 };

        let Some(root_type_name) = ctx.schema.root_operation(operation.operation_type) else {
            return Err(DemandControlError::QueryParseFailure(format!(
                "Cannot cost {} operation because the schema does not support this root type",
                operation.operation_type
            )));
        };

        cost += self.score_selection_set(ctx, &operation.selection_set, root_type_name, None)?;

        Ok(cost)
    }

    fn score_selection(
        &self,
        ctx: &ScoringContext,
        selection: &Selection,
        parent_type: &NamedType,
        list_size_directive: Option<&ListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        match selection {
            Selection::Field(f) => self.score_field(
                ctx,
                f,
                parent_type,
                list_size_directive.and_then(|dir| dir.size_of(f)),
            ),
            Selection::FragmentSpread(s) => {
                self.score_fragment_spread(ctx, s, parent_type, list_size_directive)
            }
            Selection::InlineFragment(i) => self.score_inline_fragment(
                ctx,
                i,
                i.type_condition.as_ref().unwrap_or(parent_type),
                list_size_directive,
            ),
        }
    }

    fn score_selection_set(
        &self,
        ctx: &ScoringContext,
        selection_set: &SelectionSet,
        parent_type_name: &NamedType,
        list_size_directive: Option<&ListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        for selection in selection_set.selections.iter() {
            cost += self.score_selection(ctx, selection, parent_type_name, list_size_directive)?;
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
        should_estimate_requires: bool,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        let ctx = ScoringContext {
            schema,
            query,
            variables,
            should_estimate_requires,
        };
        if let Some(op) = &query.operations.anonymous {
            cost += self.score_operation(op, &ctx)?;
        }
        for (_name, op) in query.operations.named.iter() {
            cost += self.score_operation(op, &ctx)?;
        }
        Ok(cost)
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
        let mut visitor = ResponseCostCalculator::new(&self.supergraph_schema);
        visitor.visit(request, response, variables);
        Ok(visitor.cost)
    }
}

pub(crate) struct ResponseCostCalculator<'a> {
    pub(crate) cost: f64,
    schema: &'a DemandControlledSchema,
}

impl<'schema> ResponseCostCalculator<'schema> {
    pub(crate) fn new(schema: &'schema DemandControlledSchema) -> Self {
        Self { cost: 0.0, schema }
    }
}

impl<'schema> ResponseVisitor for ResponseCostCalculator<'schema> {
    fn visit_field(
        &mut self,
        request: &ExecutableDocument,
        variables: &Object,
        parent_ty: &NamedType,
        field: &Field,
        value: &Value,
    ) {
        self.visit_list_item(request, variables, parent_ty, field, value);

        let definition = self.schema.type_field(parent_ty, &field.name);
        for argument in &field.arguments {
            if let Ok(Some(argument_definition)) = definition
                .as_ref()
                .map(|def| def.argument_by_name(&argument.name))
            {
                if let Ok(score) =
                    score_argument(&argument.value, argument_definition, self.schema, variables)
                {
                    self.cost += score;
                }
            } else {
                tracing::warn!(
                    "Failed to get schema definition for argument {} of field {}. The resulting actual cost will be a partial result.",
                    argument.name,
                    field.name
                )
            }
        }
    }

    fn visit_list_item(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        variables: &Object,
        parent_ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
    ) {
        let cost_directive = self
            .schema
            .type_field_cost_directive(parent_ty, &field.name);

        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                self.cost += cost_directive.map_or(0.0, |cost| cost.weight());
            }
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(request, variables, parent_ty, field, item);
                }
            }
            Value::Object(children) => {
                self.cost += cost_directive.map_or(1.0, |cost| cost.weight());
                self.visit_selections(request, variables, &field.selection_set, children);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ahash::HashMapExt;
    use apollo_federation::query_plan::query_planner::QueryPlanner;
    use bytes::Bytes;
    use test_log::test;
    use tower::Service;

    use super::*;
    use crate::introspection::IntrospectionCache;
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
            .with_lock(|mut lock| lock.insert::<ParsedDocument>(query));

        let planner_res = planner
            .call(QueryPlannerRequest::new(query_str.to_string(), None, ctx))
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
    async fn custom_cost_query() {
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
}
