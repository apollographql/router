use std::sync::Arc;

use ahash::HashMap;
use apollo_compiler::ast;
use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentSpread;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::schema::ExtendedType;
use apollo_federation::query_plan::serializable_document::SerializableDocument;
use serde_json_bytes::Value;

use super::CostBySubgraph;
use super::DemandControlError;
use super::directives::IncludeDirective;
use super::directives::SkipDirective;
use super::schema::DemandControlledSchema;
use super::schema::InputDefinition;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql::Response;
use crate::graphql::ResponseVisitor;
use crate::json_ext::Object;
use crate::plugins::demand_control::cost_calculator::directives::ListSizeDirective;
use crate::query_planner::DeferredNode;
use crate::query_planner::PlanNode;
use crate::query_planner::Primary;
use crate::query_planner::QueryPlan;
use crate::spec::TYPENAME;

pub(crate) struct StaticCostCalculator {
    list_size: u32,
    subgraph_list_sizes: Arc<SubgraphConfiguration<Option<u32>>>,
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
    argument_definition: &InputDefinition,
    schema: &DemandControlledSchema,
    variables: &Object,
) -> Result<f64, DemandControlError> {
    match (argument, argument_definition.ty()) {
        (_, ExtendedType::Interface(_))
        | (_, ExtendedType::Object(_))
        | (_, ExtendedType::Union(_)) => Err(DemandControlError::QueryParseFailure(format!(
            "Argument {} has type {}, but objects, interfaces, and unions are disallowed in this position",
            argument_definition.name(),
            argument_definition.ty().name()
        ))),

        (ast::Value::Object(inner_args), ExtendedType::InputObject(_)) => {
            let mut cost = argument_definition
                .cost_directive()
                .map_or(1.0, |cost| cost.weight());
            for (arg_name, arg_val) in inner_args {
                let arg_def = schema.input_field_definition(argument_definition.ty().name(), arg_name).ok_or_else(|| {
                    DemandControlError::QueryParseFailure(format!(
                        "Argument {} was found in query, but its type ({}) was not found in the schema",
                        arg_name,
                        argument_definition.ty().name()
                    ))
                })?;
                cost += score_argument(arg_val, arg_def, schema, variables)?;
            }
            Ok(cost)
        }
        (ast::Value::List(inner_args), _) => {
            let mut cost = argument_definition
                .cost_directive()
                .map_or(0.0, |cost| cost.weight());
            for arg_val in inner_args {
                cost += score_argument(arg_val, argument_definition, schema, variables)?;
            }
            Ok(cost)
        }
        (ast::Value::Variable(name), _) => {
            // We make a best effort attempt to score the variable, but some of these may not exist in the variables
            // sent on the supergraph request, such as `$representations`.
            if let Some(variable) = variables.get(name.as_str()) {
                score_variable(variable, argument_definition, schema)
            } else {
                Ok(0.0)
            }
        }
        (ast::Value::Null, _) => Ok(0.0),
        _ => Ok(argument_definition
            .cost_directive()
            .map_or(0.0, |cost| cost.weight())),
    }
}

fn score_variable(
    variable: &Value,
    argument_definition: &InputDefinition,
    schema: &DemandControlledSchema,
) -> Result<f64, DemandControlError> {
    match (variable, argument_definition.ty()) {
        (_, ExtendedType::Interface(_))
        | (_, ExtendedType::Object(_))
        | (_, ExtendedType::Union(_)) => Err(DemandControlError::QueryParseFailure(format!(
            "Argument {} has type {}, but objects, interfaces, and unions are disallowed in this position",
            argument_definition.name(),
            argument_definition.ty().name()
        ))),

        (Value::Object(inner_args), ExtendedType::InputObject(_)) => {
            let mut cost = argument_definition
                .cost_directive()
                .map_or(1.0, |cost| cost.weight());
            for (arg_name, arg_val) in inner_args {
                let arg_def = schema.input_field_definition(argument_definition.ty().name(), arg_name.as_str()).ok_or_else(|| {
                    DemandControlError::QueryParseFailure(format!(
                        "Argument {} was found in query, but its type ({}) was not found in the schema",
                        argument_definition.name(),
                        argument_definition.ty().name()
                    ))
                })?;
                cost += score_variable(arg_val, arg_def, schema)?;
            }
            Ok(cost)
        }
        (Value::Array(inner_args), _) => {
            let mut cost = argument_definition
                .cost_directive()
                .map_or(0.0, |cost| cost.weight());
            for arg_val in inner_args {
                cost += score_variable(arg_val, argument_definition, schema)?;
            }
            Ok(cost)
        }
        (Value::Null, _) => Ok(0.0),
        _ => Ok(argument_definition
            .cost_directive()
            .map_or(0.0, |cost| cost.weight())),
    }
}

