use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::PathList;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::apply_to::ApplyToResultMethods;
use crate::sources::connect::json_selection::helpers::vec_push;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::known_var::KnownVariable;
use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;
use crate::sources::connect::json_selection::location::merge_ranges;

impl_arrow_method!(MatchMethod, match_method, match_shape);
/// The match method Takes any number of pairs [key, value], and returns value for the first
/// key that equals the data. If none of the pairs match, returns None.
/// Typically, the final pair will use @ as its key to ensure some default
/// value is returned.
///
/// The most common use case would be mapping values to an enum. For example:
/// vehicleType: type->match(
///                 ['1', 'CAR'],
///                 ['2', 'VAN'],
///                 [@, 'UNKNOWN'],
///               )
fn match_method(
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
                    let (candidate_opt, candidate_errors) =
                        pair[0].apply_to_path(data, vars, input_path);
                    errors.extend(candidate_errors);

                    if let Some(candidate) = candidate_opt {
                        if candidate == *data {
                            return pair[1]
                                .apply_to_path(data, vars, input_path)
                                .prepend_errors(errors);
                        }
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
                    "Method ->{} did not match any [candidate, value] pair",
                    method_name.as_ref(),
                ),
                input_path.to_vec(),
                merge_ranges(
                    method_name.range(),
                    method_args.and_then(|args| args.range()),
                ),
            ),
        ),
    )
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
pub(crate) fn match_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if let Some(MethodArgs { args, .. }) = method_args {
        let mut result_union = Vec::new();
        let mut has_infallible_case = false;

        for pair in args {
            if let LitExpr::Array(pair) = pair.as_ref() {
                if pair.len() == 2 {
                    if let LitExpr::Path(path) = pair[0].as_ref() {
                        if let PathList::Var(known_var, _tail) = path.path.as_ref() {
                            if known_var.as_ref() == &KnownVariable::AtSign {
                                has_infallible_case = true;
                            }
                        }
                    };

                    let value_shape = pair[1].compute_output_shape(
                        input_shape.clone(),
                        dollar_shape.clone(),
                        named_var_shapes,
                        source_id,
                    );
                    result_union.push(value_shape);
                }
            }
        }

        if !has_infallible_case {
            result_union.push(Shape::none());
        }

        if result_union.is_empty() {
            Shape::error(
                format!(
                    "Method ->{} requires at least one [candidate, value] pair",
                    method_name.as_ref(),
                ),
                merge_ranges(
                    method_name.range(),
                    method_args.and_then(|args| args.range()),
                )
                .map(|range| source_id.location(range)),
            )
        } else {
            Shape::one(result_union, method_name.shape_location(source_id))
        }
    } else {
        Shape::error(
            format!(
                "Method ->{} requires at least one [candidate, value] pair",
                method_name.as_ref(),
            ),
            method_name.shape_location(source_id),
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn match_should_select_correct_value_from_options() {
        assert_eq!(
            selection!(
                r#"
            name
            __typename: kind->match(
                ['dog', 'Canine'],
                ['cat', 'Feline'],
                [@, 'Exotic'],
            )
            "#
            )
            .apply_to(&json!({
                "kind": "cat",
                "name": "Whiskers",
            })),
            (
                Some(json!({
                    "__typename": "Feline",
                    "name": "Whiskers",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn match_should_select_default_value_using_at_sign() {
        assert_eq!(
            selection!(
                r#"
            name
            __typename: kind->match(
                ['dog', 'Canine'],
                ['cat', 'Feline'],
                [@, 'Exotic'],
            )
            "#
            )
            .apply_to(&json!({
                "kind": "axlotl",
                "name": "Gulpy",
            })),
            (
                Some(json!({
                    "__typename": "Exotic",
                    "name": "Gulpy",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn match_should_result_in_error_when_no_match_found() {
        let result = selection!(
            r#"
        name
        __typename: kind->match(
            ['dog', 'Canine'],
            ['cat', 'Feline'],
        )
        "#
        )
        .apply_to(&json!({
            "kind": "axlotl",
            "name": "Gulpy",
        }));

        assert_eq!(
            result.0,
            Some(json!({
                "name": "Gulpy",
            })),
        );
        assert!(
            result
                .1
                .iter()
                .any(|e| e.message() == "Method ->match did not match any [candidate, value] pair")
        );
    }
}
