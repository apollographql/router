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

impl_arrow_method!(HasMethod, has_method, has_shape);
/// TODO: Split this into hasIndex and hasProperty on a separate PR
fn has_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(arg) = method_args.and_then(|MethodArgs { args, .. }| args.first()) else {
        return (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires an argument", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };
    match arg.apply_to_path(data, vars, input_path, spec) {
        (Some(JSON::Number(ref n)), arg_errors) => {
            match (data, n.as_i64()) {
                (JSON::Array(array), Some(index)) => {
                    let ilen = array.len() as i64;
                    // Negative indices count from the end of the array
                    let index = if index < 0 { ilen + index } else { index };
                    (Some(JSON::Bool(index >= 0 && index < ilen)), arg_errors)
                }

                (JSON::String(s), Some(index)) => {
                    let ilen = s.as_str().len() as i64;
                    // Negative indices count from the end of the array
                    let index = if index < 0 { ilen + index } else { index };
                    (Some(JSON::Bool(index >= 0 && index < ilen)), arg_errors)
                }

                _ => (Some(JSON::Bool(false)), arg_errors),
            }
        }

        (Some(JSON::String(ref s)), arg_errors) => match data {
            JSON::Object(map) => (Some(JSON::Bool(map.contains_key(s.as_str()))), arg_errors),
            _ => (Some(JSON::Bool(false)), arg_errors),
        },

        (_, arg_errors) => (Some(JSON::Bool(false)), arg_errors),
    }
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn has_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    // TODO We could be more clever here (sometimes) based on the input_shape
    // and argument shapes.
    Shape::bool(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn has_should_return_true_when_array_has_item_at_specified_index() {
        assert_eq!(
            selection!("$->has(1)").apply_to(&json!([1, 2, 3])),
            (Some(json!(true)), vec![]),
        );
    }

    #[test]
    fn has_should_return_false_when_array_does_not_have_item_at_specified_index() {
        assert_eq!(
            selection!("$->has(5)").apply_to(&json!([1, 2, 3])),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn has_should_return_true_when_string_has_character_at_specified_index() {
        assert_eq!(
            selection!("$->has(2)").apply_to(&json!("oyez")),
            (Some(json!(true)), vec![]),
        );
    }

    #[test]
    fn has_should_return_true_when_string_has_character_at_specified_negative_index() {
        assert_eq!(
            selection!("$->has(-2)").apply_to(&json!("oyez")),
            (Some(json!(true)), vec![]),
        );
    }

    #[test]
    fn has_should_return_false_when_string_does_not_have_character_at_specified_negative_index() {
        assert_eq!(
            selection!("$->has(10)").apply_to(&json!("oyez")),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn has_should_return_true_when_object_has_specified_property() {
        assert_eq!(
            selection!("object->has('a')").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(true)), vec![]),
        );
    }

    #[test]
    fn has_should_return_false_when_object_does_not_have_specified_property() {
        assert_eq!(
            selection!("object->has('c')").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn has_should_return_false_when_trying_to_access_boolean_property_name() {
        assert_eq!(
            selection!("object->has(true)").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn has_should_return_false_when_trying_to_access_null_property_name() {
        assert_eq!(
            selection!("object->has(null)").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(false)), vec![]),
        );
    }

    #[test]
    fn has_should_return_boolean_type() {
        assert_eq!(
            selection!("object->has('xxx')->typeof").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!("boolean")), vec![]),
        );
    }
}