impl StaticCostCalculator {
    pub(crate) fn new(
        supergraph_schema: Arc<DemandControlledSchema>,
        subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
        subgraph_list_sizes: Arc<SubgraphConfiguration<Option<u32>>>,
        list_size: u32,
    ) -> Self {
        Self {
            list_size,
            subgraph_list_sizes,
            supergraph_schema,
            subgraph_schemas,
        }
    }

    fn subgraph_list_size(&self, subgraph_name: &str) -> Option<u32> {
        *self.subgraph_list_sizes.get(subgraph_name)
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
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        // When we pre-process the schema, __typename isn't included. So, we short-circuit here to avoid failed lookups.
        if field.name == TYPENAME {
            return Ok(0.0);
        }
        if StaticCostCalculator::skipped_by_directives(field) {
            return Ok(0.0);
        }

        let definition = ctx
            .schema
            .output_field_definition(parent_type, &field.name)
            .ok_or_else(|| {
                DemandControlError::QueryParseFailure(format!(
                    "Field {} was found in query, but its type is missing from the schema.",
                    field.name
                ))
            })?;
        let own_list_size_directives: Vec<ListSizeDirective> = definition
            .list_size_directive_entries()
            .iter()
            .map(|entry| {
                ListSizeDirective::new(
                    &entry.directive,
                    field,
                    ctx.variables,
                    entry.parsed_sized_fields.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        let effective_expected_size = own_list_size_directives
            .iter()
            .chain(inherited_list_sizes)
            .filter_map(|dir| dir.expected_size)
            .max();

        let instance_count = if !field.ty().is_list() {
            1
        } else if let Some(value) = list_size_from_upstream {
            // This is a sized field whose length is defined by the `@listSize` directive on the parent field
            value
        } else if let Some(expected_size) = effective_expected_size {
            expected_size
        } else if let Some(subgraph_list_size) = self.subgraph_list_size(subgraph) {
            // Saturate rather than cast: a configured list size above `i32::MAX` must read as
            // very large (expensive), never wrap to a negative value.
            i32::try_from(subgraph_list_size).unwrap_or(i32::MAX)
        } else {
            i32::try_from(self.list_size).unwrap_or(i32::MAX)
        };
        // A list's instance count can never be negative. Guard the invariant at the point of
        // use so no source — client slicing arguments, upstream propagation, or configured
        // defaults — can drive the estimated cost below zero.
        let instance_count = instance_count.max(0);

        // Per-call cost: the cost to resolve this field once (from @cost on the field
        // definition, plus arguments and directives). Counted once regardless of list size.
        let field_call_weight = definition
            .field_cost_directive()
            .map_or(0.0, |cost| cost.weight());

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

        // TODO we should be computing cost of directives applied on the field as well
        // Negative weights can be used to indicate that certain arguments are less expensive to run target resolver.
        // Overall cost of a field cannot be negative, if it is we must round it up to zero.
        // see: https://ibm.github.io/graphql-specs/cost-spec.html#sec-Field-Cost.Example-Negative-Weights
        let field_call_cost = (field_call_weight + arguments_cost).max(0.0);

        let type_instance_cost = if let Some(cost_directive) = definition.type_cost_directive() {
            // negative weights can only be specified on arguments
            cost_directive.weight().max(0.0)
        } else if definition.ty().is_interface()
            || definition.ty().is_object()
            || definition.ty().is_union()
        {
            1.0
        } else {
            0.0
        };

        let child_cost = self.score_selection_set(
            ctx,
            &field.selection_set,
            field.ty().inner_named_type(),
            &own_list_size_directives,
            inherited_list_sizes,
            subgraph,
        )?;

        let mut requirements_cost = 0.0;
        if ctx.should_estimate_requires {
            // If the field is marked with `@requires`, the required selection may not be included
            // in the query's selection. Adding that requirement's cost to the field ensures it's
            // accounted for.
            let requirements = definition.requires_directive().map(|d| &d.fields);
            if let Some(selection_set) = requirements {
                requirements_cost = self.score_selection_set(
                    ctx,
                    selection_set,
                    parent_type,
                    &own_list_size_directives,
                    &[],
                    subgraph,
                )?;
            }
        }

        let cost = field_call_cost
            + (instance_count as f64) * (type_instance_cost + child_cost)
            + requirements_cost;
        tracing::debug!(
            "Overall field {} cost breakdown: field_call_cost {} + ((count) {} * (type_instance_cost {} + child_selection {})) + (requirements) {} = {}",
            field.name,
            field_call_cost,
            instance_count,
            type_instance_cost,
            child_cost,
            requirements_cost,
            cost,
        );

        Ok(cost)
    }

    fn score_fragment_spread(
        &self,
        ctx: &ScoringContext,
        fragment_spread: &FragmentSpread,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
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
            fragment.type_condition(),
            list_size_directives,
            inherited_list_sizes,
            subgraph,
        )
    }

    fn score_inline_fragment(
        &self,
        ctx: &ScoringContext,
        inline_fragment: &InlineFragment,
        parent_type: &NamedType,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        self.score_selection_set(
            ctx,
            &inline_fragment.selection_set,
            inline_fragment
                .type_condition
                .as_ref()
                .unwrap_or(parent_type),
            list_size_directives,
            inherited_list_sizes,
            subgraph,
        )
    }

    fn score_operation(
        &self,
        operation: &Operation,
        ctx: &ScoringContext,
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        let mut cost = if operation.is_mutation() { 10.0 } else { 0.0 };

        let Some(root_type_name) = ctx.schema.root_operation(operation.operation_type) else {
            return Err(DemandControlError::QueryParseFailure(format!(
                "Cannot cost {} operation because the schema does not support this root type",
                operation.operation_type
            )));
        };

        cost += self.score_selection_set(
            ctx,
            &operation.selection_set,
            root_type_name,
            &[],
            &[],
            subgraph,
        )?;

        Ok(cost)
    }

    fn score_selection(
        &self,
        ctx: &ScoringContext,
        selection: &Selection,
        parent_type: &NamedType,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        match selection {
            Selection::Field(f) => {
                // We need two things for scoring this field: (1) the list size to use for
                // instance_count if this field is a list (list_size_from_upstream), and (2) the
                // directive(s) to pass to this field's selection set so nested lists (e.g. "page"
                // under "results") get the right sizing (descended). With repeatable @listSize,
                // multiple directives can descend to the same field, so we collect all of them.
                let size_from_parent = list_size_directives
                    .iter()
                    .filter_map(|dir| dir.size_of(f))
                    .max();
                let size_from_inherited = inherited_list_sizes
                    .iter()
                    .filter_map(|dir| dir.size_of(f))
                    .max();
                let list_size_from_upstream = size_from_parent.or(size_from_inherited);

                let descended: Vec<ListSizeDirective> = list_size_directives
                    .iter()
                    .chain(inherited_list_sizes.iter())
                    .filter_map(|dir| dir.descend(f.name.as_str()))
                    .collect();

                self.score_field(
                    ctx,
                    f,
                    parent_type,
                    list_size_from_upstream,
                    &descended,
                    subgraph,
                )
            }
            Selection::FragmentSpread(s) => self.score_fragment_spread(
                ctx,
                s,
                list_size_directives,
                inherited_list_sizes,
                subgraph,
            ),
            Selection::InlineFragment(i) => self.score_inline_fragment(
                ctx,
                i,
                parent_type,
                list_size_directives,
                inherited_list_sizes,
                subgraph,
            ),
        }
    }

    fn score_selection_set(
        &self,
        ctx: &ScoringContext,
        selection_set: &SelectionSet,
        parent_type_name: &NamedType,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        for selection in selection_set.selections.iter() {
            cost += self.score_selection(
                ctx,
                selection,
                parent_type_name,
                list_size_directives,
                inherited_list_sizes,
                subgraph,
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

    fn score_plan_node(
        &self,
        plan_node: &PlanNode,
        variables: &Object,
    ) -> Result<CostBySubgraph, DemandControlError> {
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
        operation: &SerializableDocument,
        variables: &Object,
    ) -> Result<CostBySubgraph, DemandControlError> {
        tracing::debug!("On subgraph {}, scoring operation: {}", subgraph, operation);

        let schema = self.subgraph_schemas.get(subgraph).ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Query planner did not provide a schema for service {subgraph}"
            ))
        })?;

        let operation = operation
            .as_parsed()
            .map_err(DemandControlError::SubgraphOperationNotInitialized)?;
        let cost = self.estimated(operation, schema, variables, false, subgraph)?;
        Ok(CostBySubgraph::new(subgraph, cost))
    }

    fn max_score_of_nodes(
        &self,
        left: &Option<Box<PlanNode>>,
        right: &Option<Box<PlanNode>>,
        variables: &Object,
    ) -> Result<CostBySubgraph, DemandControlError> {
        match (left, right) {
            (None, None) => Ok(CostBySubgraph::default()),
            (None, Some(right)) => self.score_plan_node(right, variables),
            (Some(left), None) => self.score_plan_node(left, variables),
            (Some(left), Some(right)) => {
                let left_score = self.score_plan_node(left, variables)?;
                let right_score = self.score_plan_node(right, variables)?;
                Ok(CostBySubgraph::maximum(left_score, right_score))
            }
        }
    }

    fn summed_score_of_deferred_nodes(
        &self,
        primary: &Primary,
        deferred: &Vec<DeferredNode>,
        variables: &Object,
    ) -> Result<CostBySubgraph, DemandControlError> {
        let mut score = CostBySubgraph::default();
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
    ) -> Result<CostBySubgraph, DemandControlError> {
        let mut sum = CostBySubgraph::default();
        for node in nodes {
            sum += self.score_plan_node(node, variables)?;
        }
        Ok(sum)
    }

    /// Determine cost for a single-subgraph operation.
    pub(crate) fn estimated(
        &self,
        query: &ExecutableDocument,
        schema: &DemandControlledSchema,
        variables: &Object,
        should_estimate_requires: bool,
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        let ctx = ScoringContext {
            schema,
            query,
            variables,
            should_estimate_requires,
        };
        if let Some(op) = &query.operations.anonymous {
            cost += self.score_operation(op, &ctx, subgraph)?;
        }
        for (_name, op) in query.operations.named.iter() {
            cost += self.score_operation(op, &ctx, subgraph)?;
        }
        Ok(cost)
    }

    /// Determine cost for an operation which may span multiple subgraphs.
    pub(crate) fn planned(
        &self,
        query_plan: &QueryPlan,
        variables: &Object,
    ) -> Result<CostBySubgraph, DemandControlError> {
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

    fn score_response_field(
        &mut self,
        request: &ExecutableDocument,
        variables: &Object,
        parent_ty: &NamedType,
        field: &Field,
        value: &Value,
        include_argument_score: bool,
    ) {
        // When we pre-process the schema, __typename isn't included. So, we short-circuit here to avoid failed lookups.
        if field.name == TYPENAME {
            return;
        }

        let definition = self.schema.output_field_definition(parent_ty, &field.name);

        // We need to have a field definition for later processing, unless the query is an
        // `_entities` query. If the field should be there and isn't, return now.
        let is_entities_query = parent_ty == "Query" && field.name == "_entities";
        if definition.is_none() && !is_entities_query {
            tracing::debug!(
                "Failed to get schema definition for field {}.{}. The resulting response cost will be a partial result.",
                parent_ty,
                field.name,
            );
            return;
        }

        let mut response_field_cost: f64 = 0.0;

        // Per-call cost: field @cost + arguments, counted once per field resolution
        if include_argument_score {
            response_field_cost += definition
                .and_then(|d| d.field_cost_directive())
                .map_or(0.0, |cost| cost.weight());

            if let Some(definition) = definition {
                for argument in &field.arguments {
                    if let Some(argument_definition) = definition.argument_by_name(&argument.name) {
                        if let Ok(score) = score_argument(
                            &argument.value,
                            argument_definition,
                            self.schema,
                            variables,
                        ) {
                            response_field_cost += score;
                        }
                    } else {
                        tracing::debug!(
                            "Failed to get schema definition for argument {}.{}({}:). The resulting response cost will be a partial result.",
                            parent_ty,
                            field.name,
                            argument.name,
                        )
                    }
                }
            }
        }

        // per-instance type cost + child selections
        // NOTE: this is an upper bound as we might not know the actual returned object type
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                response_field_cost += definition
                    .and_then(|d| d.type_cost_directive())
                    .map_or(0.0, |cost| cost.weight());
            }
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(request, variables, parent_ty, field, item);
                }
            }
            Value::Object(children) => {
                response_field_cost += definition
                    .and_then(|d| d.type_cost_directive())
                    .map_or(1.0, |cost| cost.weight());
                self.visit_selections(request, variables, &field.selection_set, children);
            }
        }

        // Overall cost of a resolved field cannot be negative, if it is we must round it up to zero.
        self.cost += response_field_cost.max(0.0);
    }
}

