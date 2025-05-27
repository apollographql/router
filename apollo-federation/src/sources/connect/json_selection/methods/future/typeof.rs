use crate::sources::connect::json_selection::safe_json::Value as JSON;
use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::helpers::json_type_name;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

impl_arrow_method!(TypeOfMethod, typeof_method, typeof_shape);
/// Given a JSON structure, returns a string representing the "type"
///
/// Some examples:
/// $->echo(true)       would result in "boolean"
/// $->echo([1, 2, 3])       would result in "array"
/// $->echo("hello")       would result in "string"
/// $->echo(5)       would result in "number"
fn typeof_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if method_args.is_some() {
        (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} does not take any arguments",
                    method_name.as_ref()
                ),
                input_path
                    .to_vec()
                    .into_iter()
                    .map(|safe_json| safe_json.into())
                    .collect(),
                method_name.range(),
            )],
        )
    } else {
        let typeof_string = JSON::String(json_type_name(data).to_string().into());
        (Some(typeof_string), Vec::new())
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn typeof_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    // TODO Compute this union type once and clone it here.
    let locations = method_name.shape_location(source_id);
    Shape::one(
        [
            Shape::string_value("null", locations.clone()),
            Shape::string_value("boolean", locations.clone()),
            Shape::string_value("number", locations.clone()),
            Shape::string_value("string", locations.clone()),
            Shape::string_value("array", locations.clone()),
            Shape::string_value("object", locations.clone()),
        ],
        locations,
    )
}

#[cfg(test)]
mod tests {
    mod type_of {
        use crate::selection;
        use serde_json_bytes::json;

        #[test]
        fn should_return_typeof_null() {
            assert_eq!(
                selection!("$->typeof").apply_to(&json!(null)),
                (Some(json!("null")), Vec::new()),
            );
        }
        #[test]
        fn should_return_typeof_true() {
            assert_eq!(
                selection!("$->typeof").apply_to(&json!(true)),
                (Some(json!("boolean")), Vec::new()),
            );
        }
        #[test]
        fn should_return_typeof_false() {
            assert_eq!(
                selection!("@->typeof").apply_to(&json!(false)),
                (Some(json!("boolean")), Vec::new()),
            );
        }
        #[test]
        fn should_return_typeof_int() {
            assert_eq!(
                selection!("$->typeof").apply_to(&json!(123)),
                (Some(json!("number")), Vec::new()),
            );
        }
        #[test]
        fn should_return_typeof_float() {
            assert_eq!(
                selection!("$->typeof").apply_to(&json!(123.45)),
                (Some(json!("number")), Vec::new()),
            );
        }
        #[test]
        fn should_return_typeof_string() {
            assert_eq!(
                selection!("$->typeof").apply_to(&json!("hello")),
                (Some(json!("string")), Vec::new()),
            );
        }
        #[test]
        fn should_return_typeof_array() {
            assert_eq!(
                selection!("$->typeof").apply_to(&json!([1, 2, 3])),
                (Some(json!("array")), Vec::new()),
            );
        }
        #[test]
        fn should_return_typeof_object() {
            assert_eq!(
                selection!("$->typeof").apply_to(&json!({ "key": "value" })),
                (Some(json!("object")), Vec::new()),
            );
        }
    }
}
