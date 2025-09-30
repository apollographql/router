use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(NotMethod, not_method, not_shape);
/// Given a boolean value, returns the logical negation of that value.
///
/// Examples:
/// $(true)->not               results in false
/// $(false)->not              results in true
fn not_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if method_args.is_some() {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} does not take any arguments",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    }

    let Some(value) = data.as_bool() else {
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

    (Some(JSON::Bool(!value)), vec![])
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn not_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    if method_args.is_some() {
        return Shape::error(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.shape_location(context.source_id()),
        );
    }

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

    Shape::bool(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn not_should_negate_true() {
        assert_eq!(
            selection!("$.value->not").apply_to(&json!({
                "value": true,
            })),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn not_should_negate_false() {
        assert_eq!(
            selection!("$.value->not").apply_to(&json!({
                "value": false,
            })),
            (Some(json!(true)), vec![]),
        );
    }

    #[test]
    fn not_should_return_error_when_applied_to_non_boolean() {
        let result = selection!("$.value->not").apply_to(&json!({
            "value": "hello",
        }));

        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->not can only be applied to boolean values.")
        );
    }

    #[test]
    fn not_should_return_error_when_arguments_provided() {
        let result = selection!("$.value->not(true)").apply_to(&json!({
            "value": true,
        }));

        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->not does not take any arguments")
        );
    }

    #[test]
    fn not_should_work_with_double_negation() {
        assert_eq!(
            selection!("$.value->not->not").apply_to(&json!({
                "value": true,
            })),
            (Some(json!(true)), vec![]),
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
            span: 0..7,
        }
    }

    fn get_shape(args: Option<&MethodArgs>, input: Shape) -> Shape {
        let location = get_location();
        not_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("not".to_string(), Some(location.span)),
            args,
            input,
            Shape::none(),
        )
    }

    #[test]
    fn not_shape_should_return_bool_on_valid_boolean() {
        assert_eq!(
            get_shape(None, Shape::bool([])),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn not_shape_should_error_on_non_boolean_input() {
        assert_eq!(
            get_shape(None, Shape::string([])),
            Shape::error(
                "Method ->not can only be applied to boolean values. Got String.".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn not_shape_should_error_on_args_provided() {
        assert_eq!(
            get_shape(
                Some(&MethodArgs {
                    args: vec![WithRange::new(LitExpr::Bool(true), None)],
                    range: None
                }),
                Shape::bool([])
            ),
            Shape::error(
                "Method ->not does not take any arguments".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn not_shape_should_accept_unknown_input() {
        assert_eq!(
            get_shape(None, Shape::unknown([])),
            Shape::bool([get_location()])
        );
    }

    #[test]
    fn not_shape_should_accept_name_input() {
        assert_eq!(
            get_shape(None, Shape::name("a", [])),
            Shape::bool([get_location()])
        );
    }
}
