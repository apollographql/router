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
use crate::connectors::json_selection::methods::common::number_value_as_float;
use crate::impl_arrow_method;

impl_arrow_method!(LteMethod, lte_method, lte_shape);
/// Returns true if the applied to value is less than or equal to the argument value.
/// Simple examples:
///
/// $(3)->lte(3)       results in true
/// $(2)->lte(3)       results in true
/// $(4)->lte(3)       results in false
/// $("a")->lte("b")   results in true
/// $("c")->lte("b")   results in false
fn lte_method(
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
                let left = match number_value_as_float(left, method_name, input_path) {
                    Ok(f) => f,
                    Err(err) => {
                        apply_to_errors.push(err);
                        return None;
                    }
                };
                let right = match number_value_as_float(right, method_name, input_path) {
                    Ok(f) => f,
                    Err(err) => {
                        apply_to_errors.push(err);
                        return None;
                    }
                };

                Some(JSON::Bool(left <= right))
            }
            // String comparisons
            (JSON::String(left), JSON::String(right)) => Some(JSON::Bool(left <= right)),
            // Mixed types or incomparable types (including arrays and objects) return false
            _ => {
                apply_to_errors.push(ApplyToError::new(
                    format!(
                        "Method ->{} can only compare numbers and strings. Found: {data} <= {value}",
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
fn lte_shape(
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
        Shape::error_with_partial(
            format!(
                "Method ->{} can only compare two numbers or two strings. Found {input_shape} <= {arg_shape}",
                method_name.as_ref()
            ),
            Shape::bool_value(false, method_name.shape_location(source_id)),
            method_name.shape_location(source_id),
        )
    }
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn lte_should_return_true_when_applied_to_number_is_less_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->lte(3)
                "#
            )
            .apply_to(&json!({ "value": 2 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn lte_should_return_true_when_applied_to_number_equals_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->lte(3)
                "#
            )
            .apply_to(&json!({ "value": 3 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn lte_should_return_false_when_applied_to_number_is_greater_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->lte(3)
                "#
            )
            .apply_to(&json!({ "value": 4 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn lte_should_return_true_when_applied_to_string_is_less_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->lte("b")
                "#
            )
            .apply_to(&json!({ "value": "a" })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn lte_should_return_true_when_applied_to_string_equals_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->lte("a")
                "#
            )
            .apply_to(&json!({ "value": "a" })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn lte_should_return_false_when_applied_to_string_is_greater_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->lte("b")
                "#
            )
            .apply_to(&json!({ "value": "c" })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn lte_should_error_for_null_values() {
        let result = selection!(
            r#"
                result: value->lte(null)
            "#
        )
        .apply_to(&json!({ "value": null }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->lte can only compare numbers and strings. Found: null <= null")
        );
    }

    #[test]
    fn lte_should_error_for_boolean_values() {
        let result = selection!(
            r#"
                result: value->lte(false)
            "#
        )
        .apply_to(&json!({ "value": true }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0].message().contains(
                "Method ->lte can only compare numbers and strings. Found: true <= false"
            )
        );
    }

    #[test]
    fn lte_should_error_for_arrays() {
        let result = selection!(
            r#"
                    result: value->lte([1,2])
                "#
        )
        .apply_to(&json!({ "value": [1,2,3] }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(result.1[0].message().contains(
            "Method ->lte can only compare numbers and strings. Found: [1,2,3] <= [1,2]"
        ));
    }

    #[test]
    fn lte_should_error_for_objects() {
        let result = selection!(
            r#"
                    result: value->lte({"a": 1})
                "#
        )
        .apply_to(&json!({ "value": {"a": 1, "b": 2} }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(result.1[0].message().contains(
            "Method ->lte can only compare numbers and strings. Found: {\"a\":1,\"b\":2} <= {\"a\":1}"
        ));
    }

    #[test]
    fn lte_should_error_for_mixed_types() {
        let result = selection!(
            r#"
                    result: value->lte("string")
                "#
        )
        .apply_to(&json!({ "value": 42 }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(result.1[0].message().contains(
            "Method ->lte can only compare numbers and strings. Found: 42 <= \"string\""
        ));
    }

    #[test]
    fn lte_should_return_error_when_no_arguments_provided() {
        let result = selection!(
            r#"
                    result: value->lte()
                "#
        )
        .apply_to(&json!({ "value": 42 }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->lte requires exactly one argument")
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use serde_json::Number;
    use shape::location::Location;

    use super::*;
    use crate::connectors::json_selection::lit_expr::LitExpr;

    fn get_location() -> Location {
        Location {
            source_id: SourceId::new("test".to_string()),
            span: 0..7,
        }
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        lte_shape(
            &WithRange::new("lte".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::none(),
            &IndexMap::default(),
            &location.source_id,
        )
    }

    #[test]
    fn lte_shape_should_return_bool_on_valid_strings() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("a".to_string()), None)],
                Shape::string([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn lte_shape_should_return_bool_on_valid_numbers() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(42)), None)],
                Shape::int([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn lte_shape_should_error_on_mixed_types() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("a".to_string()), None)],
                Shape::int([])
            ),
            Shape::error_with_partial(
                "Method ->lte can only compare two numbers or two strings. Found Int <= \"a\""
                    .to_string(),
                Shape::bool_value(false, [get_location()]),
                [get_location()]
            )
        );
    }

    #[test]
    fn lte_shape_should_error_on_no_args() {
        assert_eq!(
            get_shape(vec![], Shape::string([])),
            Shape::error(
                "Method ->lte requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn lte_shape_should_error_on_too_many_args() {
        assert_eq!(
            get_shape(
                vec![
                    WithRange::new(LitExpr::Number(Number::from(42)), None),
                    WithRange::new(LitExpr::Number(Number::from(42)), None)
                ],
                Shape::int([])
            ),
            Shape::error(
                "Method ->lte requires only one argument, but 2 were provided".to_string(),
                []
            )
        );
    }

    #[test]
    fn lte_shape_should_error_on_none_args() {
        let location = get_location();
        assert_eq!(
            lte_shape(
                &WithRange::new("lte".to_string(), Some(location.span)),
                None,
                Shape::string([]),
                Shape::none(),
                &IndexMap::default(),
                &location.source_id
            ),
            Shape::error(
                "Method ->lte requires one argument".to_string(),
                [get_location()]
            )
        );
    }
}
