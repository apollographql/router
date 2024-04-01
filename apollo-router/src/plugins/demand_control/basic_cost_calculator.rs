use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentSpread;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;

use super::directives::CostDirective;
use super::directives::IncludeDirective;
use super::directives::RequiresDirective;
use super::directives::SkipDirective;
use super::schema_aware_response::SchemaAwareResponse;
use super::CostCalculator;
use super::DemandControlError;
use crate::graphql::Response;
use crate::plugins::demand_control::schema_aware_response::TypedValue;
use crate::query_planner::DeferredNode;
use crate::query_planner::PlanNode;
use crate::query_planner::Primary;
use crate::query_planner::QueryPlan;

pub(crate) struct BasicCostCalculator {}

impl BasicCostCalculator {
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
        field: &Field,
        parent_type_name: Option<&NamedType>,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        if BasicCostCalculator::skipped_by_directives(field) {
            return Ok(0.0);
        }

        let ty = field
            .inner_type_def(schema)
            .ok_or(DemandControlError::QueryParseFailure(format!(
                "Field {} was found in query, but its type is missing from the schema.",
                field.name
            )))?;

        // Determine how many instances we're scoring. If there's no user-provided
        // information, assume lists have 100 items.
        let instance_count = if field.ty().is_list() { 100.0 } else { 1.0 };

        // Determine the cost for this particular field. Scalars are free, non-scalars are not.
        // For fields with selections, add in the cost of the selections as well.
        let mut type_cost = if ty.is_interface() || ty.is_object() || ty.is_union() {
            1.0
        } else {
            0.0
        };
        type_cost += BasicCostCalculator::score_selection_set(
            &field.selection_set,
            Some(field.ty().inner_named_type()),
            schema,
        )?;

        // If the field is marked with `@requires`, the required selection may not be included
        // in the query's selection. Adding that requirement's cost to the field ensures it's
        // accounted for.
        let requirements =
            RequiresDirective::from_field(field, parent_type_name, schema)?.map(|d| d.fields);
        let requirements_cost = match requirements {
            Some(selection_set) => {
                BasicCostCalculator::score_selection_set(&selection_set, parent_type_name, schema)?
            }
            None => 0.0,
        };

        Ok(instance_count * type_cost + requirements_cost)
    }

    fn score_fragment_spread(_fragment_spread: &FragmentSpread) -> Result<f64, DemandControlError> {
        Ok(0.0)
    }

    fn score_inline_fragment(
        inline_fragment: &InlineFragment,
        parent_type: Option<&NamedType>,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        BasicCostCalculator::score_selection_set(
            &inline_fragment.selection_set,
            parent_type,
            schema,
        )
    }

    fn score_operation(
        operation: &Operation,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        let mut cost = if operation.is_mutation() { 10.0 } else { 0.0 };
        cost += BasicCostCalculator::score_selection_set(
            &operation.selection_set,
            operation.name.as_ref(),
            schema,
        )?;

        Ok(cost)
    }

    fn score_selection(
        selection: &Selection,
        parent_type: Option<&NamedType>,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        match selection {
            Selection::Field(f) => BasicCostCalculator::score_field(f, parent_type, schema),
            Selection::FragmentSpread(s) => BasicCostCalculator::score_fragment_spread(s),
            Selection::InlineFragment(i) => {
                BasicCostCalculator::score_inline_fragment(i, parent_type, schema)
            }
        }
    }

    fn score_selection_set(
        selection_set: &SelectionSet,
        parent_type_name: Option<&NamedType>,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        for selection in selection_set.selections.iter() {
            cost += BasicCostCalculator::score_selection(selection, parent_type_name, schema)?;
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

    fn score_json(
        value: &TypedValue,
        query: &ExecutableDocument,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        // It's fine to pass None as parent_type_name in all of these because that's
        // used for @requires, and that doesn't apply when counting the actual values.
        match value {
            // TODO(tninesling): This is a shitty experience
            TypedValue::Null => Ok(0.0),
            // BOOL
            TypedValue::Bool(field, _) => {
                let cost_directive = CostDirective::from_field(field)?;
                let cost = if let Some(CostDirective { weight }) = cost_directive {
                    weight
                } else {
                    0.0
                };
                println!("Bool field {}, cost: {}", field.name, cost);
                Ok(cost)
            }
            // NUMBER
            TypedValue::Number(field, _) => {
                let cost_directive = CostDirective::from_field(field)?;
                let cost = if let Some(CostDirective { weight }) = cost_directive {
                    weight
                } else {
                    0.0
                };
                println!("Number field {}, cost: {}", field.name, cost);
                Ok(cost)
            }
            // STRING
            TypedValue::String(field, _) => {
                let cost_directive = CostDirective::from_field(field)?;
                let cost = if let Some(CostDirective { weight }) = cost_directive {
                    weight
                } else {
                    0.0
                };
                println!("String field {}, cost: {}", field.name, cost);
                Ok(cost)
            }
            // ARRAY
            TypedValue::Array(field, items) => {
                let cost_directive = CostDirective::from_field(field);
                let mut cost = 0.0;
                if let Ok(Some(CostDirective { weight })) = cost_directive {
                    cost = weight;
                }
                cost += Self::summed_score_of_values(items, query, schema)?;
                println!("Array field {}, cost: {}", field.name, cost);

                Ok(cost)
            }
            // OBJECT
            TypedValue::Object(field, children) => {
                let cost_directive = CostDirective::from_field(field);
                let mut cost = 1.0;
                if let Ok(Some(CostDirective { weight })) = cost_directive {
                    cost = weight;
                }
                cost += Self::summed_score_of_values(children.values(), query, schema)?;
                println!("Object field {}, cost: {}", field.name, cost);

                Ok(cost)
            }
            // TOP-LEVEL QUERY
            TypedValue::Query(children) => {
                let cost = Self::summed_score_of_values(children.values(), query, schema)?;
                println!("Response root, cost {}", cost);

                Ok(cost)
            }
        }
    }

    fn summed_score_of_values<'a, I: IntoIterator<Item = &'a TypedValue<'a>>>(
        values: I,
        query: &ExecutableDocument,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        let mut score = 0.0;
        for value in values {
            score += Self::score_json(value, query, schema)?;
        }
        Ok(score)
    }
}

