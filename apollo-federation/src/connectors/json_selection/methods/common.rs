use serde_json::Number;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_type_name;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;

pub(crate) fn is_comparable_shape_combination(shape1: &Shape, shape2: &Shape) -> bool {
    if Shape::float([]).accepts(shape1) {
        Shape::float([]).accepts(shape2) || shape2.accepts(&Shape::unknown([]))
    } else if Shape::string([]).accepts(shape1) {
        Shape::string([]).accepts(shape2) || shape2.accepts(&Shape::unknown([]))
    } else if shape1.accepts(&Shape::unknown([])) {
        Shape::float([]).accepts(shape2)
            || Shape::string([]).accepts(shape2)
            || shape2.accepts(&Shape::unknown([]))
    } else {
        false
    }
}

pub(crate) fn number_value_as_float(
    number: &Number,
    method_name: &WithRange<String>,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> Result<f64, ApplyToError> {
    match number.as_f64() {
        Some(val) => Ok(val),
        None => {
            // Note that we don't have tests for these `None` cases because I can't actually find a case where this ever actually fails
            // It seems that the current implementation in serde_json always returns a value
            Err(ApplyToError::new(
                format!(
                    "Method ->{} fail to convert applied to value to float.",
                    method_name.as_ref(),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            ))
        }
    }
}

/// The codecs understood by `->encode` and `->decode`. JSON has no byte type,
/// so the base64 codecs map UTF-8 strings to strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    /// RFC 4648 §4 standard alphabet, padded when encoding.
    Base64,
    /// RFC 4648 §5 URL- and filename-safe alphabet, unpadded when encoding.
    Base64Url,
    /// Same as `->jsonStringify` (encode) and `->jsonParse` (decode).
    Json,
}

impl Codec {
    const EXPECTED: &'static str = r#""base64", "base64url", or "json""#;

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "base64" => Some(Self::Base64),
            "base64url" => Some(Self::Base64Url),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Evaluates the single codec argument shared by `->encode` and `->decode`.
pub(crate) fn codec_arg(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<Codec>, Vec<ApplyToError>) {
    let error = |message: String| {
        ApplyToError::new(message, input_path.to_vec(), method_name.range(), spec)
    };

    let args = method_args.map_or(&[][..], |args| args.args.as_slice());
    let [arg] = args else {
        return (
            None,
            vec![error(codec_arg_count_message(method_name, args.len()))],
        );
    };

    let (value_opt, mut errors) = arg.apply_to_path(data, vars, input_path, spec);
    let codec = match value_opt {
        Some(JSON::String(name)) => {
            let codec = Codec::from_name(name.as_str());
            if codec.is_none() {
                errors.push(error(unsupported_codec_message(method_name, name.as_str())));
            }
            codec
        }
        Some(other) => {
            errors.push(error(format!(
                "Method ->{} requires a string codec argument, got {}",
                method_name.as_ref(),
                json_type_name(&other)
            )));
            None
        }
        None => {
            errors.push(error(format!(
                "Method ->{} requires a string codec argument, but received null",
                method_name.as_ref()
            )));
            None
        }
    };
    (codec, errors)
}

/// Shape-checks the codec argument of `->encode` and `->decode`. Returns the
/// codec when the argument is a string literal, `None` when it is only known
/// at runtime, and an error shape when it is statically not a supported codec.
pub(crate) fn codec_arg_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: &Shape,
    dollar_shape: &Shape,
) -> Result<Option<Codec>, Shape> {
    let location = method_name.shape_location(context.source_id());

    let args = method_args.map_or(&[][..], |args| args.args.as_slice());
    let [arg] = args else {
        return Err(Shape::error(
            codec_arg_count_message(method_name, args.len()),
            location,
        ));
    };

    let arg_shape = arg.compute_output_shape(context, input_shape.clone(), dollar_shape.clone());
    match arg_shape.case() {
        ShapeCase::String(Some(name)) => Codec::from_name(name)
            .map(Some)
            .ok_or_else(|| Shape::error(unsupported_codec_message(method_name, name), location)),
        ShapeCase::Name(_, _) => Ok(None),
        _ if arg_shape.is_unknown() || Shape::string([]).validate(&arg_shape).is_none() => Ok(None),
        _ => Err(Shape::error(
            format!(
                "Method ->{} requires a string codec argument",
                method_name.as_ref()
            ),
            location,
        )),
    }
}

/// The error shape for a codec that needs a string input, if `input_shape` is
/// statically known and is not a string.
pub(crate) fn string_input_shape_error(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    input_shape: &Shape,
) -> Option<Shape> {
    if input_shape.is_unknown()
        || matches!(input_shape.case(), ShapeCase::Name(_, _))
        || Shape::string([]).validate(input_shape).is_none()
    {
        return None;
    }
    Some(Shape::error(
        format!("Method ->{} requires a string input", method_name.as_ref()),
        input_shape
            .locations()
            .cloned()
            .chain(method_name.shape_location(context.source_id())),
    ))
}

fn codec_arg_count_message(method_name: &WithRange<String>, count: usize) -> String {
    if count == 0 {
        format!(
            "Method ->{} requires a codec argument ({})",
            method_name.as_ref(),
            Codec::EXPECTED
        )
    } else {
        format!(
            "Method ->{} accepts exactly 1 argument (codec), got {count}",
            method_name.as_ref()
        )
    }
}

fn unsupported_codec_message(method_name: &WithRange<String>, name: &str) -> String {
    format!(
        "Method ->{} does not support codec \"{name}\", expected {}",
        method_name.as_ref(),
        Codec::EXPECTED
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case(Shape::float([]), Shape::float([]))]
    #[case(Shape::float([]), Shape::unknown([]))]
    #[case(Shape::float([]), Shape::name("test", []))]
    #[case(Shape::float([]), Shape::int([]))]
    #[case(Shape::string([]), Shape::string([]))]
    #[case(Shape::string([]), Shape::unknown([]))]
    #[case(Shape::string([]), Shape::name("test", []))]
    #[case(Shape::unknown([]), Shape::float([]))]
    #[case(Shape::unknown([]), Shape::int([]))]
    #[case(Shape::unknown([]), Shape::string([]))]
    #[case(Shape::unknown([]), Shape::unknown([]))]
    #[case(Shape::unknown([]), Shape::name("test", []))]
    #[case(Shape::name("test", []), Shape::float([]))]
    #[case(Shape::name("test", []), Shape::string([]))]
    #[case(Shape::name("test", []), Shape::unknown([]))]
    #[case(Shape::name("test", []), Shape::int([]))]
    #[case(Shape::name("test", []), Shape::name("test", []))]
    #[case(Shape::int([]), Shape::float([]))]
    #[case(Shape::int([]), Shape::int([]))]
    #[case(Shape::int([]), Shape::name("test", []))]
    #[case(Shape::int([]), Shape::unknown([]))]
    #[case(Shape::one([Shape::string([])], []), Shape::one([Shape::string([])], []))]
    fn test_is_comparable_shape_combination_positive_cases(
        #[case] shape1: Shape,
        #[case] shape2: Shape,
    ) {
        assert!(is_comparable_shape_combination(&shape1, &shape2));
    }

    #[rstest::rstest]
    #[case(Shape::string([]), Shape::int([]))]
    #[case(Shape::string([]), Shape::bool([]))]
    #[case(Shape::string([]), Shape::null([]))]
    #[case(Shape::string([]), Shape::float([]))]
    #[case(Shape::string([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::string([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::float([]), Shape::bool([]))]
    #[case(Shape::float([]), Shape::null([]))]
    #[case(Shape::float([]), Shape::string([]))]
    #[case(Shape::float([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::float([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::int([]), Shape::string([]))]
    #[case(Shape::int([]), Shape::bool([]))]
    #[case(Shape::int([]), Shape::null([]))]
    #[case(Shape::int([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::int([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::null([]), Shape::float([]))]
    #[case(Shape::null([]), Shape::int([]))]
    #[case(Shape::null([]), Shape::null([]))]
    #[case(Shape::null([]), Shape::string([]))]
    #[case(Shape::null([]), Shape::unknown([]))]
    #[case(Shape::null([]), Shape::name("test", []))]
    #[case(Shape::null([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::null([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::name("test", []), Shape::bool([]))]
    #[case(Shape::name("test", []), Shape::null([]))]
    #[case(Shape::name("test", []), Shape::list(Shape::string([]), []))]
    #[case(Shape::name("test", []), Shape::dict(Shape::string([]), []))]
    #[case(Shape::unknown([]), Shape::bool([]))]
    #[case(Shape::unknown([]), Shape::null([]))]
    #[case(Shape::unknown([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::unknown([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::list(Shape::string([]), []), Shape::string([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::float([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::int([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::bool([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::null([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::unknown([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::name("test", []))]
    #[case(Shape::list(Shape::string([]), []), Shape::dict(Shape::string([]), []))]
    #[case(Shape::dict(Shape::string([]), []), Shape::string([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::float([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::int([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::bool([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::null([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::unknown([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::name("test", []))]
    #[case(Shape::dict(Shape::string([]), []), Shape::list(Shape::string([]), []))]
    #[case(Shape::bool([]), Shape::float([]))]
    #[case(Shape::bool([]), Shape::string([]))]
    #[case(Shape::bool([]), Shape::int([]))]
    #[case(Shape::bool([]), Shape::unknown([]))]
    #[case(Shape::bool([]), Shape::name("test", []))]
    #[case(Shape::bool([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::bool([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::one([Shape::string([])], []), Shape::one([Shape::int([])], []))]
    fn test_is_comparable_shape_combination_negative_cases(
        #[case] shape1: Shape,
        #[case] shape2: Shape,
    ) {
        assert!(!is_comparable_shape_combination(&shape1, &shape2));
    }
}

#[cfg(test)]
mod codec_shape_tests {
    use serde_json::Number;
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

    fn get_codec(args: Option<Vec<LitExpr>>, dollar_shape: Shape) -> Result<Option<Codec>, Shape> {
        let location = get_location();
        let method_args = args.map(|args| MethodArgs {
            args: args
                .into_iter()
                .map(|arg| WithRange::new(arg, None))
                .collect(),
            range: None,
        });
        codec_arg_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("encode".to_string(), Some(location.span)),
            method_args.as_ref(),
            &Shape::string([]),
            &dollar_shape,
        )
    }

    fn string_arg(value: &str) -> Option<Vec<LitExpr>> {
        Some(vec![LitExpr::String(value.to_string())])
    }

    fn codec_path_arg() -> Option<Vec<LitExpr>> {
        Some(vec![LitExpr::Path(PathSelection {
            path: PathList::Key(
                Key::field("codec").into_with_range(),
                PathList::Empty.into_with_range(),
            )
            .into_with_range(),
        })])
    }

    #[rstest::rstest]
    #[case("base64", Codec::Base64)]
    #[case("base64url", Codec::Base64Url)]
    #[case("json", Codec::Json)]
    fn codec_arg_shape_resolves_literal_codecs(#[case] name: &str, #[case] codec: Codec) {
        assert_eq!(get_codec(string_arg(name), Shape::none()), Ok(Some(codec)));
    }

    #[rstest::rstest]
    #[case("hex")]
    #[case("url")]
    #[case("Base64")]
    #[case("")]
    fn codec_arg_shape_rejects_unsupported_literal_codecs(#[case] name: &str) {
        assert_eq!(
            get_codec(string_arg(name), Shape::none()),
            Err(Shape::error(
                format!(
                    "Method ->encode does not support codec \"{name}\", expected \"base64\", \"base64url\", or \"json\""
                ),
                [get_location()]
            ))
        );
    }

    #[rstest::rstest]
    #[case(None)]
    #[case(Some(vec![]))]
    fn codec_arg_shape_requires_an_argument(#[case] args: Option<Vec<LitExpr>>) {
        assert_eq!(
            get_codec(args, Shape::none()),
            Err(Shape::error(
                "Method ->encode requires a codec argument (\"base64\", \"base64url\", or \"json\")"
                    .to_string(),
                [get_location()]
            ))
        );
    }

    #[test]
    fn codec_arg_shape_rejects_extra_arguments() {
        assert_eq!(
            get_codec(
                Some(vec![
                    LitExpr::String("base64".to_string()),
                    LitExpr::String("json".to_string()),
                ]),
                Shape::none()
            ),
            Err(Shape::error(
                "Method ->encode accepts exactly 1 argument (codec), got 2".to_string(),
                [get_location()]
            ))
        );
    }

    #[test]
    fn codec_arg_shape_rejects_non_string_literal() {
        assert_eq!(
            get_codec(Some(vec![LitExpr::Number(Number::from(64))]), Shape::none()),
            Err(Shape::error(
                "Method ->encode requires a string codec argument".to_string(),
                [get_location()]
            ))
        );
    }

    fn codec_record(codec: Shape) -> Shape {
        Shape::record([("codec".to_string(), codec)].into(), [])
    }

    #[rstest::rstest]
    #[case(codec_record(Shape::string([])))]
    #[case(codec_record(Shape::name("$args.codec", [])))]
    #[case(Shape::unknown([]))]
    fn codec_arg_shape_defers_dynamic_codecs_to_runtime(#[case] dollar_shape: Shape) {
        assert_eq!(get_codec(codec_path_arg(), dollar_shape), Ok(None));
    }

    #[test]
    fn codec_arg_shape_rejects_dynamic_non_string_codec() {
        assert_eq!(
            get_codec(codec_path_arg(), codec_record(Shape::int([]))),
            Err(Shape::error(
                "Method ->encode requires a string codec argument".to_string(),
                [get_location()]
            ))
        );
    }
}
