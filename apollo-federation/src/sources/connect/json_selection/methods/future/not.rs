use crate::sources::connect::json_selection::safe_json::Value as JSON;
use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

impl_arrow_method!(NotMethod, not_method, not_shape);
/// Given a value, inverses the boolean of that value. True becomes false, false becomes true.
///
/// Examples:
/// $->echo(true)->not          results in false
/// $->echo(false)->not         results in true
/// $->echo(0)->not             results in true
/// $->echo(1)->not             results in false
fn not_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if method_args.is_some() {
        (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} does not take any arguments",
                    method_name.as_ref()
                ),
                input_path
                    .to_vec()
                    .into_iter()
                    .map(|safe_json| safe_json.into())
                    .collect(),
                method_name.range(),
            )],
        )
    } else {
        (Some(JSON::Bool(!is_truthy(data))), vec![])
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn not_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Bool(Some(value)) => {
            Shape::bool_value(!*value, method_name.shape_location(source_id))
        }
        ShapeCase::Int(Some(value)) => {
            Shape::bool_value(*value == 0, method_name.shape_location(source_id))
        }
        ShapeCase::String(Some(value)) => {
            Shape::bool_value(value.is_empty(), method_name.shape_location(source_id))
        }
        ShapeCase::Null => Shape::bool_value(true, method_name.shape_location(source_id)),
        ShapeCase::Array { .. } | ShapeCase::Object { .. } => {
            Shape::bool_value(false, method_name.shape_location(source_id))
        }
        _ => Shape::bool(method_name.shape_location(source_id)),
    }
}

fn is_truthy(data: &JSON) -> bool {
    match data {
        JSON::Bool(b) => *b,
        JSON::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        JSON::Null => false,
        JSON::String(s) => !s.as_str().is_empty(),
        JSON::Object(_) | JSON::Array(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn not_should_inverse_boolean_of_values() {
        assert_eq!(
            selection!("$->map(@->not)").apply_to(&json!([
                true,
                false,
                0,
                1,
                -123,
                null,
                "hello",
                {},
                [],
            ])),
            (
                Some(json!([
                    false, true, true, false, false, true, false, false, false,
                ])),
                vec![],
            ),
        );
    }

    #[test]
    fn not_should_inverse_boolean() {
        assert_eq!(
            selection!("$->map(@->not->not)").apply_to(&json!([
                true,
                false,
                0,
                1,
                -123,
                null,
                "hello",
                {},
                [],
            ])),
            (
                Some(json!([
                    true, false, false, true, true, false, true, true, true,
                ])),
                vec![],
            ),
        );
    }
}
