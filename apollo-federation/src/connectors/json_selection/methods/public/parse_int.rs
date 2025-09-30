use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

const DEFAULT_BASE: u32 = 10;
impl_arrow_method!(ParseIntMethod, parse_int_method, parse_int_shape);
/// Parses a string or number as an integer with an optional base.
/// Simple examples:
///
/// $("42")->parseInt       results in 42
/// $("20")->parseInt(10)     results in 20
/// $("20")->parseInt(16)     results in 32
/// $("ff")->parseInt(16)     results in 255
/// $(42)->parseInt         results in 42
/// $(123.6)->parseInt      results in 123
/// $("invalid")->parseInt  results in error
fn parse_int_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    // Handle both string and number inputs
    let input_str = match data {
        JSON::String(s) => s.as_str().to_string(),
        JSON::Number(num) => {
            // For numbers, convert to string representation for consistent parsing
            if let Some(int_val) = num.as_i64() {
                int_val.to_string()
            } else if let Some(float_val) = num.as_f64() {
                // Truncate float to integer, then convert to string
                let truncated = float_val.trunc() as i64;
                truncated.to_string()
            } else {
                return (
                    None,
                    vec![ApplyToError::new(
                        format!(
                            "Method ->{} cannot parse number: {}",
                            method_name.as_ref(),
                            num
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    )],
                );
            }
        }
        _ => {
            return (
                None,
                vec![ApplyToError::new(
                    format!(
                        "Method ->{} can only parse strings and numbers. Found: {}",
                        method_name.as_ref(),
                        data
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                )],
            );
        }
    };

    if let Some(args) = method_args
        && args.args.len() > 1
    {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} accepts at most one argument (base), but {} were provided",
                    method_name.as_ref(),
                    args.args.len()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    }

    // Parse base argument or use default (10)
    let base = match method_args
        .and_then(|args| args.args.first())
        .map(|first_arg| first_arg.apply_to_path(data, vars, input_path, spec))
    {
        Some((Some(JSON::Number(base_num)), _)) => {
            let Some(base_value) = base_num.as_u64() else {
                return (
                    None,
                    vec![ApplyToError::new(
                        format!(
                            "Method ->{} base argument must be an integer. Found: {}",
                            method_name.as_ref(),
                            base_num
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    )],
                );
            };

            // Validate radix range to prevent panic in from_str_radix
            if !(2..=36).contains(&base_value) {
                return (
                    None,
                    vec![ApplyToError::new(
                        format!(
                            "Method ->{} failed to parse '{}' as integer with base {} (radix must be between 2 and 36)",
                            method_name.as_ref(),
                            input_str,
                            base_value
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    )],
                );
            }
            base_value as u32
        }
        Some((Some(other), _)) => {
            return (
                None,
                vec![ApplyToError::new(
                    format!(
                        "Method ->{} base argument must be a number. Found: {}",
                        method_name.as_ref(),
                        other
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                )],
            );
        }
        Some((None, arg_errors)) => {
            return (None, arg_errors);
        }
        None => DEFAULT_BASE,
    };

    // Parse the string with the specified base
    match i64::from_str_radix(&input_str, base) {
        Ok(parsed_value) => (Some(JSON::Number(parsed_value.into())), vec![]),
        Err(_) => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} failed to parse '{}' as integer with base {}",
                    method_name.as_ref(),
                    input_str,
                    base
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        ),
    }
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn parse_int_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();
    if arg_count > 1 {
        return Shape::error(
            format!(
                "Method ->{} accepts at most one argument (base), but {} were provided",
                method_name.as_ref(),
                arg_count
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    // Check if input is a string, number, or could be a string/number at runtime
    if !(Shape::string([]).accepts(&input_shape)
        || Shape::float([]).accepts(&input_shape)
        || input_shape.accepts(&Shape::unknown([])))
    {
        return Shape::error_with_partial(
            format!(
                "Method ->{} can only parse strings and numbers. Found: {}",
                method_name.as_ref(),
                input_shape
            ),
            Shape::none(),
            method_name.shape_location(context.source_id()),
        );
    }

    // If we have a base argument, validate its shape
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        let arg_shape = first_arg.compute_output_shape(context, input_shape, dollar_shape);

        if !(Shape::int([]).accepts(&arg_shape) || arg_shape.accepts(&Shape::unknown([]))) {
            return Shape::error_with_partial(
                format!(
                    "Method ->{} base argument must be an integer. Found: {}",
                    method_name.as_ref(),
                    arg_shape
                ),
                Shape::int(method_name.shape_location(context.source_id())),
                method_name.shape_location(context.source_id()),
            );
        }
    }

    Shape::int(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::json;

    use crate::connectors::ConnectSpec;
    use crate::connectors::json_selection::ApplyToError;
    use crate::selection;

    #[test]
    fn parse_int_should_parse_decimal_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt
                "#
            )
            .apply_to(&json!({ "value": "42" })),
            (
                Some(json!({
                    "result": 42,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_with_explicit_base_10() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt(10)
                "#
            )
            .apply_to(&json!({ "value": "42" })),
            (
                Some(json!({
                    "result": 42,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_hexadecimal_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt(16)
                "#
            )
            .apply_to(&json!({ "value": "ff" })),
            (
                Some(json!({
                    "result": 255,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_binary_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt(2)
                "#
            )
            .apply_to(&json!({ "value": "1010" })),
            (
                Some(json!({
                    "result": 10,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_octal_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt(8)
                "#
            )
            .apply_to(&json!({ "value": "77" })),
            (
                Some(json!({
                    "result": 63,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_negative_number() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt()
                "#
            )
            .apply_to(&json!({ "value": "-42" })),
            (
                Some(json!({
                    "result": -42,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_zero() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt()
                "#
            )
            .apply_to(&json!({ "value": "0" })),
            (
                Some(json!({
                    "result": 0,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_error_for_invalid_string() {
        let result = selection!(
            r#"
                result: value->parseInt
            "#
        )
        .apply_to(&json!({ "value": "invalid" }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt failed to parse 'invalid' as integer with base 10")
        );
    }

    #[test]
    fn parse_int_should_error_for_invalid_hex_string() {
        let result = selection!(
            r#"
                result: value->parseInt(16)
            "#
        )
        .apply_to(&json!({ "value": "xyz" }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt failed to parse 'xyz' as integer with base 16")
        );
    }

    #[test]
    fn parse_int_should_error_for_empty_string() {
        let result = selection!(
            r#"
                result: value->parseInt
            "#
        )
        .apply_to(&json!({ "value": "" }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt failed to parse '' as integer with base 10")
        );
    }

    #[test]
    fn parse_int_should_error_for_boolean_input() {
        let result = selection!(
            r#"
                result: value->parseInt
            "#
        )
        .apply_to(&json!({ "value": true }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt can only parse strings and numbers. Found: true")
        );
    }

    #[test]
    fn parse_int_should_error_for_null_input() {
        let result = selection!(
            r#"
                result: value->parseInt
            "#
        )
        .apply_to(&json!({ "value": null }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt can only parse strings and numbers. Found: null")
        );
    }

    #[test]
    fn parse_int_should_error_for_array_input() {
        let result = selection!(
            r#"
                result: value->parseInt
            "#
        )
        .apply_to(&json!({ "value": [1, 2, 3] }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt can only parse strings and numbers. Found: [1,2,3]")
        );
    }

    #[test]
    fn parse_int_should_error_for_object_input() {
        let result = selection!(
            r#"
                result: value->parseInt
            "#
        )
        .apply_to(&json!({ "value": {"a": 1} }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt can only parse strings and numbers. Found: {\"a\":1}")
        );
    }

    #[test]
    fn parse_int_should_error_for_invalid_base() {
        let result = selection!(
            r#"
                result: value->parseInt(1)
            "#
        )
        .apply_to(&json!({ "value": "42" }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt failed to parse '42' as integer with base 1 (radix must be between 2 and 36)")
        );
    }

    #[test]
    fn parse_int_should_error_for_base_too_large() {
        let result = selection!(
            r#"
                result: value->parseInt(37)
            "#
        )
        .apply_to(&json!({ "value": "42" }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt failed to parse '42' as integer with base 37 (radix must be between 2 and 36)")
        );
    }

    #[test]
    fn parse_int_should_error_for_non_numeric_base() {
        let result = selection!(
            r#"
                result: value->parseInt("not_a_number")
            "#
        )
        .apply_to(&json!({ "value": "42" }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0].message().contains(
                "Method ->parseInt base argument must be a number. Found: \"not_a_number\""
            )
        );
    }

    #[test]
    fn parse_int_should_error_for_float_base() {
        let result = selection!(
            r#"
                result: value->parseInt(10.5)
            "#
        )
        .apply_to(&json!({ "value": "42" }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->parseInt base argument must be an integer. Found: 10.5")
        );
    }

    #[test]
    fn parse_int_should_error_for_too_many_arguments() {
        let result = selection!(
            r#"
                result: value->parseInt(10, 16)
            "#
        )
        .apply_to(&json!({ "value": "42" }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(result.1[0].message().contains(
            "Method ->parseInt accepts at most one argument (base), but 2 were provided"
        ));
    }

    #[test]
    fn parse_int_should_handle_large_hex_numbers() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt(16)
                "#
            )
            .apply_to(&json!({ "value": "7FFFFFFF" })),
            (
                Some(json!({
                    "result": 2147483647,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_base_36() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt(36)
                "#
            )
            .apply_to(&json!({ "value": "zz" })),
            (
                Some(json!({
                    "result": 1295,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_integer_number() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt
                "#
            )
            .apply_to(&json!({ "value": 42 })),
            (
                Some(json!({
                    "result": 42,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_negative_integer_number() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt
                "#
            )
            .apply_to(&json!({ "value": -123 })),
            (
                Some(json!({
                    "result": -123,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_parse_integer_number_with_base() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt(16)
                "#
            )
            .apply_to(&json!({ "value": 10 })),
            (
                Some(json!({
                    "result": 16,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_truncate_positive_float() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt
                "#
            )
            .apply_to(&json!({ "value": 123.6 })),
            (
                Some(json!({
                    "result": 123,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_truncate_negative_float() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt
                "#
            )
            .apply_to(&json!({ "value": -123.9 })),
            (
                Some(json!({
                    "result": -123,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_handle_zero_float() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt
                "#
            )
            .apply_to(&json!({ "value": 0.0 })),
            (
                Some(json!({
                    "result": 0,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn parse_int_should_truncate_float_with_base() {
        assert_eq!(
            selection!(
                r#"
                    result: value->parseInt(16)
                "#
            )
            .apply_to(&json!({ "value": 10.7 })),
            (
                Some(json!({
                    "result": 16,
                })),
                vec![],
            ),
        );
    }

    #[rstest::rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn parse_int_should_return_none_when_argument_evaluates_to_none(#[case] spec: ConnectSpec) {
        assert_eq!(
            selection!("$.a->parseInt($.missing)", spec).apply_to(&json!({
                "a": "42",
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .missing not found in object",
                    "path": ["missing"],
                    "range": [16, 23],
                    "spec": spec.to_string(),
                }))]
            ),
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use shape::location::Location;
    use shape::location::SourceId;

    use super::*;
    use crate::connectors::Key;
    use crate::connectors::PathSelection;
    use crate::connectors::json_selection::PathList;
    use crate::connectors::json_selection::lit_expr::LitExpr;

    fn get_location() -> Location {
        Location {
            source_id: SourceId::new("test".to_string()),
            span: 0..8,
        }
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        parse_int_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("parseInt".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::unknown([]),
        )
    }

    #[test]
    fn parse_int_shape_should_return_int_for_string_input() {
        assert_eq!(
            get_shape(vec![], Shape::string([])),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn parse_int_shape_should_return_int_for_string_input_with_base() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Number(10.into()), None)],
                Shape::string([])
            ),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn parse_int_shape_should_return_int_for_int_input() {
        assert_eq!(
            get_shape(vec![], Shape::int([])),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn parse_int_shape_should_return_int_for_float_input() {
        assert_eq!(
            get_shape(vec![], Shape::float([])),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn parse_int_shape_should_error_for_boolean_input() {
        assert_eq!(
            get_shape(vec![], Shape::bool([])),
            Shape::error_with_partial(
                "Method ->parseInt can only parse strings and numbers. Found: Bool".to_string(),
                Shape::none(),
                [get_location()]
            )
        );
    }

    #[test]
    fn parse_int_shape_should_error_for_too_many_args() {
        assert_eq!(
            get_shape(
                vec![
                    WithRange::new(LitExpr::Number(10.into()), None),
                    WithRange::new(LitExpr::Number(16.into()), None)
                ],
                Shape::string([])
            ),
            Shape::error(
                "Method ->parseInt accepts at most one argument (base), but 2 were provided"
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn parse_int_shape_should_error_for_non_integer_base() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::String("not_a_number".to_string()),
                    None
                )],
                Shape::string([])
            ),
            Shape::error_with_partial(
                "Method ->parseInt base argument must be an integer. Found: \"not_a_number\""
                    .to_string(),
                Shape::int([get_location()]),
                [get_location()]
            )
        );
    }

    #[test]
    fn parse_int_shape_should_return_int_for_none_args() {
        let location = get_location();
        assert_eq!(
            parse_int_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("parseInt".to_string(), Some(location.span)),
                None,
                Shape::string([]),
                Shape::none(),
            ),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn parse_int_shape_should_return_int_for_unknown_input() {
        assert_eq!(
            get_shape(vec![], Shape::unknown([])),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn parse_int_shape_should_return_int_for_unknown_base_argument() {
        let path = LitExpr::Path(PathSelection {
            path: PathList::Key(
                Key::field("unknown_field").into_with_range(),
                PathList::Empty.into_with_range(),
            )
            .into_with_range(),
        });

        let result = get_shape(vec![path.into_with_range()], Shape::string([]));
        assert_eq!(result, Shape::int([get_location()]));
    }

    #[test]
    fn parse_int_shape_should_error_for_object_input() {
        assert_eq!(
            get_shape(vec![], Shape::empty_object([])),
            Shape::error_with_partial(
                "Method ->parseInt can only parse strings and numbers. Found: {}".to_string(),
                Shape::none(),
                [get_location()]
            )
        );
    }

    #[test]
    fn parse_int_shape_should_error_for_array_input() {
        assert_eq!(
            get_shape(vec![], Shape::tuple([], [])),
            Shape::error_with_partial(
                "Method ->parseInt can only parse strings and numbers. Found: []".to_string(),
                Shape::none(),
                [get_location()]
            )
        );
    }
}
