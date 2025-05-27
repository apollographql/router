use std::iter::empty;

use crate::sources::connect::json_selection::helpers::json_type_name;
use crate::sources::connect::json_selection::safe_json::Value as JSON;
use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

impl_arrow_method!(KeysMethod, keys_method, keys_shape);
/// Given an object, returns an array of its keys (aka properties).
/// Simple example:
///
/// $->echo({"a": 1, "b": 2, "c": 3})       returns ["a", "b", "c"]
fn keys_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
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
            )],
        );
    }

    match data {
        JSON::Object(map) => {
            let keys = map.keys().map(|key| JSON::String(key.clone())).collect();
            (Some(JSON::Array(keys)), vec![])
        }
        _ => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an object input, not {}",
                    method_name.as_ref(),
                    json_type_name(data),
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        ),
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn keys_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Object { fields, rest, .. } => {
            // Any statically known field names become string literal shapes in
            // the resulting keys array.
            let keys_vec = fields
                .keys()
                .map(|key| Shape::string_value(key.as_str(), empty()))
                .collect::<Vec<_>>();

            Shape::array(
                keys_vec,
                // Since we're collecting key shapes, we want String for the
                // rest shape when it's not None.
                if rest.is_none() {
                    Shape::none()
                } else {
                    Shape::string(empty())
                },
                empty(),
            )
        }
        _ => Shape::error(
            "Method ->keys requires an object input",
            method_name.shape_location(source_id),
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    #[test]
    fn keys_should_return_array_of_keys_of_an_object() {
        assert_eq!(
            selection!("$->keys").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(["a", "b", "c"])), vec![]),
        );
    }

    #[test]
    fn keys_should_return_empty_array_on_empty_object() {
        assert_eq!(
            selection!("$->keys").apply_to(&json!({})),
            (Some(json!([])), vec![]),
        );
    }

    #[test]
    fn keys_should_error_when_applied_to_non_object() {
        assert_eq!(
            selection!("notAnObject->keys").apply_to(&json!({
                "notAnObject": 123,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->keys requires an object input, not number",
                    "path": ["notAnObject", "->keys"],
                    "range": [13, 17],
                }))]
            ),
        );
    }
}
