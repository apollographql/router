use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;
use base64::engine::general_purpose::URL_SAFE_PAD_INDIFFERENT;
use serde_json_bytes::Value as JSON;
use shape::Shape;

use super::JsonParseMethod;
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

impl_arrow_method!(DecodeMethod, decode_method, decode_shape);
/// Decodes a string with the named codec (inverse of `->encode`).
///
/// $('SGVsbG8sIFdvcmxkIQ==')->decode('base64')     results in "Hello, World!"
/// $('SGVsbG8sIFdvcmxkIQ')->decode('base64url')    results in "Hello, World!"
/// $('{"a":[1,2]}')->decode('json')                results in { "a": [1, 2] }
///
/// The base64 codecs are lenient about the variant: "base64" and "base64url"
/// each accept the standard or URL-safe alphabet, with or without padding.
/// JSON has no byte type, so they return the decoded bytes as a UTF-8 string,
/// and bytes that are not valid UTF-8 are an error rather than a garbled
/// string. "json" behaves exactly like `->jsonParse`.
fn decode_method(
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

    let (result, codec_errors) = match codec {
        Codec::Base64 | Codec::Base64Url => decode_base64(method_name, data, input_path, spec),
        Codec::Json => JsonParseMethod.apply(method_name, None, data, vars, input_path, spec),
    };
    errors.extend(codec_errors);
    (result, errors)
}

