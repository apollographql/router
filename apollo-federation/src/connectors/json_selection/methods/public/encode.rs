use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json_bytes::Value as JSON;
use shape::Shape;

use super::JsonStringifyMethod;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_type_name;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::json_selection::methods::ArrowMethodImpl;
use crate::connectors::json_selection::methods::common::Codec;
use crate::connectors::json_selection::methods::common::codec_arg;
use crate::connectors::json_selection::methods::common::codec_arg_shape;
use crate::connectors::json_selection::methods::common::string_input_shape_error;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(EncodeMethod, encode_method, encode_shape);
/// Encodes the input with the named codec (inverse of `->decode`).
///
/// $('Hello, World!')->encode('base64')      results in "SGVsbG8sIFdvcmxkIQ=="
/// $('Hello, World!')->encode('base64url')   results in "SGVsbG8sIFdvcmxkIQ"
/// $->echo({ "a": [1, 2] })->encode('json')  results in "{\"a\":[1,2]}"
///
/// "base64" uses the RFC 4648 §4 standard alphabet with padding, and
/// "base64url" uses the RFC 4648 §5 URL-safe alphabet without padding. JSON has
/// no byte type, so both encode the UTF-8 bytes of a string input. "json"
/// behaves exactly like `->jsonStringify` and accepts any input.
fn encode_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let (codec, mut errors) = codec_arg(method_name, method_args, data, vars, input_path, spec);
    let Some(codec) = codec else {
        return (None, errors);
    };

    let engine = match codec {
        Codec::Base64 => &STANDARD,
        Codec::Base64Url => &URL_SAFE_NO_PAD,
        Codec::Json => {
            let (result, json_errors) =
                JsonStringifyMethod.apply(method_name, None, data, vars, input_path, spec);
            errors.extend(json_errors);
            return (result, errors);
        }
    };

    let JSON::String(input) = data else {
        errors.push(ApplyToError::new(
            format!(
                "Method ->{} requires a string input, got {}",
                method_name.as_ref(),
                json_type_name(data)
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
        return (None, errors);
    };

    (
        Some(JSON::String(engine.encode(input.as_str()).into())),
        errors,
    )
}

fn encode_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let codec = match codec_arg_shape(
        context,
        method_name,
        method_args,
        &input_shape,
        &dollar_shape,
    ) {
        Ok(codec) => codec,
        Err(error) => return error,
    };

    match codec {
        Some(Codec::Base64 | Codec::Base64Url) => {
            string_input_shape_error(context, method_name, &input_shape)
                .unwrap_or_else(|| Shape::string(method_name.shape_location(context.source_id())))
        }
        Some(Codec::Json) => {
            JsonStringifyMethod.shape(context, method_name, None, input_shape, dollar_shape)
        }
        // Every codec encodes to a string, but only some require a string
        // input, so a codec known only at runtime leaves the input unchecked.
        None => Shape::string(method_name.shape_location(context.source_id())),
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    // --- RFC 4648 §10 test vectors ---

    #[rstest::rstest]
    #[case("", "")]
    #[case("f", "Zg==")]
    #[case("fo", "Zm8=")]
    #[case("foo", "Zm9v")]
    #[case("foob", "Zm9vYg==")]
    #[case("fooba", "Zm9vYmE=")]
    #[case("foobar", "Zm9vYmFy")]
    fn encode_base64_rfc_vectors(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(
            selection!("$->encode('base64')").apply_to(&json!(input)),
            (Some(json!(expected)), vec![]),
        );
    }

    #[rstest::rstest]
    #[case("", "")]
    #[case("f", "Zg")]
    #[case("fo", "Zm8")]
    #[case("foo", "Zm9v")]
    #[case("foob", "Zm9vYg")]
    #[case("fooba", "Zm9vYmE")]
    #[case("foobar", "Zm9vYmFy")]
    fn encode_base64url_rfc_vectors_are_unpadded(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(
            selection!("$->encode('base64url')").apply_to(&json!(input)),
            (Some(json!(expected)), vec![]),
        );
    }

    // --- Alphabets differ only in the characters for 62 and 63 ---

    #[rstest::rstest]
    #[case("base64", "PDw/Pz8+Pg==")]
    #[case("base64url", "PDw_Pz8-Pg")]
    fn encode_uses_the_named_alphabet(#[case] codec: &str, #[case] expected: &str) {
        assert_eq!(
            selection!(&format!("$->encode('{codec}')")).apply_to(&json!("<<???>>")),
            (Some(json!(expected)), vec![]),
        );
    }

    #[rstest::rstest]
    #[case("base64", "Y2Fmw6kg8J+OiQ==")]
    #[case("base64url", "Y2Fmw6kg8J-OiQ")]
    fn encode_uses_utf8_bytes(#[case] codec: &str, #[case] expected: &str) {
        assert_eq!(
            selection!(&format!("$->encode('{codec}')")).apply_to(&json!("café 🎉")),
            (Some(json!(expected)), vec![]),
        );
    }

    // --- The json codec is ->jsonStringify ---

    #[rstest::rstest]
    #[case(json!(null))]
    #[case(json!(true))]
    #[case(json!(42))]
    #[case(json!(10.8))]
    #[case(json!("hello \"world\""))]
    #[case(json!("café 🎉"))]
    #[case(json!([1, "two", null]))]
    #[case(json!({"key": "value", "nested": {"list": [1, 2]}}))]
    fn encode_json_matches_json_stringify(#[case] input: JSON) {
        let expected = selection!("$->jsonStringify").apply_to(&input);
        assert!(expected.1.is_empty());
        assert_eq!(selection!("$->encode('json')").apply_to(&input), expected);
    }

    // --- Codec argument ---

    #[test]
    fn encode_accepts_codec_from_data() {
        let data = json!({"text": "foo", "codec": "base64url"});
        assert_eq!(
            selection!("text->encode($.codec)").apply_to(&data),
            (Some(json!("Zm9v")), vec![]),
        );
    }

    #[test]
    fn encode_accepts_codec_from_variable() {
        let mut vars = IndexMap::default();
        vars.insert("$codec".to_string(), json!("base64"));
        assert_eq!(
            selection!("$->encode($codec)").apply_with_vars(&json!("f"), &vars),
            (Some(json!("Zg==")), vec![]),
        );
    }

    #[rstest::rstest]
    #[case(
        "$->encode",
        r#"requires a codec argument ("base64", "base64url", or "json")"#
    )]
    #[case(
        "$->encode()",
        r#"requires a codec argument ("base64", "base64url", or "json")"#
    )]
    #[case(
        "$->encode('base64', 'base64url')",
        "accepts exactly 1 argument (codec), got 2"
    )]
    #[case("$->encode(42)", "requires a string codec argument, got number")]
    #[case("$->encode(null)", "requires a string codec argument, got null")]
    #[case(
        "$->encode('hex')",
        r#"does not support codec "hex", expected "base64", "base64url", or "json""#
    )]
    #[case(
        "$->encode('BASE64')",
        r#"does not support codec "BASE64", expected "base64", "base64url", or "json""#
    )]
    fn encode_should_error_on_bad_codec_argument(#[case] selection: &str, #[case] message: &str) {
        let (result, errors) = selection!(selection).apply_to(&json!("foo"));
        assert_eq!(result, None);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message(), format!("Method ->encode {message}"));
    }

    #[test]
    fn encode_should_error_on_missing_codec_path() {
        let (result, errors) = selection!("$->encode($.codec)").apply_to(&json!("foo"));
        assert_eq!(result, None);
        assert!(
            errors.iter().any(|e| e.message()
                == "Method ->encode requires a string codec argument, but received null"),
            "{errors:?}"
        );
    }

    // --- The base64 codecs need a string input ---

    #[rstest::rstest]
    #[case(json!(42), "number")]
    #[case(json!(true), "boolean")]
    #[case(json!(null), "null")]
    #[case(json!([1, 2, 3]), "array")]
    #[case(json!({"key": "value"}), "object")]
    fn encode_base64_should_error_on_non_string_input(
        #[case] input: JSON,
        #[case] expected_type: &str,
        #[values("base64", "base64url")] codec: &str,
    ) {
        let (result, errors) = selection!(&format!("$->encode('{codec}')")).apply_to(&input);
        assert_eq!(result, None);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].message(),
            format!("Method ->encode requires a string input, got {expected_type}")
        );
    }

    #[test]
    fn encode_from_data_property() {
        let data = json!({"credentials": "alice:s3cret"});
        assert_eq!(
            selection!("basic: credentials->encode('base64')").apply_to(&data),
            (Some(json!({"basic": "YWxpY2U6czNjcmV0"})), vec![]),
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
            span: 0..6,
        }
    }

    fn get_shape(codec: &str, input: Shape) -> Shape {
        let location = get_location();
        encode_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("encode".to_string(), Some(location.span)),
            Some(&MethodArgs {
                args: vec![WithRange::new(LitExpr::String(codec.to_string()), None)],
                range: None,
            }),
            input,
            Shape::none(),
        )
    }

    #[rstest::rstest]
    #[case(Shape::string([]))]
    #[case(Shape::string_value("abc", []))]
    #[case(Shape::unknown([]))]
    #[case(Shape::name("a", []))]
    fn encode_base64_shape_returns_string_for_string_input(
        #[case] input: Shape,
        #[values("base64", "base64url")] codec: &str,
    ) {
        assert_eq!(get_shape(codec, input), Shape::string([get_location()]));
    }

    #[rstest::rstest]
    #[case(Shape::int([]))]
    #[case(Shape::bool([]))]
    #[case(Shape::null([]))]
    #[case(Shape::empty_object([]))]
    #[case(Shape::list(Shape::string([]), []))]
    fn encode_base64_shape_errors_for_non_string_input(
        #[case] input: Shape,
        #[values("base64", "base64url")] codec: &str,
    ) {
        assert_eq!(
            get_shape(codec, input),
            Shape::error(
                "Method ->encode requires a string input".to_string(),
                [get_location()]
            )
        );
    }

    #[rstest::rstest]
    #[case(Shape::string([]))]
    #[case(Shape::int([]))]
    #[case(Shape::null([]))]
    #[case(Shape::empty_object([]))]
    #[case(Shape::list(Shape::string([]), []))]
    #[case(Shape::unknown([]))]
    fn encode_json_shape_matches_json_stringify_shape(#[case] input: Shape) {
        let location = get_location();
        let expected = JsonStringifyMethod.shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("encode".to_string(), Some(location.span)),
            None,
            input.clone(),
            Shape::none(),
        );
        assert_eq!(get_shape("json", input), expected);
        assert_eq!(expected, Shape::string([get_location()]));
    }

    #[test]
    fn encode_shape_errors_for_unsupported_codec() {
        assert_eq!(
            get_shape("url", Shape::string([])),
            Shape::error(
                r#"Method ->encode does not support codec "url", expected "base64", "base64url", or "json""#
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn encode_shape_returns_string_for_runtime_codec() {
        let location = get_location();
        assert_eq!(
            encode_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("encode".to_string(), Some(location.span)),
                Some(&MethodArgs {
                    args: vec![WithRange::new(
                        LitExpr::Path(crate::connectors::PathSelection {
                            path: crate::connectors::json_selection::PathList::Key(
                                crate::connectors::Key::field("codec").into_with_range(),
                                crate::connectors::json_selection::PathList::Empty
                                    .into_with_range(),
                            )
                            .into_with_range(),
                        }),
                        None,
                    )],
                    range: None,
                }),
                Shape::empty_object([]),
                Shape::record([("codec".to_string(), Shape::string([]))].into(), []),
            ),
            Shape::string([get_location()])
        );
    }
}
