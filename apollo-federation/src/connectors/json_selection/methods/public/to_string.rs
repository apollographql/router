use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_to_string;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(ToStringMethod, to_string_method, to_string_shape);
/// Returns a string representation of the applied value.
/// Simple examples:
///
/// $(42)->toString()         results in "42"
/// $("hello")->toString()    results in "hello"
/// $(true)->toString()       results in "true"
/// $(null)->toString()       results in ""
/// $([1,2,3])->toString()    results in error
/// $({a: 1})->toString()     results in error
fn to_string_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(args) = method_args
        && !args.args.is_empty()
    {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} does not accept any arguments, but {} were provided",
                    method_name.as_ref(),
                    args.args.len()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    }

    let string_value = match json_to_string(data) {
        Ok(result) => result.unwrap_or_default(),
        Err(error) => {
            return (
                None,
                vec![ApplyToError::new(
                    format!(
                        "Method ->{} {error} Use ->jsonStringify or ->joinNotNull instead",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                )],
            );
        }
    };

    (Some(JSON::String(string_value.into())), vec![])
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn to_string_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();
    if arg_count > 0 {
        return Shape::error(
            format!(
                "Method ->{} does not accept any arguments, but {arg_count} were provided",
                method_name.as_ref(),
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    // Check if input is an object or array shape
    if Shape::empty_object([]).accepts(&input_shape) || Shape::tuple([], []).accepts(&input_shape) {
        return Shape::error_with_partial(
            format!(
                "Method ->{} cannot convert arrays or objects to strings. Use ->jsonStringify or ->joinNotNull instead",
                method_name.as_ref()
            ),
            Shape::none(),
            method_name.shape_location(context.source_id()),
        );
    }

    Shape::string(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn to_string_should_convert_number_to_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->toString()
                "#
            )
            .apply_to(&json!({ "value": 42 })),
            (
                Some(json!({
                    "result": "42",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn to_string_should_convert_boolean_true_to_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->toString()
                "#
            )
            .apply_to(&json!({ "value": true })),
            (
                Some(json!({
                    "result": "true",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn to_string_should_convert_boolean_false_to_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->toString()
                "#
            )
            .apply_to(&json!({ "value": false })),
            (
                Some(json!({
                    "result": "false",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn to_string_should_convert_null_to_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->toString()
                "#
            )
            .apply_to(&json!({ "value": null })),
            (
                Some(json!({
                    "result": "",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn to_string_should_keep_string_as_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->toString()
                "#
            )
            .apply_to(&json!({ "value": "hello" })),
            (
                Some(json!({
                    "result": "hello",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn to_string_should_error_for_arrays() {
        let result = selection!(
            r#"
                result: value->toString()
            "#
        )
        .apply_to(&json!({ "value": [1, 2, 3] }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->toString cannot convert arrays or objects to strings. Use ->jsonStringify or ->joinNotNull instead")
        );
    }

    #[test]
    fn to_string_should_error_for_objects() {
        let result = selection!(
            r#"
                result: value->toString()
            "#
        )
        .apply_to(&json!({ "value": {"a": 1, "b": 2} }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->toString cannot convert arrays or objects to strings. Use ->jsonStringify or ->joinNotNull instead")
        );
    }

    #[test]
    fn to_string_should_convert_float_to_string() {
        assert_eq!(
            selection!(
                r#"
                    result: value->toString()
                "#
            )
            .apply_to(&json!({ "value": 1.23 })),
            (
                Some(json!({
                    "result": "1.23",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn to_string_should_error_when_arguments_provided() {
        let result = selection!(
            r#"
                result: value->toString("arg")
            "#
        )
        .apply_to(&json!({ "value": 42 }));

        assert_eq!(result.0, Some(json!({})),);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->toString does not accept any arguments, but 1 were provided")
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use shape::location::Location;
    use shape::location::SourceId;

    use super::*;
    use crate::connectors::json_selection::lit_expr::LitExpr;

    fn get_location() -> Location {
        Location {
            source_id: SourceId::new("test".to_string()),
            span: 0..8,
        }
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        to_string_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("toString".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::unknown([]),
        )
    }

    #[test]
    fn to_string_shape_should_return_string_for_int_input() {
        assert_eq!(
            get_shape(vec![], Shape::int([])),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn to_string_shape_should_return_string_for_string_input() {
        assert_eq!(
            get_shape(vec![], Shape::string([])),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn to_string_shape_should_return_string_for_bool_input() {
        assert_eq!(
            get_shape(vec![], Shape::bool([])),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn to_string_shape_should_return_string_for_null_input() {
        assert_eq!(
            get_shape(vec![], Shape::null([])),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn to_string_shape_should_return_string_for_unknown_input() {
        assert_eq!(
            get_shape(vec![], Shape::unknown([])),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn to_string_shape_should_return_string_for_name_input() {
        assert_eq!(
            get_shape(vec![], Shape::name("a", [])),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn to_string_shape_should_error_on_args() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("arg".to_string()), None)],
                Shape::int([])
            ),
            Shape::error(
                "Method ->toString does not accept any arguments, but 1 were provided".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn to_string_shape_should_return_string_for_none_args() {
        let location = get_location();
        assert_eq!(
            to_string_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("toString".to_string(), Some(location.span)),
                None,
                Shape::int([]),
                Shape::none(),
            ),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn to_string_shape_should_error_for_object_input() {
        assert_eq!(
            get_shape(vec![], Shape::empty_object([])),
            Shape::error_with_partial(
                "Method ->toString cannot convert arrays or objects to strings. Use ->jsonStringify or ->joinNotNull instead"
                    .to_string(),
                Shape::none(),
                [get_location()]
            )
        );
    }

    #[test]
    fn to_string_shape_should_error_for_array_input() {
        assert_eq!(
            get_shape(vec![], Shape::tuple([], [])),
            Shape::error_with_partial(
                "Method ->toString cannot convert arrays or objects to strings. Use ->jsonStringify or ->joinNotNull instead"
                    .to_string(),
                Shape::none(),
                [get_location()]
            )
        );
    }
}
