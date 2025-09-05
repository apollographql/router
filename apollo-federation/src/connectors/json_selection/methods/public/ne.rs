use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::json_selection::methods::common::number_value_as_float;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(NeMethod, ne_method, ne_shape);
/// Returns true if argument is not equal to the applied to value or false if they are equal.
/// Simple examples:
///
/// $(123)->ne(123)       results in false
/// $(123)->ne(456)       results in true
fn ne_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args
        && let [arg] = args.as_slice()
    {
        let (value_opt, mut apply_to_errors) = arg.apply_to_path(data, vars, input_path, spec);
        let matches = value_opt.and_then(|value| match (data, &value) {
            // Number comparisons: Always convert to float so 1 == 1.0
            (JSON::Number(left), JSON::Number(right)) => {
                let left = match number_value_as_float(left, method_name, input_path, spec) {
                    Ok(f) => f,
                    Err(err) => {
                        apply_to_errors.push(err);
                        return None;
                    }
                };
                let right = match number_value_as_float(right, method_name, input_path, spec) {
                    Ok(f) => f,
                    Err(err) => {
                        apply_to_errors.push(err);
                        return None;
                    }
                };

                Some(JSON::Bool(left != right))
            }
            // Everything else
            _ => Some(JSON::Bool(&value != data)),
        });

        return (matches, apply_to_errors);
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
            spec,
        )],
    )
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn ne_shape(
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
                "Method ->{} requires only one argument, but {arg_count} were provided",
                method_name.as_ref(),
            ),
            vec![],
        );
    }

    let Some(first_arg) = method_args.and_then(|args| args.args.first()) else {
        return Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(context.source_id()),
        );
    };
    let arg_shape = first_arg.compute_output_shape(context, input_shape.clone(), dollar_shape);

    // Ensures that the arguments are of the same type... this includes covering cases like int/float and unknown/name
    if !(input_shape.accepts(&arg_shape) || arg_shape.accepts(&input_shape)) {
        return Shape::error_with_partial(
            format!(
                "Method ->{} can only compare values of the same type. Got {input_shape} != {arg_shape}.",
                method_name.as_ref()
            ),
            Shape::bool_value(true, method_name.shape_location(context.source_id())),
            method_name.shape_location(context.source_id()),
        );
    }

    Shape::bool(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn ne_should_return_false_when_applied_to_equals_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne(123)
                "#
            )
            .apply_to(&json!({ "value": 123 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_true_when_applied_to_does_not_equal_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne(1234)
                "#
            )
            .apply_to(&json!({ "value": 123 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_false_when_applied_to_numbers_of_different_types() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne(1)
                "#
            )
            .apply_to(&json!({ "value": 1.0 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_false_when_applied_to_negative_numbers_of_different_types() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne(-1)
                "#
            )
            .apply_to(&json!({ "value": -1.0 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_false_when_applied_to_equals_string_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne("hello")
                "#
            )
            .apply_to(&json!({ "value": "hello" })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_true_when_applied_to_does_not_equal_string_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne("world")
                "#
            )
            .apply_to(&json!({ "value": "hello" })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_false_when_applied_to_equals_bool_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne(true)
                "#
            )
            .apply_to(&json!({ "value": true })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_true_when_applied_to_does_not_equal_bool_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne(false)
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
    }

    #[test]
    fn ne_should_return_false_when_applied_to_equals_object_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne({"name": "John", "age": 30})
                "#
            )
            .apply_to(&json!({ "value": {"name": "John", "age": 30} })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_true_when_applied_to_does_not_equal_object_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne({"name": "Jane", "age": 25})
                "#
            )
            .apply_to(&json!({ "value": {"name": "John", "age": 30} })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_false_when_applied_to_equals_array_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne([1, 2, 3])
                "#
            )
            .apply_to(&json!({ "value": [1, 2, 3] })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_true_when_applied_to_does_not_equal_array_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->ne([4, 5, 6])
                "#
            )
            .apply_to(&json!({ "value": [1, 2, 3] })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn ne_should_return_error_when_no_arguments_provided() {
        let result = selection!(
            r#"
                result: value->ne()
            "#
        )
        .apply_to(&json!({ "value": 123 }));

        assert_eq!(result.0, Some(json!({})));
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->ne requires exactly one argument")
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use serde_json::Number;
    use shape::location::Location;
    use shape::location::SourceId;

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
        ne_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("ne".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::none(),
        )
    }

    #[test]
    fn ne_shape_should_return_bool_on_valid_strings() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("a".to_string()), None)],
                Shape::string([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn ne_shape_should_return_bool_on_valid_numbers() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(42)), None)],
                Shape::int([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn ne_shape_should_return_bool_on_valid_booleans() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                Shape::bool([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn ne_shape_should_error_on_mixed_types() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("a".to_string()), None)],
                Shape::int([])
            ),
            Shape::error_with_partial(
                "Method ->ne can only compare values of the same type. Got Int != \"a\"."
                    .to_string(),
                Shape::bool_value(true, [get_location()]),
                [get_location()]
            )
        );
    }

    #[test]
    fn ne_shape_should_error_on_no_args() {
        assert_eq!(
            get_shape(vec![], Shape::string([])),
            Shape::error(
                "Method ->ne requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn ne_shape_should_error_on_too_many_args() {
        assert_eq!(
            get_shape(
                vec![
                    WithRange::new(LitExpr::Number(Number::from(42)), None),
                    WithRange::new(LitExpr::Number(Number::from(43)), None)
                ],
                Shape::int([])
            ),
            Shape::error(
                "Method ->ne requires only one argument, but 2 were provided".to_string(),
                []
            )
        );
    }

    #[test]
    fn ne_shape_should_error_on_none_args() {
        let location = get_location();
        assert_eq!(
            ne_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("ne".to_string(), Some(location.span)),
                None,
                Shape::string([]),
                Shape::none(),
            ),
            Shape::error(
                "Method ->ne requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn ne_shape_should_return_bool_on_unknown_input() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("test".to_string()), None)],
                Shape::unknown([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn ne_shape_should_return_bool_on_named_input() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(42)), None)],
                Shape::name("a", [])
            ),
            Shape::bool([get_location()])
        );
    }
}
