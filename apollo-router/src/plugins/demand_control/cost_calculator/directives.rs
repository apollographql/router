use ahash::HashMap;
use ahash::HashSet;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::ast::Value as AstValue;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::parser::Parser;
use apollo_compiler::validation::Valid;
use apollo_federation::link::cost_spec_definition::ListSizeDirective as ParsedListSizeDirective;
use indexmap::IndexSet;
use serde_json_bytes::Value as JsonValue;
use tower::BoxError;

use crate::json_ext::Object;
use crate::json_ext::ValueExt;
use crate::plugins::demand_control::DemandControlError;

// Infers a size value from an AST argument value.
//
// Returns:
// - `Some(n)` for integer values (e.g., `first: 10` → 10)
// - `Some(len)` for array values (e.g., `ids: ["a", "b"]` → 2)
// - Resolves variable references through the provided variables map
// - `None` for null, missing, or unsupported value types
fn infer_size_from_argument(value: Option<&AstValue>, variables: &Object) -> Option<i32> {
    match value? {
        AstValue::Int(n) => n.try_to_i32().ok(),
        AstValue::List(items) => Some(items.len() as i32),
        AstValue::Variable(var_name) => infer_size_from_variable(variables.get(var_name.as_str())),
        _ => None,
    }
}

// Infers a size value from a JSON variable value.
fn infer_size_from_variable(value: Option<&JsonValue>) -> Option<i32> {
    match value? {
        JsonValue::Array(items) => Some(items.len() as i32),
        other => other.as_i32(),
    }
}

// Collects slicing argument sizes from both default values and actual query arguments.
// Actual values override defaults when both are present.
fn collect_slicing_sizes<'a>(
    field: &'a Field,
    slicing_argument_names: &IndexSet<String>,
    variables: &Object,
) -> HashMap<&'a str, i32> {
    let is_slicing_arg = |name: &str| slicing_argument_names.contains(name);

    let defaults = field.definition.arguments.iter().filter_map(|arg| {
        is_slicing_arg(arg.name.as_str())
            .then(|| infer_size_from_argument(arg.default_value.as_deref(), variables))
            .flatten()
            .map(|size| (arg.name.as_str(), size))
    });

    let actuals = field.arguments.iter().filter_map(|arg| {
        is_slicing_arg(arg.name.as_str())
            .then(|| infer_size_from_argument(Some(&arg.value), variables))
            .flatten()
            .map(|size| (arg.name.as_str(), size))
    });

    defaults.chain(actuals).collect()
}

pub(in crate::plugins::demand_control) struct IncludeDirective {
    pub(in crate::plugins::demand_control) is_included: bool,
}

impl IncludeDirective {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &Field,
    ) -> Result<Option<Self>, BoxError> {
        let directive = field
            .directives
            .get("include")
            .and_then(|skip| skip.specified_argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_included: cond });

        Ok(directive)
    }
}

pub(in crate::plugins::demand_control) struct ListSizeDirective<'schema> {
    pub(in crate::plugins::demand_control) expected_size: Option<i32>,
    pub(in crate::plugins::demand_control) sized_fields: Option<HashSet<&'schema str>>,
}

impl<'schema> ListSizeDirective<'schema> {
    pub(in crate::plugins::demand_control) fn new(
        parsed: &'schema ParsedListSizeDirective,
        field: &Field,
        variables: &Object,
    ) -> Result<Self, DemandControlError> {
        let expected_size = match parsed.slicing_argument_names.as_ref() {
            Some(slicing_argument_names) => {
                let slicing_sizes = collect_slicing_sizes(field, slicing_argument_names, variables);

                if parsed.require_one_slicing_argument && slicing_sizes.len() != 1 {
                    return Err(DemandControlError::QueryParseFailure(format!(
                        "Exactly one slicing argument is required, but found {}",
                        slicing_sizes.len()
                    )));
                }

                slicing_sizes.into_values().max().or(parsed.assumed_size)
            }
            None => parsed.assumed_size,
        };

        Ok(Self {
            expected_size,
            sized_fields: parsed
                .sized_fields
                .as_ref()
                .map(|set| set.iter().map(|s| s.as_str()).collect()),
        })
    }

    pub(in crate::plugins::demand_control) fn size_of(&self, field: &Field) -> Option<i32> {
        if self
            .sized_fields
            .as_ref()
            .is_some_and(|sf| sf.contains(field.name.as_str()))
        {
            self.expected_size
        } else {
            None
        }
    }
}

pub(in crate::plugins::demand_control) struct RequiresDirective {
    pub(in crate::plugins::demand_control) fields: SelectionSet,
}

