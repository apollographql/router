use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::location::SourceId;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::json_selection::methods::common::is_comparable_shape_combination;
use crate::impl_arrow_method;

impl_arrow_method!(GtMethod, gt_method, gt_shape);
/// Returns true if the applied to value is greater than the argument value.
/// Simple examples:
///
/// $(3)->gt(3)       results in false
/// $(4)->gt(3)       results in true
/// $(2)->gt(3)       results in false
/// $("a")->gt("b")   results in false
/// $("c")->gt("b")   results in true
fn gt_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(first_arg) = method_args.and_then(|args| args.args.first()) else {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires exactly one argument",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        );
    };

    let (value_opt, arg_errors) = first_arg.apply_to_path(data, vars, input_path);
    let mut apply_to_errors = arg_errors;
    // We have to do this because Value doesn't implement PartialOrd
    let matches = value_opt.and_then(|value| {
        match (data, &value) {
            // Number comparisons
            (JSON::Number(left), JSON::Number(right)) => {
                let left = match left.as_f64() {
                    Some(val) => val,
                    None => {
                        // Note that we don't have tests for these `None` cases because I can't actually find a case where this ever actually fails
                        // It seems that the current implementation in serde_json always returns a value
                        apply_to_errors.push(ApplyToError::new(
                            format!(
                                "Method ->{} fail to convert applied to value to float.",
                                method_name.as_ref(),
                            ),
                            input_path.to_vec(),
                            method_name.range(),
                        ));
                        return None;
                    }
                };
                let right = match right.as_f64() {
                    Some(val) => val,
                    None => {
                        apply_to_errors.push(ApplyToError::new(
                            format!(
                                "Method ->{} fail to convert argument to float.",
                                method_name.as_ref(),
                            ),
                            input_path.to_vec(),
                            method_name.range(),
                        ));
                        return None;
                    }
                };

                Some(JSON::Bool(left > right))
            }
            // String comparisons
            (JSON::String(left), JSON::String(right)) => Some(JSON::Bool(left > right)),
            // Mixed types or uncomparable types (including arrays and objects) return false
            _ => {
                apply_to_errors.push(ApplyToError::new(
                    format!(
                        "Method ->{} can only compare numbers and strings. Found: {data} > {value}",
                        method_name.as_ref(),
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                ));

                None
            }
        }
    });

    (matches, apply_to_errors)
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn gt_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();
    if arg_count > 1 {
        return Shape::error(
            format!(
                "Method ->{} requires only one argument, but {arg_count} were provided",
                method_name.as_ref(),
            ),
            vec![],
        );
    }

    let Some(first_arg) = method_args.and_then(|args| args.args.first()) else {
        return Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(source_id),
        );
    };

    let arg_shape = first_arg.compute_output_shape(
        input_shape.clone(),
        dollar_shape,
        named_var_shapes,
        source_id,
    );

    if is_comparable_shape_combination(&arg_shape, &input_shape) {
        Shape::bool(method_name.shape_location(source_id))
    } else {
        Shape::error(
            format!(
                "Method ->{} can only compare two numbers or two strings. Found {input_shape} > {arg_shape}",
                method_name.as_ref()
            ),
            method_name.shape_location(source_id),
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn gt_should_return_true_when_applied_to_number_is_greater_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gt(3)
                "#
            )
            .apply_to(&json!({ "value": 4 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gt_should_return_false_when_applied_to_number_equals_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gt(3)
                "#
            )
            .apply_to(&json!({ "value": 3 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gt_should_return_false_when_applied_to_number_is_less_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gt(3)
                "#
            )
            .apply_to(&json!({ "value": 2 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gt_should_return_true_when_applied_to_string_is_greater_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gt("b")
                "#
            )
            .apply_to(&json!({ "value": "c" })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gt_should_return_false_when_applied_to_string_equals_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gt("a")
                "#
            )
            .apply_to(&json!({ "value": "a" })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gt_should_return_false_when_applied_to_string_is_less_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gt("b")
                "#
            )
            .apply_to(&json!({ "value": "a" })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gt_should_return_false_with_error_for_null_values() {
        let result = selection!(
            r#"
                result: value->gt(null)
            "#
        )
        .apply_to(&json!({ "value": null }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->gt can only compare numbers and strings. Found: null > null")
        );
    }

    #[test]
    fn gt_should_return_false_with_error_for_boolean_values() {
        let result = selection!(
            r#"
                result: value->gt(false)
            "#
        )
        .apply_to(&json!({ "value": true }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->gt can only compare numbers and strings. Found: true > false")
        );
    }

    #[test]
    fn gt_should_return_false_with_error_for_arrays() {
        let result = selection!(
            r#"
                    result: value->gt([1,2])
                "#
        )
        .apply_to(&json!({ "value": [1,2,3] }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0].message().contains(
                "Method ->gt can only compare numbers and strings. Found: [1,2,3] > [1,2]"
            )
        );
    }

    #[test]
    fn gt_should_return_false_with_error_for_objects() {
        let result = selection!(
            r#"
                    result: value->gt({"a": 1})
                "#
        )
        .apply_to(&json!({ "value": {"a": 1, "b": 2} }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(result.1[0].message().contains(
            "Method ->gt can only compare numbers and strings. Found: {\"a\":1,\"b\":2} > {\"a\":1}"
        ));
    }

    #[test]
    fn gt_should_return_false_and_error_for_mixed_types() {
        let result = selection!(
            r#"
                    result: value->gt("string")
                "#
        )
        .apply_to(&json!({ "value": 42 }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0].message().contains(
                "Method ->gt can only compare numbers and strings. Found: 42 > \"string\""
            )
        );
    }
}
