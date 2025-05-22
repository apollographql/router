use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::safe_json::string::SafeString;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;
use crate::sources::connect::json_selection::safe_json::value::Value as JSON;

impl_arrow_method!(
    UrlSafeMethod,
    urlsafe_method,
    urlsafe_method_shape
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
fn urlsafe_method(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    _input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let JSON::String(safe_string) = data {
        let s = match safe_string {
            SafeString::Safe(byte_string) => byte_string,
            SafeString::Unsafe(byte_string) => byte_string,
        };
        let s = Some(JSON::String(SafeString::Safe(s.to_owned())));
        return (s, vec![]);
    }

    (Some(data.clone()), vec![])
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn urlsafe_method_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        first_arg.compute_output_shape(
            input_shape,
            dollar_shape,
            named_var_shapes,
            source_id,
        )
    } else {
        Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(source_id),
        )
    }
}

// #[cfg(test)]
// mod tests {
//     use serde_json_bytes::json;

//     use super::*;
//     use crate::selection;

//     #[rstest::rstest]
//     #[case(json!(["a","b","c"]), ", ", json!("a, b, c"))]
//     #[case(json!([1, 2, 3]), "|", json!("1|2|3"))]
//     #[case(json!([1.00000000000001, 2.9999999999999, 0.3]), "|", json!("1.00000000000001|2.9999999999999|0.3"))]
//     #[case(json!([true, false]), " and ", json!("true and false"))]
//     #[case(json!([null, "a", null, 1, null]), ", ", json!("a, 1"))]
//     #[case(json!([null, null]), ", ", json!(""))]
//     #[case(json!(1), ", ", json!("1"))]
//     #[case(json!("a"), ", ", json!("a"))]
//     #[case(json!(true), ", ", json!("true"))]
//     #[case(json!(null), ", ", json!(""))]
//     fn urlsafe_should_combine_arrays_with_a_separator(
//         #[case] input: JSON,
//         #[case] separator: String,
//         #[case] expected: JSON,
//     ) {
//         assert_eq!(
//             selection!(&format!("$->joinNotNull('{}')", separator)).apply_to(&input),
//             (Some(expected), vec![]),
//         );
//     }

//     #[rstest::rstest]
//     #[case(json!({"a": 1}), vec!["Method ->joinNotNull requires an array of scalar values as input"])]
//     #[case(json!([{"a": 1}, {"a": 2}]), vec!["Method ->joinNotNull requires an array of scalar values as input"])]
//     #[case(json!([[1, 2]]), vec!["Method ->joinNotNull requires an array of scalar values as input"])]
//     fn urlsafe_warnings(#[case] input: JSON, #[case] expected_warnings: Vec<&str>) {
//         use itertools::Itertools;

//         let (result, warnings) = selection!("$->joinNotNull(',')").apply_to(&input);
//         assert_eq!(result, None);
//         assert_eq!(
//             warnings.iter().map(|w| w.message()).collect_vec(),
//             expected_warnings
//         );
//     }

//     fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
//         urlsafe_method_shape(
//             &WithRange::new("joinNotNull".to_string(), Some(0..7)),
//             Some(&MethodArgs { args, range: None }),
//             input,
//             Shape::none(),
//             &IndexMap::default(),
//             &SourceId::new("test".to_string()),
//         )
//     }

//     #[test]
//     fn test_urlsafe_shape_no_args() {
//         let output_shape = get_shape(vec![], Shape::list(Shape::string([]), []));
//         assert_eq!(
//             output_shape,
//             Shape::error(
//                 "Method ->joinNotNull requires one argument".to_string(),
//                 vec![]
//             )
//         );
//     }

//     #[test]
//     fn test_urlsafe_shape_non_string_args() {
//         let output_shape = get_shape(
//             vec![WithRange::new(LitExpr::Bool(true), None)],
//             Shape::list(Shape::string([]), []),
//         );
//         assert_eq!(
//             output_shape,
//             Shape::error(
//                 "Method ->joinNotNull requires a string argument".to_string(),
//                 vec![]
//             )
//         );
//     }

//     #[test]
//     fn test_urlsafe_shape_two_args() {
//         let output_shape = get_shape(
//             vec![
//                 WithRange::new(LitExpr::String(",".to_string()), None),
//                 WithRange::new(LitExpr::String(",".to_string()), None),
//             ],
//             Shape::list(Shape::string([]), []),
//         );
//         assert_eq!(
//             output_shape,
//             Shape::error(
//                 "Method ->joinNotNull requires only one argument, but 2 were provided".to_string(),
//                 vec![]
//             )
//         );
//     }

//     #[test]
//     fn test_urlsafe_shape_scalar_input() {
//         let output_shape = get_shape(
//             vec![WithRange::new(LitExpr::String(",".to_string()), None)],
//             Shape::string([]),
//         );
//         assert_eq!(
//             output_shape,
//             Shape::string([SourceId::new("test".to_string()).location(0..7)])
//         );
//     }

//     #[test]
//     fn test_urlsafe_shape_list_of_list_input() {
//         let output_shape = get_shape(
//             vec![WithRange::new(LitExpr::String(",".to_string()), None)],
//             Shape::list(Shape::list(Shape::string([]), []), []),
//         );
//         assert_eq!(
//             output_shape,
//             Shape::error(
//                 "Method ->joinNotNull requires an array of scalar values as input".to_string(),
//                 vec![]
//             )
//         );
//     }

//     #[test]
//     fn test_urlsafe_shape_unknown_input() {
//         let output_shape = get_shape(
//             vec![WithRange::new(LitExpr::String(",".to_string()), None)],
//             Shape::unknown([]),
//         );
//         assert_eq!(
//             output_shape,
//             Shape::string([SourceId::new("test".to_string()).location(0..7)])
//         );
//     }

//     #[test]
//     fn test_urlsafe_shape_named_input() {
//         let output_shape = get_shape(
//             vec![WithRange::new(LitExpr::String(",".to_string()), None)],
//             Shape::name("$root.bar", []),
//         );
//         assert_eq!(
//             output_shape,
//             Shape::string([SourceId::new("test".to_string()).location(0..7)])
//         );
//     }
// }
