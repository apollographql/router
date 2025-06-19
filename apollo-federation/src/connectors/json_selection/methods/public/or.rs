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
/// $(true)->or(false)            results in true
/// $(false)->or(true)            results in true
/// $(true)->or(true)             results in true
/// $(false)->or(false)           results in false
/// $(false)->or(false, true)     results in true
fn or_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let mut result = match data {
        JSON::Bool(value) => *value,
        _ => {
            return (
                None,
                vec![ApplyToError::new(
                    format!(
                        "Method ->{} can only be applied to boolean values.",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                )],
            );
        }
    };

    if let Some(MethodArgs { args, .. }) = method_args {
        let mut errors = Vec::new();

        for arg in args {
            if result {
                break;
            }
            let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path);
            errors.extend(arg_errors);

            match value_opt {
                Some(JSON::Bool(value)) => result = value,
                Some(_) => {
                    errors.extend(vec![ApplyToError::new(
                        format!(
                            "Method ->{} can only accept boolean arguments.",
                            method_name.as_ref()
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                    )]);
                }
                None => {}
            }
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
        ShapeCase::Unknown | ShapeCase::Bool(Some(false)) => {
            // Continue onward... if unknown we don't know the shape, and if it's false, it still might be true later!
        }
        _ => {
            return Shape::error(
                format!(
                    "Method ->{} can only be applied to boolean values.",
                    method_name.as_ref()
                ),
                method_name.shape_location(source_id),
            );
        }
    }

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
                ShapeCase::Unknown | ShapeCase::Bool(Some(false)) => {
                    // Continue onward... if unknown we don't know the shape, and if it's false, it still might be true later!
                }
                _ => {
                    return Shape::error(
                        format!(
                            "Method ->{} can only accept boolean arguments.",
                            method_name.as_ref()
                        ),
                        method_name.shape_location(source_id),
                    );
                }
            }
        }
    }

    Shape::bool(method_name.shape_location(source_id))
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
    fn or_should_return_error_when_arguments_are_not_boolean() {
        let result = selection!("$.a->or($.b, $.c)").apply_to(&json!({
            "a": false,
            "b": null,
            "c": 0,
        }));

        assert_eq!(result.0, Some(json!(false)));
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->or can only accept boolean arguments.")
        );
    }
    #[test]
    fn or_should_return_error_when_applied_to_non_boolean() {
        let result = selection!("$.b->or($.a, $.c)").apply_to(&json!({
            "a": false,
            "b": null,
            "c": 0,
        }));

        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->or can only be applied to boolean values.")
        );
    }
}
