use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

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
                spec,
            )],
        ),
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn json_stringify_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    Shape::string(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::connectors::ApplyToError;
    use crate::selection;

    #[rstest::rstest]
    #[case(json!(null), json!("null"), vec![])]
    #[case(json!(true), json!("true"), vec![])]
    #[case(json!(false), json!("false"), vec![])]
    #[case(json!(42), json!("42"), vec![])]
    #[case(json!(10.8), json!("10.8"), vec![])]
    #[case(json!("hello world"), json!("\"hello world\""), vec![])]
    #[case(json!([1, 2, 3]), json!("[1,2,3]"), vec![])]
    #[case(json!({ "key": "value" }), json!("{\"key\":\"value\"}"), vec![])]
    #[case(json!([1, "two", true, null]), json!("[1,\"two\",true,null]"), vec![])]
    fn json_stringify_should_stringify_various_structures(
        #[case] input: JSON,
        #[case] expected: JSON,
        #[case] errors: Vec<ApplyToError>,
    ) {
        assert_eq!(
            selection!("$->jsonStringify").apply_to(&input),
            (Some(expected), errors),
        );
    }

    #[test]
    fn json_stringify_should_error_when_provided_argument() {
        assert_eq!(
            selection!("$->jsonStringify(1)").apply_to(&json!(null)),
            (
                None,
                vec![ApplyToError::new(
                    "Method ->jsonStringify does not take any arguments".to_string(),
                    vec![json!("->jsonStringify")],
                    Some(3..16),
                    ConnectSpec::latest(),
                )],
            ),
        );
    }
}