fn decode_base64(
    method_name: &WithRange<String>,
    data: &JSON,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let error = |message: String| {
        (
            None,
            vec![ApplyToError::new(
                message,
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        )
    };

    let JSON::String(input) = data else {
        return error(format!(
            "Method ->{} requires a string input, got {}",
            method_name.as_ref(),
            json_type_name(data)
        ));
    };
    let input = input.as_str();

    // The two alphabets differ only in the characters for 62 and 63, so the
    // presence of either URL-safe character picks the alphabet.
    let decoded = if input.contains(['-', '_']) {
        URL_SAFE_PAD_INDIFFERENT.decode(input)
    } else {
        STANDARD_PAD_INDIFFERENT.decode(input)
    };

    let bytes = match decoded {
        Ok(bytes) => bytes,
        Err(err) => {
            return error(format!(
                "Method ->{} failed to decode base64 input: {err}",
                method_name.as_ref()
            ));
        }
    };

    match String::from_utf8(bytes) {
        Ok(text) => (Some(JSON::String(text.into())), Vec::new()),
        Err(err) => error(format!(
            "Method ->{} decoded bytes that are not valid UTF-8: {}",
            method_name.as_ref(),
            err.utf8_error()
        )),
    }
}

fn decode_shape(
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
            JsonParseMethod.shape(context, method_name, None, input_shape, dollar_shape)
        }
        // A codec known only at runtime could produce any JSON value.
        None => Shape::unknown(method_name.shape_location(context.source_id())),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    // --- RFC 4648 §10 test vectors, padded and unpadded ---

    #[rstest::rstest]
    #[case("", "")]
    #[case("Zg==", "f")]
    #[case("Zm8=", "fo")]
    #[case("Zm9v", "foo")]
    #[case("Zm9vYg==", "foob")]
    #[case("Zm9vYmE=", "fooba")]
    #[case("Zm9vYmFy", "foobar")]
    #[case("Zg", "f")]
    #[case("Zm8", "fo")]
    #[case("Zm9vYg", "foob")]
    #[case("Zm9vYmE", "fooba")]
    #[case("Zg=", "f")] // partial padding is tolerated too
    fn decode_rfc_vectors(
        #[case] input: &str,
        #[case] expected: &str,
        #[values("base64", "base64url")] codec: &str,
    ) {
        assert_eq!(
            selection!(&format!("$->decode('{codec}')")).apply_to(&json!(input)),
            (Some(json!(expected)), vec![]),
        );
    }

    // --- Either alphabet is accepted under either codec name ---

    #[rstest::rstest]
    #[case("PDw/Pz8+Pg==")]
    #[case("PDw/Pz8+Pg")]
    #[case("PDw_Pz8-Pg==")]
    #[case("PDw_Pz8-Pg")]
    fn decode_accepts_both_alphabets(
        #[case] input: &str,
        #[values("base64", "base64url")] codec: &str,
    ) {
        assert_eq!(
            selection!(&format!("$->decode('{codec}')")).apply_to(&json!(input)),
            (Some(json!("<<???>>")), vec![]),
        );
    }

    #[test]
    fn decode_multibyte_utf8() {
        assert_eq!(
            selection!("$->decode('base64url')").apply_to(&json!("Y2Fmw6kg8J-OiQ")),
            (Some(json!("café 🎉")), vec![]),
        );
    }

    // --- The motivating case: Gmail API message bodies ---

    #[test]
    fn decode_gmail_message_body() {
        // The Gmail API returns `payload.body.data` as unpadded base64url.
        let data = json!({
            "id": "18c2f0a1b2c3d4e5",
            "payload": {
                "mimeType": "text/html",
                "body": { "size": 28, "data": "PHA-U3Vic2NyaWJlPyBJdCdzIGZyZWUhPC9wPg" }
            }
        });
        assert_eq!(
            selection!("id html: payload.body.data->decode('base64url')").apply_to(&data),
            (
                Some(json!({
                    "id": "18c2f0a1b2c3d4e5",
                    "html": "<p>Subscribe? It's free!</p>"
                })),
                vec![],
            ),
        );
    }

    // --- Round trips ---

    #[rstest::rstest]
    #[case("")]
    #[case("a")]
    #[case("Hello, World!")]
    #[case("<<???>>")]
    #[case("café 🎉")]
    #[case("{\"nested\":\"json\"}")]
    fn encode_then_decode_roundtrip(
        #[case] original: &str,
        #[values("base64", "base64url")] codec: &str,
    ) {
        assert_eq!(
            selection!(&format!("$->encode('{codec}')->decode('{codec}')"))
                .apply_to(&json!(original)),
            (Some(json!(original)), vec![]),
        );
    }

    // --- Invalid input ---

    #[rstest::rstest]
    #[case("not base64!")]
    #[case("Z")]
    #[case("Zm9v=")]
    #[case("Zm9v Yg==")]
    #[case("PDw/Pz8-Pg")]
    fn decode_should_error_on_invalid_base64(#[case] input: &str) {
        let (result, errors) = selection!("$->decode('base64')").apply_to(&json!(input));
        assert_eq!(result, None);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .message()
                .starts_with("Method ->decode failed to decode base64 input: "),
            "{}",
            errors[0].message()
        );
    }

    #[rstest::rstest]
    #[case("//4=")] // 0xFF 0xFE
    #[case("gA")] // 0x80, a lone continuation byte
    #[case("wyg")] // 0xC3 0x28, a truncated two-byte sequence
    fn decode_should_error_on_invalid_utf8(#[case] input: &str) {
        let (result, errors) = selection!("$->decode('base64')").apply_to(&json!(input));
        assert_eq!(result, None);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .message()
                .starts_with("Method ->decode decoded bytes that are not valid UTF-8: "),
            "{}",
            errors[0].message()
        );
    }

    // --- Codec argument and input type ---

    #[rstest::rstest]
    #[case(
        "$->decode",
        r#"requires a codec argument ("base64", "base64url", or "json")"#
    )]
    #[case("$->decode('base64', 1)", "accepts exactly 1 argument (codec), got 2")]
    #[case("$->decode(true)", "requires a string codec argument, got boolean")]
    #[case(
        "$->decode('hex')",
        r#"does not support codec "hex", expected "base64", "base64url", or "json""#
    )]
    fn decode_should_error_on_bad_codec_argument(#[case] selection: &str, #[case] message: &str) {
        let (result, errors) = selection!(selection).apply_to(&json!("Zm9v"));
        assert_eq!(result, None);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message(), format!("Method ->decode {message}"));
    }

    #[rstest::rstest]
    #[case(json!(42), "number")]
    #[case(json!(false), "boolean")]
    #[case(json!(null), "null")]
    #[case(json!(["Zm9v"]), "array")]
    #[case(json!({"data": "Zm9v"}), "object")]
    fn decode_base64_should_error_on_non_string_input(
        #[case] input: JSON,
        #[case] expected_type: &str,
        #[values("base64", "base64url")] codec: &str,
    ) {
        let (result, errors) = selection!(&format!("$->decode('{codec}')")).apply_to(&input);
        assert_eq!(result, None);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].message(),
            format!("Method ->decode requires a string input, got {expected_type}")
        );
    }
}

