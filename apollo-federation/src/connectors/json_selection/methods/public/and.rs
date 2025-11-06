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
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(AndMethod, and_method, and_shape);
/// Given 2 or more values to compare, returns true if all of the values are true.
///
/// Examples:
/// $(true)->and(false)            results in false
/// $(false)->and(true)            results in false
/// $(true)->and(true)             results in true
/// $(false)->and(false)           results in false
/// $(true)->and(false, true)      results in false
fn and_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(mut result) = data.as_bool() else {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} can only be applied to boolean values.",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    let Some(MethodArgs { args, .. }) = method_args else {
        return (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires arguments", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    let mut errors = Vec::new();
    for arg in args {
        if !result {
            break;
        }
        let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path, spec);
        errors.extend(arg_errors);

        match value_opt {
            Some(JSON::Bool(value)) => result = result && value,
            Some(_) => {
                errors.extend(vec![ApplyToError::new(
                    format!(
                        "Method ->{} can only accept boolean arguments.",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    arg.range(),
                    spec,
                )]);
            }
            None => {
                return (None, errors);
            }
        }
    }

    (Some(JSON::Bool(result)), errors)
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn and_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    if method_args.and_then(|args| args.args.first()).is_none() {
        return Shape::error(
            format!(
                "Method ->{} requires at least one argument",
                method_name.as_ref()
            ),
            method_name.shape_location(context.source_id()),
        );
    };

    // We will accept anything bool-like OR unknown/named
    if !(Shape::bool([]).accepts(&input_shape) || input_shape.accepts(&Shape::unknown([]))) {
        return Shape::error(
            format!(
                "Method ->{} can only be applied to boolean values. Got {input_shape}.",
                method_name.as_ref()
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    if let Some(MethodArgs { args, .. }) = method_args {
        for (i, arg) in args.iter().enumerate() {
            let arg_shape =
                arg.compute_output_shape(context, input_shape.clone(), dollar_shape.clone());

            // We will accept anything bool-like OR unknown/named
            if !(Shape::bool([]).accepts(&arg_shape) || arg_shape.accepts(&Shape::unknown([]))) {
                return Shape::error(
                    format!(
                        "Method ->{} can only accept boolean arguments. Got {arg_shape} at position {i}.",
                        method_name.as_ref()
                    ),
                    method_name.shape_location(context.source_id()),
                );
            }
        }
    }

    Shape::bool(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::json;

    use crate::connectors::ConnectSpec;
    use crate::connectors::json_selection::ApplyToError;
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
    fn and_should_return_false_when_first_is_false_second_is_true() {
        assert_eq!(
            selection!("$.first->and($.second)").apply_to(&json!({
                "first": false,
                "second": true,
            })),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn and_should_return_false_when_both_values_are_false() {
        assert_eq!(
            selection!("$.first->and($.second)").apply_to(&json!({
                "first": false,
                "second": false,
            })),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn and_should_return_error_when_arguments_are_not_boolean() {
        let result = selection!("$.a->and($.b, $.c)").apply_to(&json!({
            "a": true,
            "b": null,
            "c": 0,
        }));

        assert_eq!(result.0, Some(json!(true)));
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->and can only accept boolean arguments.")
        );
    }
    #[test]
    fn and_should_return_error_when_applied_to_non_boolean() {
        let result = selection!("$.b->and($.a, $.c)").apply_to(&json!({
            "a": false,
            "b": null,
            "c": 0,
        }));

        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->and can only be applied to boolean values.")
        );
    }

    #[test]
    fn and_should_return_error_when_no_arguments_provided() {
        let result = selection!("$.a->and").apply_to(&json!({
            "a": true,
        }));

        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->and requires arguments")
        );
    }

    #[rstest::rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn and_should_return_none_when_argument_evaluates_to_none(#[case] spec: ConnectSpec) {
        assert_eq!(
            selection!("$.a->and($.missing)", spec).apply_to(&json!({
                "a": true,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .missing not found in object",
                    "path": ["missing"],
                    "range": [11, 18],
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
            span: 0..7,
        }
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        and_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("and".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::none(),
        )
    }

    #[test]
    fn and_shape_should_return_bool_on_valid_booleans() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(false), None)],
                Shape::bool([])
            ),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn and_shape_should_error_on_non_boolean_input() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                Shape::string([])
            ),
            Shape::error(
                "Method ->and can only be applied to boolean values. Got String.".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn and_shape_should_error_on_non_boolean_args() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("test".to_string()), None)],
                Shape::bool([])
            ),
            Shape::error(
                "Method ->and can only accept boolean arguments. Got \"test\" at position 0."
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn and_shape_should_error_on_no_args() {
        assert_eq!(
            get_shape(vec![], Shape::bool([])),
            Shape::error(
                "Method ->and requires at least one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn and_shape_should_error_on_none_args() {
        let location = get_location();
        assert_eq!(
            and_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("and".to_string(), Some(location.span)),
                None,
                Shape::bool([]),
                Shape::none(),
            ),
            Shape::error(
                "Method ->and requires at least one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn and_shape_should_error_on_args_that_compute_as_none() {
        let path = LitExpr::Path(PathSelection {
            path: PathList::Key(
                Key::field("a").into_with_range(),
                PathList::Empty.into_with_range(),
            )
            .into_with_range(),
        });
        let location = get_location();
        assert_eq!(
            and_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("and".to_string(), Some(location.span)),
                Some(&MethodArgs {
                    args: vec![path.into_with_range()],
                    range: None
                }),
                Shape::bool([]),
                Shape::none(),
            ),
            Shape::error(
                "Method ->and can only accept boolean arguments. Got None at position 0."
                    .to_string(),
                [get_location()]
            )
        );
    }
}
