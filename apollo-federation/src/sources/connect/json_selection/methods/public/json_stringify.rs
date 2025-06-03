use crate::sources::connect::json_selection::safe_json::Value as JSON;
use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

impl_arrow_method!(
    JsonStringifyMethod,
    json_stringify_method,
    json_stringify_shape
);
/// Returns a string representation of a structure
/// The simplest possible example:
///
///    
/// $->echo({ "key": "value" })->jsonStringify     would result in "{\"key\":\"value\"}"
fn json_stringify_method(
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

    match serde_json::to_string(data) {
        Ok(val) => (Some(JSON::String(val.into())), Vec::new()),
        Err(err) => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} failed to serialize JSON: {}",
                    method_name.as_ref(),
                    err
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        ),
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn json_stringify_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    Shape::string(method_name.shape_location(source_id))
}

#[cfg(test)]
mod tests {
    mod json_stringify {
        use serde_json_bytes::json;

        use crate::selection;
        use crate::sources::connect::ApplyToError;

        #[test]
        fn should_stringify_null() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!(null)),
                (Some(json!("null")), Vec::new()),
            );
        }

        #[test]
        fn should_stringify_true() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!(true)),
                (Some(json!("true")), Vec::new()),
            );
        }

        #[test]
        fn should_stringify_false() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!(false)),
                (Some(json!("false")), Vec::new()),
            );
        }

        #[test]
        fn should_stringify_integer() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!(42)),
                (Some(json!("42")), Vec::new()),
            );
        }

        #[test]
        fn should_stringify_float() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!(10.8)),
                (Some(json!("10.8")), Vec::new()),
            );
        }

        #[test]
        fn should_stringify_string() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!("hello world")),
                (Some(json!("\"hello world\"")), Vec::new()),
            );
        }

        #[test]
        fn should_stringify_array() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!([1, 2, 3])),
                (Some(json!("[1, 2, 3]")), Vec::new()),
            );
        }

        #[test]
        fn should_stringify_object() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!({ "key": "value" })),
                (Some(json!("{ \"key\": \"value\" }")), Vec::new()),
            );
        }

        #[test]
        fn should_stringify_complex() {
            assert_eq!(
                selection!("$->jsonStringify").apply_to(&json!([1, "two", true, null])),
                (Some(json!("[1, \"two\", true, null]")), Vec::new()),
            );
        }

        #[test]
        fn should_error_when_provided_argument() {
            assert_eq!(
                selection!("$->jsonStringify(1)").apply_to(&json!(null)),
                (
                    None,
                    vec![ApplyToError::new(
                        "Method ->jsonStringify does not take any arguments".to_string(),
                        vec![json!("->jsonStringify").into()],
                        Some(3..16)
                    )]
                ),
            );
        }
    }
}
