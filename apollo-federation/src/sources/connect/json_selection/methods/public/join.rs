use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

impl_arrow_method!(JoinMethod, join_method, join_method_shape);
/// Takes an array of scalar values and joins them into a single string using a separator.
///
/// $->echo(["hello", "world"])->join(", ") would result in "hello, world"
fn join_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let mut warnings = vec![];

    let Some(separator) = method_args
        .and_then(|args| args.args.first())
        .and_then(|s| match &**s {
            LitExpr::String(s) => Some(s),
            _ => None,
        })
    else {
        warnings.push(ApplyToError::new(
            format!(
                "Method ->{} requires a string argument",
                method_name.as_ref()
            ),
            input_path.to_vec(),
            method_name.range(),
        ));
        return (None, warnings);
    };

    fn to_string(value: &JSON) -> (String, Option<String>) {
        match value {
            JSON::Bool(b) => (b.then_some("true").unwrap_or("false").to_string(), None),
            JSON::Number(number) => (number.to_string(), None),
            JSON::String(byte_string) => (byte_string.as_str().to_string(), None),
            JSON::Null => ("".to_string(), None),
            JSON::Array(_) | JSON::Object(_) => (
                "".to_string(),
                Some("Method ->join requires an array of scalars values as input".to_string()),
            ),
        }
    }

    let joined = match data {
        JSON::Array(values) => values
            .iter()
            .map(to_string)
            .map(|(value, err)| {
                if let Some(err) = err {
                    warnings.push(ApplyToError::new(
                        err.to_string(),
                        input_path.to_vec(),
                        method_name.range(),
                    ));
                }
                value
            })
            .collect::<Vec<_>>()
            .join(separator),
        // Single values are emitted as strings with no separator
        _ => {
            let (value, err) = to_string(data);
            if let Some(err) = err {
                warnings.push(ApplyToError::new(
                    err.to_string(),
                    input_path.to_vec(),
                    method_name.range(),
                ));
            }
            value
        }
    };

    (Some(JSON::String(joined.into())), warnings)
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn join_method_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    let input_shape_contract = Shape::one(
        [
            Shape::string([]),
            Shape::int([]),
            Shape::float([]),
            Shape::bool([]),
            Shape::null([]),
            Shape::list(
                Shape::one(
                    [
                        Shape::string([]),
                        Shape::int([]),
                        Shape::float([]),
                        Shape::bool([]),
                        Shape::null([]),
                    ],
                    [],
                ),
                [],
            ),
        ],
        [],
    );

    // allow unknown input
    if !(input_shape.is_unknown() || matches!(input_shape.case(), ShapeCase::Name(_, _))) {
        let mismatches = input_shape_contract.validate(&input_shape);
        if !mismatches.is_empty() {
            return Shape::error(
                format!(
                    "Method ->{} requires an array of scalars values as input",
                    method_name.as_ref()
                ),
                [],
            );
        }
    }

    let Some(selection_shape) = method_args
        .and_then(|args| args.args.first())
        .map(|s| s.compute_output_shape(input_shape, dollar_shape, named_var_shapes, source_id))
    else {
        return Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            vec![],
        );
    };

    let method_count = method_args.map(|args| args.args.len()).unwrap_or_default();
    if method_count > 1 {
        return Shape::error(
            format!(
                "Method ->{} requires only one argument, but {} were provided",
                method_name.as_ref(),
                method_count
            ),
            vec![],
        );
    }

    // allow unknown separator
    if !(selection_shape.is_unknown() || matches!(selection_shape.case(), ShapeCase::Name(_, _))) {
        let mismatches = Shape::string([]).validate(&selection_shape);
        if !mismatches.is_empty() {
            return Shape::error(
                format!(
                    "Method ->{} requires a string argument",
                    method_name.as_ref()
                ),
                vec![],
            );
        }
    }

    Shape::string(method_name.shape_location(source_id))
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    #[rstest::rstest]
    #[case(json!(["a","b","c"]), ", ", json!("a, b, c"))]
    #[case(json!([1, 2, 3]), "|", json!("1|2|3"))]
    #[case(json!([1.00000000000001, 2.9999999999999, 0.3]), "|", json!("1.00000000000001|2.9999999999999|0.3"))]
    #[case(json!([true, false]), " and ", json!("true and false"))]
    #[case(json!([null, "a", 1]), ", ", json!(", a, 1"))]
    #[case(json!([null, null]), ", ", json!(", "))] // two nulls should be joined with a comma
    #[case(json!(1), ", ", json!("1"))]
    #[case(json!("a"), ", ", json!("a"))]
    #[case(json!(true), ", ", json!("true"))]
    #[case(json!(null), ", ", json!(""))]
    fn join_should_combine_arrays_with_a_separator(
        #[case] input: JSON,
        #[case] separator: String,
        #[case] expected: JSON,
    ) {
        assert_eq!(
            selection!(&format!("$->join('{}')", separator)).apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    #[rstest::rstest]
    #[case(json!({"a": 1}), json!(""), vec!["Method ->join requires an array of scalars values as input"])]
    #[case(json!([{"a": 1}, {"a": 2}]), json!(","), vec!["Method ->join requires an array of scalars values as input"])]
    #[case(json!([[1, 2]]), json!(""), vec!["Method ->join requires an array of scalars values as input"])]
    fn join_warnings(
        #[case] input: JSON,
        #[case] expected: JSON,
        #[case] expected_warnings: Vec<&str>,
    ) {
        use itertools::Itertools;

        let (result, warnings) = selection!("$->join(',')").apply_to(&input);
        assert_eq!(result, Some(expected));
        assert_eq!(
            warnings.iter().map(|w| w.message()).collect_vec(),
            expected_warnings
        );
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        join_method_shape(
            &WithRange::new("join".to_string(), Some(0..7)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::none(),
            &IndexMap::default(),
            &SourceId::new("test".to_string()),
        )
    }

    #[test]
    fn test_join_shape_no_args() {
        let output_shape = get_shape(vec![], Shape::list(Shape::string([]), []));
        assert_eq!(
            output_shape,
            Shape::error("Method ->join requires one argument".to_string(), vec![])
        );
    }

    #[test]
    fn test_join_shape_non_string_args() {
        let output_shape = get_shape(
            vec![WithRange::new(LitExpr::Bool(true), None)],
            Shape::list(Shape::string([]), []),
        );
        assert_eq!(
            output_shape,
            Shape::error(
                "Method ->join requires a string argument".to_string(),
                vec![]
            )
        );
    }

    #[test]
    fn test_join_shape_two_args() {
        let output_shape = get_shape(
            vec![
                WithRange::new(LitExpr::String(",".to_string()), None),
                WithRange::new(LitExpr::String(",".to_string()), None),
            ],
            Shape::list(Shape::string([]), []),
        );
        assert_eq!(
            output_shape,
            Shape::error(
                "Method ->join requires only one argument, but 2 were provided".to_string(),
                vec![]
            )
        );
    }

    #[test]
    fn test_join_shape_scalar_input() {
        let output_shape = get_shape(
            vec![WithRange::new(LitExpr::String(",".to_string()), None)],
            Shape::string([]),
        );
        assert_eq!(
            output_shape,
            Shape::string([SourceId::new("test".to_string()).location(0..7)])
        );
    }

    #[test]
    fn test_join_shape_list_of_list_input() {
        let output_shape = get_shape(
            vec![WithRange::new(LitExpr::String(",".to_string()), None)],
            Shape::list(Shape::list(Shape::string([]), []), []),
        );
        assert_eq!(
            output_shape,
            Shape::error(
                "Method ->join requires an array of scalars values as input".to_string(),
                vec![]
            )
        );
    }

    #[test]
    fn test_join_shape_unknown_input() {
        let output_shape = get_shape(
            vec![WithRange::new(LitExpr::String(",".to_string()), None)],
            Shape::unknown([]),
        );
        assert_eq!(
            output_shape,
            Shape::string([SourceId::new("test".to_string()).location(0..7)])
        );
    }

    #[test]
    fn test_join_shape_named_input() {
        let output_shape = get_shape(
            vec![WithRange::new(LitExpr::String(",".to_string()), None)],
            Shape::name("$root.bar", []),
        );
        assert_eq!(
            output_shape,
            Shape::string([SourceId::new("test".to_string()).location(0..7)])
        );
    }
}