impl ResponseVisitor for ResponseCostCalculator<'_> {
    fn visit_field(
        &mut self,
        request: &ExecutableDocument,
        variables: &Object,
        parent_ty: &NamedType,
        field: &Field,
        value: &Value,
    ) {
        self.score_response_field(request, variables, parent_ty, field, value, true);
    }

    fn visit_list_item(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        variables: &Object,
        parent_ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
    ) {
        self.score_response_field(request, variables, parent_ty, field, value, false);
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
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::assert_snapshot_subscriber;
    use crate::compute_job::ComputeJobType;
    use crate::plugins::authorization::CacheKeyMetadata;
    use crate::query_planner::QueryPlannerService;
    use crate::services::QueryPlannerContent;
    use crate::services::QueryPlannerRequest;
    use crate::services::layers::query_analysis::ParsedDocument;
    use crate::services::query_planner::PlanOptions;
    use crate::spec;
    use crate::spec::Query;

    impl StaticCostCalculator {
        fn rust_planned(
            &self,
            query_plan: &apollo_federation::query_plan::QueryPlan,
            variables: &Object,
        ) -> Result<f64, DemandControlError> {
            let js_planner_node: PlanNode = query_plan.node.as_ref().unwrap().into();
            Ok(self.score_plan_node(&js_planner_node, variables)?.total())
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
    ///
    /// Does not consider cost-by-subgraph or make use of `StaticCostCalculator::subgraph_list_size`,
    /// which are only available when estimating the cost against a query plan.
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
        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            100,
        );

        calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &variables,
                true,
                "",
            )
            .unwrap()
    }

    /// Estimate cost of an operation on a plain, non-federated schema.
    ///
    /// Does not consider cost-by-subgraph or make use of `StaticCostCalculator::subgraph_list_size`,
    /// which are only available when estimating the cost against a query plan.
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
        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            100,
        );

        calculator
            .estimated(&query, &calculator.supergraph_schema, &variables, true, "")
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

        let mut planner = QueryPlannerService::new(schema.into(), config.clone())
            .await
            .unwrap();

        let ctx = Context::new();
        ctx.extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(query.clone()));

        let planner_res = planner
            .call(QueryPlannerRequest::new(
                query_str.to_string(),
                None,
                query,
                CacheKeyMetadata::default(),
                PlanOptions::default(),
                ComputeJobType::QueryPlanning,
                variables.clone(),
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
                DemandControlledSchema::new(subgraph_schema.schema.clone()).unwrap();
            demand_controlled_subgraph_schemas
                .insert(subgraph_name.to_string(), demand_controlled_subgraph_schema);
        }

        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Arc::new(demand_controlled_subgraph_schemas),
            Default::default(),
            100,
        );

        calculator.planned(&query_plan, &variables).unwrap().total()
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
            Default::default(),
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
        let response = Response::from_bytes(Bytes::from(response_bytes)).unwrap();
        let schema =
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap();
        StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            100,
        )
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
        let response = Response::from_bytes(Bytes::from(response_bytes)).unwrap();

        let schema = DemandControlledSchema::new(Arc::new(schema)).unwrap();
        StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            100,
        )
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
        // NB: does not consider cost-by-subgraph or make use of `StaticCostCalculator::subgraph_list_size`,
        // which are only available when estimating the cost against a query plan.
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_deferred_query.graphql");
        let (schema, query) = parse_schema_and_operation(schema, query, &Default::default());
        let schema = Arc::new(
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap(),
        );

        let calculator =
            StaticCostCalculator::new(schema.clone(), Default::default(), Default::default(), 100);
        let conservative_estimate = calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &Default::default(),
                true,
                "",
            )
            .unwrap();

        let calculator =
            StaticCostCalculator::new(schema.clone(), Default::default(), Default::default(), 5);
        let narrow_estimate = calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &Default::default(),
                true,
                "",
            )
            .unwrap();

        assert_eq!(conservative_estimate, 10200.0);
        assert_eq!(narrow_estimate, 35.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_typenames() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_typename_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_typename_response.json");

        async {
            assert_eq!(actual_cost(schema, query, variables, response), 2.0);
        }
        // This was previously logging a warning for every __typename in the response. At the time of writing,
        // this should not produce logs. Generally, it should not produce undue noise for valid requests.
        .with_subscriber(assert_snapshot_subscriber!())
        .await
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

    #[test]
    fn negative_slicing_argument_is_clamped_and_never_reduces_cost() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query =
            include_str!("./fixtures/custom_cost_query_with_variable_slicing_argument.graphql");

        // A negative client-supplied slicing value must be treated as zero, not as negative
        // cost that would deflate the estimate below what the operation actually costs.
        let negative = estimated_cost(
            schema,
            query,
            r#"{"costlyInput": {"somethingWithCost": 10}, "fieldCountVar": -10}"#,
        );
        let zero = estimated_cost(
            schema,
            query,
            r#"{"costlyInput": {"somethingWithCost": 10}, "fieldCountVar": 0}"#,
        );

        assert!(
            negative >= 0.0,
            "estimated cost must never be negative, got {negative}"
        );
        assert_eq!(
            negative, zero,
            "a negative slicing value must be clamped to zero"
        );
    }

    #[test]
    fn oversized_configured_list_size_does_not_wrap_negative() {
        // A configured default list size above `i32::MAX` must saturate to a large (expensive)
        // value, never wrap to a negative one that would invert the estimate.
        let schema_str = include_str!("./fixtures/basic_schema.graphql");
        let query_str = include_str!("./fixtures/basic_object_list_query.graphql");
        let schema =
            apollo_compiler::Schema::parse_and_validate(schema_str, "schema.graphqls").unwrap();
        let query = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            query_str,
            "query.graphql",
        )
        .unwrap();
        let schema = DemandControlledSchema::new(Arc::new(schema)).unwrap();
        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            u32::MAX,
        );

        let cost = calculator
            .estimated(
                &query,
                &calculator.supergraph_schema,
                &Default::default(),
                true,
                "",
            )
            .unwrap();

        assert!(
            cost >= 0.0,
            "estimated cost must never be negative for an oversized configured list size, got {cost}"
        );
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

    #[test(tokio::test)]
    async fn subscription_request() {
        let schema = include_str!("./fixtures/subscription_schema.graphql");
        let query = include_str!("./fixtures/subscription_query.graphql");
        let variables = "{}";

        assert_eq!(estimated_cost(schema, query, variables), 1.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 1.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 1.0);
    }

    /// Tests to ensure Vec-based implementation (for supporting multiple @listSize directives)
    /// maintains backward compatibility with existing single-directive behavior
    mod backward_compatibility_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/custom_cost_schema.graphql");

        #[rstest::rstest]
        #[case::no_directive("query { enumWithCost }", "{}", 15.0)]
        #[case::single_slicing_argument_with_array(
            r#"query { itemsByIds(ids: ["a", "b"]) { id } }"#,
            "{}",
            2.0
        )]
        #[case::slicing_argument_with_variable(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": ["x", "y", "z"]}"#,
            3.0
        )]
        #[case::nested_sized_fields(
            r#"query { containerWithNestedList(first: 5) { page { id } } }"#,
            "{}",
            6.0
        )]
        #[case::assumed_size_fallback(
            r#"query Q($ids: [ID!]) { itemsByIdsWithAssumedSize(ids: $ids) { id } }"#,
            r#"{"ids": null}"#,
            50.0
        )]
        #[case::sized_fields_propagate_to_nested_lists(
            r#"query { fieldWithDynamicListSize { items { id } } }"#,
            "{}",
            11.0
        )]
        fn vec_based_implementation_maintains_backward_compatibility(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }
    }

    /// Tests for array-based slicing arguments in @listSize directive
    mod array_slicing_argument_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/custom_cost_schema.graphql");

        #[rstest::rstest]
        #[case::inline_array_of_3(
            r#"query { itemsByIds(ids: ["a", "b", "c"]) { id } }"#,
            "{}",
            3.0
        )]
        #[case::empty_inline_array(r#"query { itemsByIds(ids: []) { id } }"#, "{}", 0.0)]
        #[case::variable_array_of_5(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": ["a", "b", "c", "d", "e"]}"#,
            5.0
        )]
        #[case::variable_empty_array(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": []}"#,
            0.0
        )]
        fn array_length_determines_list_size(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        #[rstest::rstest]
        #[case::null_variable(r#"{"ids": null}"#)]
        #[case::missing_variable("{}")]
        fn null_or_missing_array_falls_back_to_assumed_size(#[case] variables: &str) {
            let query = r#"query Q($ids: [ID!]) { itemsByIdsWithAssumedSize(ids: $ids) { id } }"#;
            assert_eq!(estimated_cost(SCHEMA, query, variables), 50.0);
        }
    }

    /// Tests for nested input path resolution in @listSize slicingArguments
    ///
    /// Note: Expected costs include the cost of input objects (1 per nested object).
    /// For inline objects: SearchInput (1) + PaginationInput (1) = 2 base cost
    /// For variables: input objects are costed based on their nesting level
    mod nested_input_path_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/custom_cost_schema.graphql");

        // Input object costs:
        // - SearchInput: 1
        // - PaginationInput: 1
        // Total input cost for search queries: 2

        #[rstest::rstest]
        #[case::inline_nested_first_10(
            r#"query { search(input: {pagination: {first: 10}}) { id } }"#,
            "{}",
            12.0
        )]
        #[case::inline_nested_first_5(
            r#"query { search(input: {pagination: {first: 5}, query: "test"}) { id } }"#,
            "{}",
            7.0
        )]
        #[case::variable_nested_object(
            r#"query Q($input: SearchInput!) { search(input: $input) { id } }"#,
            r#"{"input": {"pagination": {"first": 7}, "query": "test"}}"#,
            9.0
        )]
        #[case::variable_nested_first_only(
            r#"query Q($input: SearchInput!) { search(input: $input) { id } }"#,
            r#"{"input": {"pagination": {"first": 3}}}"#,
            5.0
        )]
        fn nested_path_determines_list_size(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        // Input object costs for searchWithAssumedSize:
        // - SearchInput: 1
        // - PaginationInput: 1 (if present)
        // When path not found, falls back to assumedSize (25)

        #[rstest::rstest]
        #[case::missing_nested_value(r#"{"input": {"pagination": {}}}"#, 27.0)]
        #[case::missing_pagination(r#"{"input": {}}"#, 26.0)]
        #[case::null_input(r#"{"input": null}"#, 25.0)]
        fn missing_nested_path_falls_back_to_assumed_size(
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            let query =
                r#"query Q($input: SearchInput) { searchWithAssumedSize(input: $input) { id } }"#;
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        // DeeplyNestedInput has 3 levels: DeeplyNestedInput(1) + NestedLevel1Input(1) + NestedLevel2Input(1) = 3

        #[test]
        fn deeply_nested_path_inline() {
            let query = r#"query { deeplyNested(input: {level1: {level2: {count: 15}}}) { id } }"#;
            assert_eq!(estimated_cost(SCHEMA, query, "{}"), 18.0);
        }

        #[test]
        fn deeply_nested_path_variable() {
            let query =
                r#"query Q($input: DeeplyNestedInput!) { deeplyNested(input: $input) { id } }"#;
            let variables = r#"{"input": {"level1": {"level2": {"count": 12}}}}"#;
            assert_eq!(estimated_cost(SCHEMA, query, variables), 15.0);
        }

        #[test]
        fn inline_nested_object_with_other_fields() {
            // Ensure other fields in the nested object don't affect the size resolution
            let query = r#"query { search(input: {pagination: {first: 8, after: "cursor"}, query: "search term"}) { id } }"#;
            assert_eq!(estimated_cost(SCHEMA, query, "{}"), 10.0);
        }
    }

    /// Nested sizedFields in @listSize (e.g. "results { page }")
    mod nested_sized_fields_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/custom_cost_schema.graphql");

        #[rstest::rstest]
        #[case::simple_sized_fields_on_nested_type(
            r#"query { containerWithNestedList(first: 5) { page { id } metadata } }"#,
            "{}",
            6.0
        )]
        #[case::nested_sized_fields_two_levels(
            r#"query { deepContainerWithNestedList(first: 7) { results { page { id } } } }"#,
            "{}",
            9.0
        )]
        #[case::nested_sized_fields_with_variable(
            r#"query Q($n: Int!) { deepContainerWithNestedList(first: $n) { results { page { id } } } }"#,
            r#"{"n": 3}"#,
            5.0
        )]
        #[case::nested_sized_fields_with_default_value(
            r#"query { deepContainerWithNestedList { results { page { id } } } }"#,
            "{}",
            12.0
        )]
        #[case::nested_sized_fields_not_selected(
            r#"query { deepContainerWithNestedList(first: 100) { total } }"#,
            "{}",
            1.0
        )]
        #[case::intermediate_container_without_sized_field(
            r#"query { deepContainerWithNestedList(first: 100) { results { metadata } } }"#,
            "{}",
            2.0
        )]
        #[case::mixed_sized_fields_single_and_nested(
            r#"query {
                deepContainerWithMixedSizedFields(first: 5) {
                    page { id }
                    results { page { id } }
                }
            }"#,
            "{}",
            12.0
        )]
        fn nested_sized_fields_cases(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        /// Schema load fails when a sizedFields path has more than one leaf (one-leaf-per-path rule).
        #[test]
        fn multiple_leaves_in_one_path_fails_at_schema_load() {
            use std::sync::Arc;

            use crate::plugins::demand_control::cost_calculator::schema::DemandControlledSchema;
            use crate::spec;

            // Schema with sizedFields: ["results { page metadata }"] - two leaves in one path
            let schema_str = include_str!("./fixtures/custom_cost_schema.graphql").replace(
                r#"sizedFields: ["results { page }"]"#,
                r#"sizedFields: ["results { page metadata }"]"#,
            );
            // ResultContainer has page: [A] and metadata: String. So "results { page metadata }"
            // has two top-level selections with no sub-selections (page is a leaf, metadata is a leaf).
            let schema = spec::Schema::parse(&schema_str, &Default::default()).unwrap();
            let result = DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone()));

            match &result {
                Err(e) => assert!(
                    e.to_string().contains("at most one list field per path"),
                    "expected error about one list field per path, got: {}",
                    e
                ),
                Ok(_) => {
                    panic!("expected schema load to fail for multiple list fields in one path")
                }
            }
        }
    }

    /// Tests for `@cost` on types: objects, interface implementations, and union members.
    /// For interfaces, the max `@cost` across all implementing object types is used.
    /// For unions, the max `@cost` across all member types is used.
    mod type_cost_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/type_cost_schema.graphql");

        #[test]
        fn object_with_cost_directive() {
            // CostlyObject has @cost(weight: 7)
            let cost = estimated_cost(SCHEMA, "query { objectWithCost { id } }", "{}");
            assert_eq!(cost, 7.0);
        }

        #[test]
        fn object_without_cost_directive_defaults_to_one() {
            // PlainObject has no @cost, defaults to 1.0
            let cost = estimated_cost(SCHEMA, "query { objectNoCost { id } }", "{}");
            assert_eq!(cost, 1.0);
        }

        #[test]
        fn interface_uses_max_cost_from_implementations() {
            // Animal: Cat @cost(3), Dog @cost(5) → max is 5
            let cost = estimated_cost(SCHEMA, "query { cheapAnimal { name } }", "{}");
            assert_eq!(cost, 5.0);
        }

        #[test]
        fn interface_without_cost_on_implementations_defaults_to_one() {
            // NoCostAnimal: Fish (no @cost), Bird (no @cost) → default 1.0
            let cost = estimated_cost(SCHEMA, "query { noCostInterface { name } }", "{}");
            assert_eq!(cost, 1.0);
        }

        #[test]
        fn union_uses_max_cost_from_members() {
            // SearchResult: Article @cost(2), Video @cost(8) → max is 8
            let cost = estimated_cost(
                SCHEMA,
                "query { searchResult { ... on Article { title } ... on Video { url } } }",
                "{}",
            );
            assert_eq!(cost, 8.0);
        }

        #[test]
        fn union_with_mixed_cost_uses_max() {
            // MixedUnion: CostlyMember @cost(10), PlainMember (no @cost) → max is 10
            let cost = estimated_cost(
                SCHEMA,
                "query { mixedUnion { ... on CostlyMember { value } ... on PlainMember { value } } }",
                "{}",
            );
            assert_eq!(cost, 10.0);
        }

        #[test]
        fn union_without_cost_on_members_defaults_to_one() {
            // NoCostUnion: PlainA (no @cost), PlainB (no @cost) → default 1.0
            let cost = estimated_cost(
                SCHEMA,
                "query { noCostUnion { ... on PlainA { id } ... on PlainB { id } } }",
                "{}",
            );
            assert_eq!(cost, 1.0);
        }
    }

    /// Tests for negative cost clamping per the cost spec.
    /// Field cost must never be negative — if `@cost` weights (especially negative argument
    /// weights) produce a negative total, it is clamped to zero.
    /// See: https://ibm.github.io/graphql-specs/cost-spec.html#sec-Field-Cost.Example-Negative-Weights
    mod negative_cost_tests {
        use super::actual_cost;
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/negative_cost_schema.graphql");

        #[rstest::rstest]
        #[case::negative_arg_clamps_to_zero(
            "query { fieldWithNegativeArgCost(discount: 1) }",
            "{}",
            0.0  // field(5) + arg(-20) = -15 → clamped to 0
        )]
        #[case::exact_zero_stays_zero(
            "query { fieldWithExactZeroCost(offset: 1) }",
            "{}",
            0.0  // field(10) + arg(-10) = 0
        )]
        #[case::positive_result_not_clamped(
            "query { fieldWithPartialNegativeArg(offset: 1) }",
            "{}",
            7.0  // field(10) + arg(-3) = 7
        )]
        #[case::mixed_args_net_negative_clamps_to_zero(
            "query { fieldWithMixedArgs(a: 1, b: 1) }",
            "{}",
            0.0  // field(10) + arg_a(-30) + arg_b(5) = -15 → clamped to 0
        )]
        #[case::object_field_with_negative_arg_clamps_to_zero(
            "query { objectWithNegativeArgCost(discount: 1) { id } }",
            "{}",
            1.0  // field_call = max(0, 5 + (-50)) = 0, type_instance = 1 (NegCostObject) → 0 + 1*(1+0) = 1
        )]
        #[case::child_cost_preserved_when_field_cost_clamps(
            "query { nestedObjectWithNegativeArgCost(discount: 1) { inner { value } } }",
            "{}",
            2.0  // field_call = max(0, 5 + (-50)) = 0, type_instance = 1, child(inner) = 1 → 0 + 1*(1+1) = 2
        )]
        fn estimated_cost_clamps_negative_to_zero(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        #[rstest::rstest]
        #[case::scalar_response_clamps_to_zero(
            "query { fieldWithNegativeArgCost(discount: 1) }",
            "{}",
            br#"{"data": {"fieldWithNegativeArgCost": "hello"}}"#,
            0.0  // scalar weight(0) + arg(-20) = -20 → clamped to 0
        )]
        #[case::object_response_clamps_to_zero(
            "query { objectWithNegativeArgCost(discount: 1) { id } }",
            "{}",
            br#"{"data": {"objectWithNegativeArgCost": {"id": "1"}}}"#,
            0.0  // object weight(5) + arg(-50) = -45 → clamped to 0
        )]
        #[case::positive_response_cost_not_clamped(
            "query { fieldWithPartialNegativeArg(offset: 1) }",
            "{}",
            br#"{"data": {"fieldWithPartialNegativeArg": "hello"}}"#,
            7.0  // scalar with @cost(weight: 10) + arg(-3) = 7, not clamped
        )]
        fn actual_cost_clamps_negative_to_zero(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] response_bytes: &'static [u8],
            #[case] expected_cost: f64,
        ) {
            assert_eq!(
                actual_cost(SCHEMA, query, variables, response_bytes),
                expected_cost
            );
        }
    }
}