impl CostCalculator for BasicCostCalculator {
    fn estimated(
        query: &ExecutableDocument,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        if let Some(op) = &query.anonymous_operation {
            cost += BasicCostCalculator::score_operation(op, schema)?;
        }
        for (_name, op) in query.named_operations.iter() {
            cost += BasicCostCalculator::score_operation(op, schema)?;
        }
        Ok(cost)
    }

    fn actual(
        response: &Response,
        query: &ExecutableDocument,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError> {
        let operation = &query.anonymous_operation.clone().unwrap(); // TODO: Handle named ops
        let schema_aware_response = SchemaAwareResponse::zip(operation, response)?;
        Self::score_json(&schema_aware_response.value, query, schema)
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;
    use serde_json_bytes::Value;
    use test_log::test;

    use super::*;

    fn cost(schema_str: &str, query_str: &str) -> f64 {
        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        BasicCostCalculator::estimated(&query, &schema).unwrap()
    }

    fn actual_cost(schema_str: &str, query_str: &str, response_body: Value) -> f64 {
        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test_service", response_body.to_bytes()).unwrap();
        BasicCostCalculator::actual(&response, &query, &schema).unwrap()
    }

    #[test]
    fn query_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_query.graphql");

        assert_eq!(cost(schema, query), 0.0)
    }

    #[test]
    fn mutation_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_mutation.graphql");

        assert_eq!(cost(schema, query), 10.0)
    }

    #[test]
    fn object_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_object_query.graphql");

        assert_eq!(cost(schema, query), 1.0)
    }

    #[test]
    fn interface_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_interface_query.graphql");

        assert_eq!(cost(schema, query), 1.0)
    }

    #[test]
    fn union_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_union_query.graphql");

        assert_eq!(cost(schema, query), 1.0)
    }

    #[test]
    fn list_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_object_list_query.graphql");

        assert_eq!(cost(schema, query), 100.0)
    }

    #[test]
    fn scalar_list_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_scalar_list_query.graphql");

        assert_eq!(cost(schema, query), 0.0)
    }

    #[test]
    fn nested_object_lists() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_nested_list_query.graphql");

        assert_eq!(cost(schema, query), 10100.0)
    }

    #[test]
    fn skip_directive_excludes_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_skipped_query.graphql");

        assert_eq!(cost(schema, query), 0.0)
    }

    #[test]
    fn include_directive_excludes_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_excluded_query.graphql");

        assert_eq!(cost(schema, query), 0.0)
    }

    #[test]
    fn requires_adds_required_field_cost() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_required_query.graphql");

        assert_eq!(cost(schema, query), 10200.0);
    }

    #[test]
    fn response_cost() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = r#"
            {
                ships {
                    id
                    name
                }
            }
        "#;
        let response_body = json!({
            "data": {
                "ships": [
                    {
                        "id": 1,
                        "name": "Boaty McBoatface"
                    },
                    {
                        "id": 2,
                        "name": "HMS Grapherson"
                    }
                ]
            }
        });

        assert_eq!(actual_cost(schema, query, response_body), 6.0);
    }
}