impl RequiresDirective {
    pub(in crate::plugins::demand_control) fn from_field_definition(
        definition: &FieldDefinition,
        parent_type_name: &NamedType,
        schema: &Valid<Schema>,
    ) -> Result<Option<Self>, DemandControlError> {
        let requires_arg = definition
            .directives
            .get("join__field")
            .and_then(|requires| requires.specified_argument_by_name("requires"))
            .and_then(|arg| arg.as_str());

        if let Some(arg) = requires_arg {
            let field_set =
                Parser::new().parse_field_set(schema, parent_type_name.clone(), arg, "")?;

            Ok(Some(RequiresDirective {
                fields: field_set.selection_set.clone(),
            }))
        } else {
            Ok(None)
        }
    }
}

pub(in crate::plugins::demand_control) struct SkipDirective {
    pub(in crate::plugins::demand_control) is_skipped: bool,
}

impl SkipDirective {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &Field,
    ) -> Result<Option<Self>, BoxError> {
        let directive = field
            .directives
            .get("skip")
            .and_then(|skip| skip.specified_argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_skipped: cond });

        Ok(directive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod infer_size_from_variable_tests {
        use serde_json_bytes::json;

        use super::*;

        #[rstest::rstest]
        #[case::integer_value(json!(42), Some(42))]
        #[case::zero(json!(0), Some(0))]
        #[case::negative_integer(json!(-5), Some(-5))]
        #[case::array_with_items(json!(["a", "b", "c"]), Some(3))]
        #[case::empty_array(json!([]), Some(0))]
        #[case::null_value(json!(null), None)]
        #[case::string_value(json!("not a size"), None)]
        #[case::boolean_value(json!(true), None)]
        #[case::object_value(json!({"key": "value"}), None)]
        #[case::float_value(json!(1.5), None)]
        fn test_infer_size(#[case] input: JsonValue, #[case] expected: Option<i32>) {
            assert_eq!(infer_size_from_variable(Some(&input)), expected);
        }

        #[test]
        fn none_input_returns_none() {
            assert_eq!(infer_size_from_variable(None), None);
        }
    }

    mod infer_size_from_argument_tests {
        use apollo_compiler::Node;
        use apollo_compiler::ast::IntValue;
        use serde_json_bytes::json;

        use super::*;

        // Helper to create a list with n string items
        fn list_of_size(n: usize) -> AstValue {
            let items = (0..n)
                .map(|i| Node::new(AstValue::String(format!("item{i}"))))
                .collect();
            AstValue::List(items)
        }

        #[rstest::rstest]
        #[case::integer_10("10", Some(10))]
        #[case::integer_0("0", Some(0))]
        #[case::negative("-5", Some(-5))]
        fn integer_values(#[case] input: &str, #[case] expected: Option<i32>) {
            let value = AstValue::Int(IntValue::new_parsed(input));
            assert_eq!(
                infer_size_from_argument(Some(&value), &Object::new()),
                expected
            );
        }

        #[rstest::rstest]
        #[case::three_items(3, Some(3))]
        #[case::one_item(1, Some(1))]
        #[case::empty(0, Some(0))]
        fn list_values(#[case] size: usize, #[case] expected: Option<i32>) {
            let value = list_of_size(size);
            assert_eq!(
                infer_size_from_argument(Some(&value), &Object::new()),
                expected
            );
        }

        #[rstest::rstest]
        #[case::resolves_to_int("count", json!(5), Some(5))]
        #[case::resolves_to_array("ids", json!(["x", "y", "z"]), Some(3))]
        #[case::resolves_to_empty_array("empty", json!([]), Some(0))]
        #[case::resolves_to_null("nullval", json!(null), None)]
        fn variable_resolution(
            #[case] var_name: &str,
            #[case] var_value: serde_json_bytes::Value,
            #[case] expected: Option<i32>,
        ) {
            let value = AstValue::Variable(apollo_compiler::Name::new_unchecked(var_name));
            let mut variables = Object::new();
            variables.insert(var_name, var_value);
            assert_eq!(infer_size_from_argument(Some(&value), &variables), expected);
        }

        #[rstest::rstest]
        #[case::none_input(None)]
        #[case::string_value(Some(AstValue::String("not a size".into())))]
        #[case::boolean_value(Some(AstValue::Boolean(true)))]
        #[case::missing_variable(Some(AstValue::Variable(apollo_compiler::Name::new_unchecked(
            "missing"
        ))))]
        fn unsupported_values_return_none(#[case] value: Option<AstValue>) {
            assert_eq!(
                infer_size_from_argument(value.as_ref(), &Object::new()),
                None
            );
        }
    }
}
