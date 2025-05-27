use crate::sources::connect::json_selection::safe_json::Value as JSON;
use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

impl_arrow_method!(AndMethod, and_method, and_shape);
/// Given 2 or more values to compare, returns true if all of the values are truthy or false if any of them are falsy.
///
/// Examples:
/// $->echo(true)->and(false)               results in false
/// $->echo(false)->and(true)               results in false
/// $->echo(true)->and(true)                results in true
/// $->echo(false)->and(false)              results in false
/// $->echo(true)->and(true, true)          results in true
fn and_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        let mut result = is_truthy(data);
        let mut errors = Vec::new();

        for arg in args {
            if !result {
                break;
            }
            let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path);
            errors.extend(arg_errors);
            result = value_opt.is_some_and(|value| is_truthy(&value));
        }

        (Some(JSON::Bool(result)), errors)
    } else {
        (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires arguments", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
            )],
        )
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn and_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Bool(Some(false)) => {
            return Shape::bool_value(false, method_name.shape_location(source_id));
        }
        ShapeCase::Int(Some(value)) if *value == 0 => {
            return Shape::bool_value(false, method_name.shape_location(source_id));
        }
        ShapeCase::String(Some(value)) if value.is_empty() => {
            return Shape::bool_value(false, method_name.shape_location(source_id));
        }
        ShapeCase::Null => {
            return Shape::bool_value(false, method_name.shape_location(source_id));
        }
        _ => {}
    };

    if let Some(MethodArgs { args, .. }) = method_args {
        for arg in args {
            let arg_shape = arg.compute_output_shape(
                input_shape.clone(),
                dollar_shape.clone(),
                named_var_shapes,
                source_id,
            );
            match arg_shape.case() {
                ShapeCase::Bool(Some(false)) => {
                    return Shape::bool_value(false, method_name.shape_location(source_id));
                }
                ShapeCase::Int(Some(value)) if *value == 0 => {
                    return Shape::bool_value(false, method_name.shape_location(source_id));
                }
                ShapeCase::String(Some(value)) if value.is_empty() => {
                    return Shape::bool_value(false, method_name.shape_location(source_id));
                }
                ShapeCase::Null => {
                    return Shape::bool_value(false, method_name.shape_location(source_id));
                }
                _ => {}
            }
        }
    }

    Shape::bool(method_name.shape_location(source_id))
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
    fn and_should_return_true_when_both_values_are_truthy() {
        assert_eq!(
            selection!("$.both->and($.and)").apply_to(&json!({
                "both": true,
                "and": true,
            })),
            (Some(json!(true)), vec![]),
        );
    }
    #[test]
    fn and_should_return_false_when_either_value_is_falsy() {
        assert_eq!(
            selection!("data.x->and($.data.y)").apply_to(&json!({
                "data": {
                    "x": true,
                    "y": false,
                },
            })),
            (Some(json!(false)), vec![]),
        );
    }
    #[test]
    fn and_should_return_false_when_any_value_is_falsy() {
        assert_eq!(
            selection!("$.a->and($.b, $.c)").apply_to(&json!({
                "a": true,
                "b": null,
                "c": true,
            })),
            (Some(json!(false)), vec![]),
        );
    }
    #[test]
    fn and_should_true_when_all_values_are_truthy() {
        assert_eq!(
            selection!("$.b->and($.c, $.a)").apply_to(&json!({
                "a": "hello",
                "b": true,
                "c": 123,
            })),
            (Some(json!(true)), vec![]),
        );
    }
}
