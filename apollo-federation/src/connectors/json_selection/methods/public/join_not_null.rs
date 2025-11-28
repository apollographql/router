use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_to_string;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(
    JoinNotNullMethod,
    join_not_null_method,
    join_not_null_method_shape
);
/// Takes an array of scalar values and joins them into a single string using a
/// separator, skipping null values.
///
/// This method is specifically useful when dealing with lists of entity
/// references in Federation, which can contain null. It's rare that you'll want
/// to send a `null` to an upstream service when fetching a batch of entities,
/// so this is a useful and convenient method.
///
/// $->echo(["hello", null, "world"])->joinNotNull(", ") would result in "hello, world"
fn join_not_null_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let mut warnings = vec![];

    let Some((separator, arg_warnings)) = method_args
        .and_then(|args| args.args.first())
        .map(|arg| arg.apply_to_path(data, vars, input_path, spec))
    else {
        warnings.push(ApplyToError::new(
            format!(
                "Method ->{} requires a string argument",
                method_name.as_ref()
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
        return (None, warnings);
    };

    warnings.extend(arg_warnings);

    let Some(separator) = separator.as_ref().and_then(|s| match s {
        JSON::String(s) => Some(s),
        _ => None,
    }) else {
        warnings.push(ApplyToError::new(
            format!(
                "Method ->{} requires a string argument, but received {}",
                method_name.as_ref(),
                separator
                    .as_ref()
                    .map_or_else(|| "null".to_string(), |s| s.to_string())
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
        return (None, warnings);
    };

    fn to_string(value: &JSON, method_name: &str) -> Result<Option<String>, String> {
        json_to_string(value).map_err(|_| {
            format!("Method ->{method_name} requires an array of scalar values as input",)
        })
    }

    let joined = match data {
        JSON::Array(values) => {
            let mut joined = Vec::with_capacity(values.len());
            for value in values {
                match to_string(value, method_name) {
                    Ok(Some(value)) => joined.push(value),
                    Ok(None) => {}
                    Err(err) => {
                        warnings.push(ApplyToError::new(
                            err,
                            input_path.to_vec(),
                            method_name.range(),
                            spec,
                        ));
                        return (None, warnings);
                    }
                }
            }
            joined.join(separator.as_str())
        }
        // Single values are emitted as strings with no separator
        _ => match to_string(data, method_name) {
            Ok(value) => value.unwrap_or_else(|| "".to_string()),
            Err(err) => {
                warnings.push(ApplyToError::new(
                    err,
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                ));
                return (None, warnings);
            }
        },
    };

    (Some(JSON::String(joined.into())), warnings)
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn join_not_null_method_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
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
        if mismatches.is_some() {
            return Shape::error(
                format!(
                    "Method ->{} requires an array of scalar values as input",
                    method_name.as_ref()
                ),
                [],
            );
        }
    }

    let Some(selection_shape) = method_args
        .and_then(|args| args.args.first())
        .map(|s| s.compute_output_shape(context, input_shape, dollar_shape))
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
        if mismatches.is_some() {
            return Shape::error(
                format!(
                    "Method ->{} requires a string argument",
                    method_name.as_ref()
                ),
                vec![],
            );
        }
    }

    Shape::string(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;
    use shape::location::SourceId;

    use super::*;
    use crate::connectors::json_selection::ApplyToError;
    use crate::connectors::json_selection::lit_expr::LitExpr;
    use crate::selection;

    #[rstest::rstest]
    #[case(json!(["a","b","c"]), ", ", json!("a, b, c"))]
    #[case(json!([1, 2, 3]), "|", json!("1|2|3"))]
    #[case(json!([1.00000000000001, 2.9999999999999, 0.3]), "|", json!("1.00000000000001|2.9999999999999|0.3"))]
    #[case(json!([true, false]), " and ", json!("true and false"))]
    #[case(json!([null, "a", null, 1, null]), ", ", json!("a, 1"))]
    #[case(json!([null, null]), ", ", json!(""))]
    #[case(json!(1), ", ", json!("1"))]
    #[case(json!("a"), ", ", json!("a"))]
    #[case(json!(true), ", ", json!("true"))]
    #[case(json!(null), ", ", json!(""))]
    fn join_not_null_should_combine_arrays_with_a_separator(
        #[case] input: JSON,
        #[case] separator: String,
        #[case] expected: JSON,
    ) {
        assert_eq!(
            selection!(&format!("$->joinNotNull('{separator}')")).apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    #[test]
    fn join_not_null_evaluates_argument() {
        assert_eq!(
            selection!(&"$->joinNotNull(@->first)").apply_to(&json!(["1", "2", "3"])),
            (Some(json!("11213")), vec![]),
        );
    }

    #[rstest::rstest]
    #[case(json!({"a": 1}), vec!["Method ->joinNotNull requires an array of scalar values as input"])]
    #[case(json!([{"a": 1}, {"a": 2}]), vec!["Method ->joinNotNull requires an array of scalar values as input"])]
    #[case(json!([[1, 2]]), vec!["Method ->joinNotNull requires an array of scalar values as input"])]
    fn join_not_null_warnings(#[case] input: JSON, #[case] expected_warnings: Vec<&str>) {
        use itertools::Itertools;

        let (result, warnings) = selection!("$->joinNotNull(',')").apply_to(&input);
        assert_eq!(result, None);
        assert_eq!(
            warnings.iter().map(|w| w.message()).collect_vec(),
            expected_warnings
        );
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        join_not_null_method_shape(
            &ShapeContext::new(SourceId::new("test".to_string())),
            &WithRange::new("joinNotNull".to_string(), Some(0..7)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::none(),
        )
    }

    #[test]
    fn test_join_not_null_shape_no_args() {
        let output_shape = get_shape(vec![], Shape::list(Shape::string([]), []));
        assert_eq!(
            output_shape,
            Shape::error(
                "Method ->joinNotNull requires one argument".to_string(),
                vec![]
            )
        );
    }

    #[test]
    fn test_join_not_null_shape_non_string_args() {
        let output_shape = get_shape(
            vec![WithRange::new(LitExpr::Bool(true), None)],
            Shape::list(Shape::string([]), []),
        );
        assert_eq!(
            output_shape,
            Shape::error(
                "Method ->joinNotNull requires a string argument".to_string(),
                vec![]
            )
        );
    }

    #[test]
    fn test_join_not_null_shape_two_args() {
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
                "Method ->joinNotNull requires only one argument, but 2 were provided".to_string(),
                vec![]
            )
        );
    }

    #[test]
    fn test_join_not_null_shape_scalar_input() {
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
    fn test_join_not_null_shape_list_of_list_input() {
        let output_shape = get_shape(
            vec![WithRange::new(LitExpr::String(",".to_string()), None)],
            Shape::list(Shape::list(Shape::string([]), []), []),
        );
        assert_eq!(
            output_shape,
            Shape::error(
                "Method ->joinNotNull requires an array of scalar values as input".to_string(),
                vec![]
            )
        );
    }

    #[test]
    fn test_join_not_null_shape_unknown_input() {
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
    fn test_join_not_null_shape_named_input() {
        let output_shape = get_shape(
            vec![WithRange::new(LitExpr::String(",".to_string()), None)],
            Shape::name("$root.bar", []),
        );
        assert_eq!(
            output_shape,
            Shape::string([SourceId::new("test".to_string()).location(0..7)])
        );
    }

    #[rstest::rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn join_not_null_should_return_none_when_argument_evaluates_to_none(#[case] spec: ConnectSpec) {
        assert_eq!(
            selection!("$.a->joinNotNull($.missing)", spec).apply_to(&json!({
                "a": ["hello", "world"],
            })),
            (
                None,
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .missing not found in object",
                        "path": ["missing"],
                        "range": [19, 26],
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Method ->joinNotNull requires a string argument, but received null",
                        "path": ["a", "->joinNotNull"],
                        "range": [5, 16],
                        "spec": spec.to_string(),
                    }))
                ]
            ),
        );
    }
}
