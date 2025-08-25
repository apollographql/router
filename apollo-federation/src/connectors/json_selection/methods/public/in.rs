use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::json_selection::methods::common::number_value_as_float;
use crate::impl_arrow_method;

impl_arrow_method!(InMethod, in_method, in_shape);
/// Returns true if the applied value is equal to any of the values in the array argument.
/// Simple examples:
///
/// $(123)->in([123, 456, 789])       results in true
/// $(123)->in([456, 789])            results in false
fn in_method(
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
        let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path, spec);
        let mut apply_to_errors = arg_errors;

        let matches = value_opt.and_then(|value| {
            if let JSON::Array(array) = &value {
                for item in array {
                    let is_equal = match (data, item) {
                        // Number comparisons: Always convert to float so 1 == 1.0
                        (JSON::Number(left), JSON::Number(right)) => {
                            let left =
                                match number_value_as_float(left, method_name, input_path, spec) {
                                    Ok(f) => f,
                                    Err(err) => {
                                        apply_to_errors.push(err);
                                        return None;
                                    }
                                };
                            let right =
                                match number_value_as_float(right, method_name, input_path, spec) {
                                    Ok(f) => f,
                                    Err(err) => {
                                        apply_to_errors.push(err);
                                        return None;
                                    }
                                };
                            left == right
                        }
                        // Everything else
                        _ => item == data,
                    };

                    if is_equal {
                        return Some(JSON::Bool(true));
                    }
                }
                Some(JSON::Bool(false))
            } else {
                apply_to_errors.push(ApplyToError::new(
                    format!(
                        "Method ->{} requires an array argument, but got: {value}",
                        method_name.as_ref(),
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                ));
                None
            }
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
fn in_shape(
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

    if !Shape::tuple([], []).accepts(&arg_shape) && !arg_shape.accepts(&Shape::unknown([])) {
        return Shape::error(
            format!(
                "Method ->{} requires an array argument, but got: {arg_shape}",
                method_name.as_ref()
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    let ShapeCase::Array { prefix, tail } = arg_shape.case() else {
        return Shape::bool(method_name.shape_location(context.source_id()));
    };

    // Ensures that the input is of the same type as all the array elements... this includes covering cases like int/float and unknown/name
    if let Some(item) = prefix
        .iter()
        .find(|item| !(input_shape.accepts(item) || item.accepts(&input_shape)))
    {
        return Shape::error_with_partial(
            format!(
                "Method ->{} can only compare values of the same type. Got {input_shape} == {item}.",
                method_name.as_ref()
            ),
            Shape::bool_value(false, method_name.shape_location(context.source_id())),
            method_name.shape_location(context.source_id()),
        );
    }

    // Also check the tail for type mismatch
    if !(tail.is_none() || input_shape.accepts(tail) || tail.accepts(&input_shape)) {
        return Shape::error_with_partial(
            format!(
                "Method ->{} can only compare values of the same type. Got {input_shape} == {tail}.",
                method_name.as_ref()
            ),
            Shape::bool_value(false, method_name.shape_location(context.source_id())),
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
    fn in_should_return_true_when_applied_to_value_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([123, 456, 789])
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
    fn in_should_return_false_when_applied_to_value_not_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([456, 789])
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
    fn in_should_return_true_when_applied_to_numbers_of_different_types() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([1, 2.5, 3])
                "#
            )
            .apply_to(&json!({ "value": 1.0 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn in_should_return_true_when_applied_to_string_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in(["hello", "world", "test"])
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
    fn in_should_return_false_when_applied_to_string_not_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in(["world", "test"])
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
    fn in_should_return_true_when_applied_to_bool_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([true, false])
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
    fn in_should_return_false_when_applied_to_bool_not_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([false])
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
    fn in_should_return_true_when_applied_to_object_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([{"name": "John", "age": 30}, {"name": "Jane", "age": 25}])
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
    fn in_should_return_false_when_applied_to_object_not_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([{"name": "Jane", "age": 25}])
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
    fn in_should_return_true_when_applied_to_array_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([[1, 2, 3], [4, 5, 6]])
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
    fn in_should_return_false_when_applied_to_array_not_in_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([[4, 5, 6], [7, 8, 9]])
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
    fn in_should_return_false_for_empty_array() {
        assert_eq!(
            selection!(
                r#"
                    result: value->in([])
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
    fn in_should_return_error_when_no_arguments_provided() {
        let result = selection!(
            r#"
                result: value->in()
            "#
        )
        .apply_to(&json!({ "value": 123 }));

        assert_eq!(result.0, Some(json!({})));
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->in requires exactly one argument")
        );
    }

    #[test]
    fn in_should_return_error_when_argument_is_not_array() {
        let result = selection!(
            r#"
                result: value->in(123)
            "#
        )
        .apply_to(&json!({ "value": 123 }));

        assert_eq!(result.0, Some(json!({})));
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->in requires an array argument, but got: 123")
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
        in_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("in".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::none(),
        )
    }

    #[test]
    fn in_shape_should_return_bool_on_valid_string_array() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::Array(vec![
                        WithRange::new(LitExpr::String("a".to_string()), None),
                        WithRange::new(LitExpr::String("b".to_string()), None),
                    ]),
                    None
                )],
                Shape::string([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn in_shape_should_return_bool_on_valid_number_array() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::Array(vec![
                        WithRange::new(LitExpr::Number(Number::from(42)), None),
                        WithRange::new(LitExpr::Number(Number::from(43)), None),
                    ]),
                    None
                )],
                Shape::int([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn in_shape_should_return_bool_on_valid_bool_array() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::Array(vec![
                        WithRange::new(LitExpr::Bool(true), None),
                        WithRange::new(LitExpr::Bool(false), None),
                    ]),
                    None
                )],
                Shape::bool([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn in_shape_should_error_on_non_array_argument() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("a".to_string()), None)],
                Shape::string([])
            ),
            Shape::error(
                "Method ->in requires an array argument, but got: \"a\"".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn in_shape_should_error_on_mixed_types() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::Array(vec![WithRange::new(LitExpr::String("a".to_string()), None),]),
                    None
                )],
                Shape::int([])
            ),
            Shape::error_with_partial(
                "Method ->in can only compare values of the same type. Got Int == \"a\"."
                    .to_string(),
                Shape::bool_value(false, [get_location()]),
                [get_location()]
            )
        );
    }

    #[test]
    fn in_shape_should_error_on_no_args() {
        assert_eq!(
            get_shape(vec![], Shape::string([])),
            Shape::error(
                "Method ->in requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn in_shape_should_error_on_too_many_args() {
        assert_eq!(
            get_shape(
                vec![
                    WithRange::new(
                        LitExpr::Array(vec![WithRange::new(
                            LitExpr::Number(Number::from(42)),
                            None
                        ),]),
                        None
                    ),
                    WithRange::new(
                        LitExpr::Array(vec![WithRange::new(
                            LitExpr::Number(Number::from(43)),
                            None
                        ),]),
                        None
                    )
                ],
                Shape::int([])
            ),
            Shape::error(
                "Method ->in requires only one argument, but 2 were provided".to_string(),
                []
            )
        );
    }

    #[test]
    fn in_shape_should_error_on_none_args() {
        let location = get_location();
        assert_eq!(
            in_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("in".to_string(), Some(location.span)),
                None,
                Shape::string([]),
                Shape::none(),
            ),
            Shape::error(
                "Method ->in requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn in_shape_should_return_bool_on_unknown_input() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::Array(vec![WithRange::new(
                        LitExpr::String("test".to_string()),
                        None
                    ),]),
                    None
                )],
                Shape::unknown([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn in_shape_should_return_bool_on_named_input() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::Array(vec![WithRange::new(
                        LitExpr::Number(Number::from(42)),
                        None
                    ),]),
                    None
                )],
                Shape::name("a", [])
            ),
            Shape::bool([get_location()])
        );
    }
}