#[cfg(test)]
mod json_codec_tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    // ->decode('json') is ->jsonParse: identical results, and identical errors
    // apart from naming the method that was actually called.
    #[rstest::rstest]
    #[case(json!("null"))]
    #[case(json!("42"))]
    #[case(json!(" true "))]
    #[case(json!("\"line\\nbreak\""))]
    #[case(json!("[1,\"two\",null]"))]
    #[case(json!("{\"nested\":{\"deep\":[1,{\"x\":true}]}}"))]
    #[case(json!(""))]
    #[case(json!("not valid json"))]
    #[case(json!("{\"key\":}"))]
    #[case(json!(42))]
    #[case(json!(null))]
    #[case(json!([1, 2, 3]))]
    #[case(json!({"key": "value"}))]
    fn decode_json_matches_json_parse(#[case] input: JSON) {
        let (expected, expected_errors) = selection!("$->jsonParse").apply_to(&input);
        let (actual, actual_errors) = selection!("$->decode('json')").apply_to(&input);
        assert_eq!(actual, expected);
        assert_eq!(
            actual_errors
                .iter()
                .map(|e| e
                    .message()
                    .replace("Method ->decode ", "Method ->jsonParse "))
                .collect::<Vec<_>>(),
            expected_errors
                .iter()
                .map(|e| e.message().to_string())
                .collect::<Vec<_>>(),
        );
    }

    #[rstest::rstest]
    #[case(json!(null))]
    #[case(json!("café 🎉"))]
    #[case(json!([1, "two", null]))]
    #[case(json!({"key": "value", "nested": {"list": [1, 2]}}))]
    fn encode_then_decode_json_roundtrip(#[case] original: JSON) {
        assert_eq!(
            selection!("$->encode('json')->decode('json')").apply_to(&original),
            (Some(original), vec![]),
        );
    }

    #[test]
    fn decode_base64url_then_json() {
        // A JWT payload segment is base64url-encoded JSON.
        assert_eq!(
            selection!("$->decode('base64url')->decode('json')")
                .apply_to(&json!("eyJzdWIiOiIxMjMiLCJhZG1pbiI6dHJ1ZX0")),
            (Some(json!({"sub": "123", "admin": true})), vec![]),
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
            span: 0..6,
        }
    }

    fn shape_with_arg(arg: LitExpr, input: Shape, dollar: Shape) -> Shape {
        let location = get_location();
        decode_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("decode".to_string(), Some(location.span)),
            Some(&MethodArgs {
                args: vec![WithRange::new(arg, None)],
                range: None,
            }),
            input,
            dollar,
        )
    }

    fn get_shape(codec: &str, input: Shape) -> Shape {
        shape_with_arg(LitExpr::String(codec.to_string()), input, Shape::none())
    }

    #[rstest::rstest]
    #[case(Shape::string([]))]
    #[case(Shape::string_value("Zm9v", []))]
    #[case(Shape::unknown([]))]
    #[case(Shape::name("a", []))]
    fn decode_base64_shape_returns_string_for_string_input(
        #[case] input: Shape,
        #[values("base64", "base64url")] codec: &str,
    ) {
        assert_eq!(get_shape(codec, input), Shape::string([get_location()]));
    }

    #[rstest::rstest]
    #[case(Shape::int([]))]
    #[case(Shape::null([]))]
    #[case(Shape::empty_object([]))]
    #[case(Shape::list(Shape::string([]), []))]
    fn decode_base64_shape_errors_for_non_string_input(
        #[case] input: Shape,
        #[values("base64", "base64url")] codec: &str,
    ) {
        assert_eq!(
            get_shape(codec, input),
            Shape::error(
                "Method ->decode requires a string input".to_string(),
                [get_location()]
            )
        );
    }

    #[rstest::rstest]
    #[case(Shape::string([]))]
    #[case(Shape::string_value("[1]", []))]
    #[case(Shape::int([]))]
    #[case(Shape::unknown([]))]
    fn decode_json_shape_matches_json_parse_shape(#[case] input: Shape) {
        let location = get_location();
        let expected = JsonParseMethod.shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("decode".to_string(), Some(location.span)),
            None,
            input.clone(),
            Shape::none(),
        );
        assert_eq!(get_shape("json", input), expected);
        assert_eq!(expected, Shape::unknown([get_location()]));
    }

    #[test]
    fn decode_shape_errors_for_unsupported_codec() {
        assert_eq!(
            get_shape("hex", Shape::string([])),
            Shape::error(
                r#"Method ->decode does not support codec "hex", expected "base64", "base64url", or "json""#
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn decode_shape_returns_unknown_for_runtime_codec() {
        let codec_path = LitExpr::Path(PathSelection {
            path: PathList::Key(
                Key::field("codec").into_with_range(),
                PathList::Empty.into_with_range(),
            )
            .into_with_range(),
        });
        assert_eq!(
            shape_with_arg(
                codec_path,
                Shape::string([]),
                Shape::record([("codec".to_string(), Shape::string([]))].into(), [])
            ),
            Shape::unknown([get_location()])
        );
    }
}
