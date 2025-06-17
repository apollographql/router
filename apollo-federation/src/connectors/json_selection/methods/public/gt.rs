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
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        let (value_opt, arg_errors) = first_arg.apply_to_path(data, vars, input_path);
        let mut apply_to_errors = arg_errors;
        // We have to do this because Value doesn't implement PartialOrd
        let matches = value_opt.is_some_and(|value| {
            match (data, &value) {
                // Number comparisons
                (JSON::Number(left), JSON::Number(right)) => {
                    left.as_f64().unwrap_or(0.0) > right.as_f64().unwrap_or(0.0)
                }
                // String comparisons
                (JSON::String(left), JSON::String(right)) => left > right,
                // Boolean comparisons
                (JSON::Bool(left), JSON::Bool(right)) => left > right,
                // Null comparisons (null == null)
                (JSON::Null, JSON::Null) => false,
                // Mixed types or uncomparable types (including arrays and objects) return false
                _ => {
                    apply_to_errors.push(ApplyToError::new(
                        format!(
                            "Method ->{} can directly compare numbers, strings, booleans, and null. Either a mix of these was provided or something else such as an array or object. Found: {} > {}",
                            method_name.as_ref(),
                            data,
                            value
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                    ));

                    false
                }
            }
        });

        return (Some(JSON::Bool(matches)), apply_to_errors);
    }
    (
        None,
        vec![ApplyToError::new(
            format!(
                "Method ->{} requires exactly one argument",
                method_name.as_ref()
            ),
            input_path.to_vec(),
            method_name.range(),
        )],
    )
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn gt_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    Shape::bool(method_name.shape_location(source_id))
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
    fn gt_should_compare_null_values() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gt(null)
                "#
            )
            .apply_to(&json!({ "value": null })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gt_should_compare_boolean_values() {
        // true > false should be true
        assert_eq!(
            selection!(
                r#"
                    result: value->gt(false)
                "#
            )
            .apply_to(&json!({ "value": true })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );

        // false > true should be false
        assert_eq!(
            selection!(
                r#"
                    result: value->gt(true)
                "#
            )
            .apply_to(&json!({ "value": false })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
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

        assert_eq!(
            result.0,
            Some(json!({
                "result": false,
            })),
        );
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->gt can directly compare numbers, strings, booleans, and null. Either a mix of these was provided or something else such as an array or object. Found: [1,2,3] > [1,2]")
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

        assert_eq!(
            result.0,
            Some(json!({
                "result": false,
            })),
        );
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->gt can directly compare numbers, strings, booleans, and null. Either a mix of these was provided or something else such as an array or object. Found: {\"a\":1,\"b\":2} > {\"a\":1}")
        );
    }

    #[test]
    fn gt_should_return_false_and_error_for_mixed_types() {
        let result = selection!(
            r#"
                    result: value->gt("string")
                "#
        )
        .apply_to(&json!({ "value": 42 }));

        assert_eq!(
            result.0,
            Some(json!({
                "result": false,
            })),
        );
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->gt can directly compare numbers, strings, booleans, and null. Either a mix of these was provided or something else such as an array or object. Found: 42 > \"string\"")
        );
    }
}
