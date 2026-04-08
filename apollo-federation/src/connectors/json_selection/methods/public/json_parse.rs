use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_type_name;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(JsonParseMethod, json_parse_method, json_parse_shape);
/// Parses a JSON string into a structured value (inverse of `->jsonStringify`)
///
/// $('{"key":"value"}')->jsonParse              results in { "key": "value" }
/// $("42")->jsonParse                          results in 42
/// $("true")->jsonParse                        results in true
/// $("null")->jsonParse                        results in null
/// $("[1,2,3]")->jsonParse                     results in [1, 2, 3]
/// $('"hello"')->jsonParse                     results in "hello"
/// $->jsonStringify->jsonParse                 round-trips back to the original value
fn json_parse_method(
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

    match data {
        JSON::String(s) => match serde_json::from_str::<JSON>(s.as_str()) {
            Ok(parsed) => (Some(parsed), Vec::new()),
            Err(err) => (
                None,
                vec![ApplyToError::new(
                    format!(
                        "Method ->{} failed to parse JSON string: {}",
                        method_name.as_ref(),
                        err
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                )],
            ),
        },
        _ => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires a string input, got {}",
                    method_name.as_ref(),
                    json_type_name(data)
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        ),
    }
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn json_parse_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    // The output shape of jsonParse is unknown at static analysis time,
    // since the parsed value could be any JSON type.
    Shape::unknown(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;
    use serde_json_bytes::json;

    use super::*;
    use crate::connectors::ApplyToError;
    use crate::selection;

    // --- Primitive types ---

    #[rstest::rstest]
    #[case(json!("null"), json!(null), vec![])]
    #[case(json!("true"), json!(true), vec![])]
    #[case(json!("false"), json!(false), vec![])]
    #[case(json!("42"), json!(42), vec![])]
    #[case(json!("0"), json!(0), vec![])]
    #[case(json!("-1"), json!(-1), vec![])]
    #[case(json!("-99"), json!(-99), vec![])]
    #[case(json!("10.8"), json!(10.8), vec![])]
    #[case(json!("0.0"), json!(0.0), vec![])]
    #[case(json!("-1.5"), json!(-1.5), vec![])]
    #[case(json!("1e10"), json!(1e10), vec![])]
    #[case(json!("2.5e-3"), json!(2.5e-3), vec![])]
    #[case(json!("\"hello world\""), json!("hello world"), vec![])]
    #[case(json!("\"\""), json!(""), vec![])]
    #[case(json!("\"with \\\"escaped\\\" quotes\""), json!("with \"escaped\" quotes"), vec![])]
    #[case(json!("\"line\\nbreak\""), json!("line\nbreak"), vec![])]
    #[case(json!("\"tab\\there\""), json!("tab\there"), vec![])]
    #[case(json!("\"back\\\\slash\""), json!("back\\slash"), vec![])]
    #[case(json!("\"unicode \\u0041\""), json!("unicode A"), vec![])]
    fn json_parse_should_parse_primitives(
        #[case] input: JSON,
        #[case] expected: JSON,
        #[case] errors: Vec<ApplyToError>,
    ) {
        assert_eq!(
            selection!("$->jsonParse").apply_to(&input),
            (Some(expected), errors),
        );
    }

    // --- Arrays ---

    #[rstest::rstest]
    #[case(json!("[]"), json!([]), vec![])]
    #[case(json!("[1,2,3]"), json!([1, 2, 3]), vec![])]
    #[case(json!("[1, 2, 3]"), json!([1, 2, 3]), vec![])]
    #[case(json!("[1,\"two\",true,null]"), json!([1, "two", true, null]), vec![])]
    #[case(json!("[[1,2],[3,4]]"), json!([[1, 2], [3, 4]]), vec![])]
    #[case(json!("[{\"a\":1},{\"b\":2}]"), json!([{"a": 1}, {"b": 2}]), vec![])]
    #[case(json!("[null,null,null]"), json!([null, null, null]), vec![])]
    fn json_parse_should_parse_arrays(
        #[case] input: JSON,
        #[case] expected: JSON,
        #[case] errors: Vec<ApplyToError>,
    ) {
        assert_eq!(
            selection!("$->jsonParse").apply_to(&input),
            (Some(expected), errors),
        );
    }

    // --- Objects ---

    #[rstest::rstest]
    #[case(json!("{}"), json!({}), vec![])]
    #[case(json!("{\"key\":\"value\"}"), json!({"key": "value"}), vec![])]
    #[case(json!("{\"a\":1,\"b\":2,\"c\":3}"), json!({"a": 1, "b": 2, "c": 3}), vec![])]
    #[case(json!("{\"nested\":{\"deep\":{\"value\":true}}}"), json!({"nested": {"deep": {"value": true}}}), vec![])]
    #[case(json!("{\"arr\":[1,2,3],\"obj\":{\"k\":\"v\"}}"), json!({"arr": [1, 2, 3], "obj": {"k": "v"}}), vec![])]
    #[case(json!("{\"empty_arr\":[],\"empty_obj\":{}}"), json!({"empty_arr": [], "empty_obj": {}}), vec![])]
    #[case(json!("{\"null_val\":null,\"bool_val\":false}"), json!({"null_val": null, "bool_val": false}), vec![])]
    fn json_parse_should_parse_objects(
        #[case] input: JSON,
        #[case] expected: JSON,
        #[case] errors: Vec<ApplyToError>,
    ) {
        assert_eq!(
            selection!("$->jsonParse").apply_to(&input),
            (Some(expected), errors),
        );
    }

    // --- Whitespace handling ---

    #[rstest::rstest]
    #[case(json!(" 42 "), json!(42), vec![])]
    #[case(json!("\t42\t"), json!(42), vec![])]
    #[case(json!("\n42\n"), json!(42), vec![])]
    #[case(json!("  true  "), json!(true), vec![])]
    #[case(json!("  null  "), json!(null), vec![])]
    #[case(json!(" { \"key\" : \"value\" } "), json!({"key": "value"}), vec![])]
    #[case(json!(" [ 1 , 2 , 3 ] "), json!([1, 2, 3]), vec![])]
    #[case(json!("\n{\n  \"a\": 1,\n  \"b\": 2\n}\n"), json!({"a": 1, "b": 2}), vec![])]
    fn json_parse_should_handle_leading_and_trailing_whitespace(
        #[case] input: JSON,
        #[case] expected: JSON,
        #[case] errors: Vec<ApplyToError>,
    ) {
        assert_eq!(
            selection!("$->jsonParse").apply_to(&input),
            (Some(expected), errors),
        );
    }

    // --- Error cases: invalid JSON strings ---

    #[rstest::rstest]
    #[case(json!("not valid json"))]
    #[case(json!(""))]
    #[case(json!("{"))]
    #[case(json!("["))]
    #[case(json!("{\"key\":}"))]
    #[case(json!("[1,2,]"))]
    #[case(json!("'single quotes'"))]
    #[case(json!("undefined"))]
    #[case(json!("{key: value}"))]
    fn json_parse_should_error_on_invalid_json(#[case] input: JSON) {
        let result = selection!("$->jsonParse").apply_to(&input);
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains("failed to parse JSON string")
        );
    }

    // --- Error cases: non-string input types ---

    #[rstest::rstest]
    #[case(json!(42), "number")]
    #[case(json!(1.5), "number")]
    #[case(json!(true), "boolean")]
    #[case(json!(false), "boolean")]
    #[case(json!(null), "null")]
    #[case(json!([1, 2, 3]), "array")]
    #[case(json!({"key": "value"}), "object")]
    fn json_parse_should_error_on_non_string_input(
        #[case] input: JSON,
        #[case] expected_type: &str,
    ) {
        let result = selection!("$->jsonParse").apply_to(&input);
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains(&format!("requires a string input, got {expected_type}"))
        );
    }

    // --- Error case: arguments not accepted ---

    #[test]
    fn json_parse_should_error_when_provided_argument() {
        assert_eq!(
            selection!("$->jsonParse(1)").apply_to(&json!("null")),
            (
                None,
                vec![ApplyToError::new(
                    "Method ->jsonParse does not take any arguments".to_string(),
                    vec![json!("->jsonParse")],
                    Some(3..12),
                    ConnectSpec::latest(),
                )],
            ),
        );
    }

    // --- Round-trip tests ---

    #[rstest::rstest]
    #[case(json!({ "key": [1, "two", true, null] }))]
    #[case(json!(42))]
    #[case(json!("hello"))]
    #[case(json!(true))]
    #[case(json!(null))]
    #[case(json!([1, 2, 3]))]
    #[case(json!({ "nested": { "deep": [1, { "x": true }] } }))]
    fn json_stringify_then_json_parse_roundtrip(#[case] original: JSON) {
        assert_eq!(
            selection!("$->jsonStringify->jsonParse").apply_to(&original),
            (Some(original), vec![]),
        );
    }

    // --- Variable-based tests ---

    #[test]
    fn json_parse_from_variable() {
        let mut vars = IndexMap::default();
        vars.insert(
            "$encoded".to_string(),
            json!("{\"id\":123,\"name\":\"Alice\"}"),
        );
        assert_eq!(
            selection!("$encoded->jsonParse").apply_with_vars(&json!({}), &vars),
            (Some(json!({"id": 123, "name": "Alice"})), vec![]),
        );
    }

    #[test]
    fn json_parse_from_variable_property() {
        let mut vars = IndexMap::default();
        vars.insert(
            "$response".to_string(),
            json!({"body": "{\"status\":\"ok\",\"count\":42}"}),
        );
        assert_eq!(
            selection!("$response.body->jsonParse").apply_with_vars(&json!({}), &vars),
            (Some(json!({"status": "ok", "count": 42})), vec![]),
        );
    }

    #[test]
    fn json_parse_from_nested_variable_property() {
        let mut vars = IndexMap::default();
        vars.insert("$data".to_string(), json!({"outer": {"inner": "[1,2,3]"}}));
        assert_eq!(
            selection!("$data.outer.inner->jsonParse").apply_with_vars(&json!({}), &vars),
            (Some(json!([1, 2, 3])), vec![]),
        );
    }

    #[test]
    fn json_parse_from_data_property() {
        let data = json!({"payload": "{\"key\":\"value\"}"});
        assert_eq!(
            selection!("payload->jsonParse").apply_to(&data),
            (Some(json!({"key": "value"})), vec![]),
        );
    }

    #[test]
    fn json_parse_from_nested_data_property() {
        let data = json!({"response": {"encoded": "true"}});
        assert_eq!(
            selection!("response.encoded->jsonParse").apply_to(&data),
            (Some(json!(true)), vec![]),
        );
    }

    #[test]
    fn json_parse_then_select_into_parsed_result() {
        let data = json!({"payload": "{\"users\":[{\"name\":\"Alice\"},{\"name\":\"Bob\"}]}"});
        assert_eq!(
            selection!("payload->jsonParse { users { name } }").apply_to(&data),
            (
                Some(json!({"users": [{"name": "Alice"}, {"name": "Bob"}]})),
                vec![],
            ),
        );
    }

    #[test]
    fn json_parse_variable_with_roundtrip() {
        let mut vars = IndexMap::default();
        vars.insert("$input".to_string(), json!({"x": 1, "y": 2}));
        assert_eq!(
            selection!("$input->jsonStringify->jsonParse").apply_with_vars(&json!({}), &vars),
            (Some(json!({"x": 1, "y": 2})), vec![]),
        );
    }
}
