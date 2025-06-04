use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(OrMethod, or_method, or_shape);
/// Given 2 or more values to compare, returns true if any of the values are truthy or false if none of them are truthy.
///
/// Examples:
/// $->echo(true)->or(false)            results in true
/// $->echo(false)->or(true)            results in true
/// $->echo(true)->or(true)             results in true
/// $->echo(false)->or(false)           results in false
/// $->echo(false)->or(false, true)     results in true
fn or_method(
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
            if result {
                break;
            }
            let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path);
            errors.extend(arg_errors);
            result = value_opt.map(|value| is_truthy(&value)).unwrap_or(false);
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
fn or_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Bool(Some(true)) => {
            return Shape::bool_value(true, method_name.shape_location(source_id));
        }
        ShapeCase::Int(Some(value)) if *value != 0 => {
            return Shape::bool_value(true, method_name.shape_location(source_id));
        }
        ShapeCase::String(Some(value)) if !value.is_empty() => {
            return Shape::bool_value(true, method_name.shape_location(source_id));
        }
        ShapeCase::Array { .. } | ShapeCase::Object { .. } => {
            return Shape::bool_value(true, method_name.shape_location(source_id));
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
                ShapeCase::Bool(Some(true)) => {
                    return Shape::bool_value(true, method_name.shape_location(source_id));
                }
                ShapeCase::Int(Some(value)) if *value != 0 => {
                    return Shape::bool_value(true, method_name.shape_location(source_id));
                }
                ShapeCase::String(Some(value)) if !value.is_empty() => {
                    return Shape::bool_value(true, method_name.shape_location(source_id));
                }
                ShapeCase::Array { .. } | ShapeCase::Object { .. } => {
                    return Shape::bool_value(true, method_name.shape_location(source_id));
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
    fn or_should_return_true_when_either_value_is_truthy() {
        assert_eq!(
            selection!("$.both->or($.and)").apply_to(&json!({
                "both": true,
                "and": false,
            })),
            (Some(json!(true)), vec![]),
        );
    }
    #[test]
    fn or_should_return_false_when_neither_value_is_truthy() {
        assert_eq!(
            selection!("data.x->or($.data.y)").apply_to(&json!({
                "data": {
                    "x": false,
                    "y": false,
                },
            })),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn or_should_return_true_when_any_value_is_truthy() {
        assert_eq!(
            selection!("$.a->or($.b, $.c)").apply_to(&json!({
                "a": true,
                "b": null,
                "c": true,
            })),
            (Some(json!(true)), vec![]),
        );
    }
    #[test]
    fn or_should_return_false_when_no_value_is_truthy() {
        assert_eq!(
            selection!("$.b->or($.a, $.c)").apply_to(&json!({
                "a": false,
                "b": null,
                "c": 0,
            })),
            (Some(json!(false)), vec![]),
        );
    }
}
