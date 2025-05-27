use crate::sources::connect::json_selection::safe_json::Value as JSON;
use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::apply_to::ApplyToResultMethods;
use crate::sources::connect::json_selection::helpers::vec_push;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;
use crate::sources::connect::json_selection::location::merge_ranges;

impl_arrow_method!(MatchIfMethod, match_if_method, match_if_shape);
/// Like ->match, but expects the first element of each pair to evaluate to a
/// boolean, returning the second element of the first pair whose first element
/// is true. This makes providing a final catch-all case easy, since the last
/// pair can be [true, <default>].
///
/// Simplest example:
///
/// $->echo(123)->matchIf([123, 'It matched!'], [true, 'It did not match!'])        results in 'It matched!'
/// $->echo(123)->matchIf([456, 'It matched!'], [true, 'It did not match!'])        results in 'It did not match!'
///
fn match_if_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let mut errors = Vec::new();

    if let Some(MethodArgs { args, .. }) = method_args {
        for pair in args {
            if let LitExpr::Array(pair) = pair.as_ref() {
                if pair.len() == 2 {
                    let (condition_opt, condition_errors) =
                        pair[0].apply_to_path(data, vars, input_path);
                    errors.extend(condition_errors);

                    if condition_opt == Some(JSON::Bool(true)) {
                        return pair[1]
                            .apply_to_path(data, vars, input_path)
                            .prepend_errors(errors);
                    };
                }
            }
        }
    }
    (
        None,
        vec_push(
            errors,
            ApplyToError::new(
                format!(
                    "Method ->{} did not match any [condition, value] pair",
                    method_name.as_ref(),
                ),
                input_path
                    .to_vec()
                    .into_iter()
                    .map(|safe_json| safe_json.into())
                    .collect(),
                merge_ranges(
                    method_name.range(),
                    method_args.and_then(|args| args.range()),
                ),
            ),
        ),
    )
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn match_if_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    use super::super::public::match_shape;
    // Since match_shape does not inspect the candidate expressions, we can
    // reuse it for ->matchIf, where the only functional difference is that the
    // candidate expressions are expected to be boolean.
    match_shape(
        method_name,
        method_args,
        input_shape,
        dollar_shape,
        named_var_shapes,
        source_id,
    )
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn match_if_should_return_first_element_evaluated_to_true() {
        assert_eq!(
            selection!(
                r#"
            num: value->matchIf(
                [@->typeof->eq('number'), @],
                [true, 'not a number']
            )
            "#
            )
            .apply_to(&json!({ "value": 123 })),
            (
                Some(json!({
                    "num": 123,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn match_if_should_return_default_true_element_when_no_other_matches() {
        assert_eq!(
            selection!(
                r#"
            num: value->matchIf(
                [@->typeof->eq('number'), @],
                [true, 'not a number']
            )
            "#
            )
            .apply_to(&json!({ "value": true })),
            (
                Some(json!({
                    "num": "not a number",
                })),
                vec![],
            ),
        );
    }
}
