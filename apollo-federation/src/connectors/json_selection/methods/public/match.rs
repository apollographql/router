use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::PathList;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::apply_to::ApplyToResultMethods;
use crate::connectors::json_selection::helpers::vec_push;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::known_var::KnownVariable;
use crate::connectors::json_selection::lit_expr::LitExpr;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::json_selection::location::merge_ranges;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

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
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let mut errors = Vec::new();

    if let Some(MethodArgs { args, .. }) = method_args {
        for pair in args {
            if let LitExpr::Array(pair) = pair.as_ref() {
                let (pattern, value) = match pair.as_slice() {
                    [pattern, value] => (pattern, value),
                    _ => continue,
                };
                let (candidate_opt, candidate_errors) =
                    pattern.apply_to_path(data, vars, input_path, spec);
                errors.extend(candidate_errors);

                if let Some(candidate) = candidate_opt
                    && candidate == *data
                {
                    return value
                        .apply_to_path(data, vars, input_path, spec)
                        .prepend_errors(errors);
                };
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
                spec,
            ),
        ),
    )
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
pub(crate) fn match_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    if let Some(MethodArgs { args, .. }) = method_args {
        let mut result_union = Vec::new();
        let mut has_infallible_case = false;

        for pair in args {
            if let LitExpr::Array(pair) = pair.as_ref() {
                let (pattern, value) = match pair.as_slice() {
                    [pattern, value] => (pattern, value),
                    _ => continue,
                };
                if let LitExpr::Path(path) = pattern.as_ref()
                    && let PathList::Var(known_var, _tail) = path.path.as_ref()
                    && known_var.as_ref() == &KnownVariable::AtSign
                {
                    has_infallible_case = true;
                };

                let value_shape =
                    value.compute_output_shape(context, input_shape.clone(), dollar_shape.clone());
                result_union.push(value_shape);
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
                .map(|range| context.source_id().location(range)),
            )
        } else {
            Shape::one(
                result_union,
                method_name.shape_location(context.source_id()),
            )
        }
    } else {
        Shape::error(
            format!(
                "Method ->{} requires at least one [candidate, value] pair",
                method_name.as_ref(),
            ),
            method_name.shape_location(context.source_id()),
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::connectors::ConnectSpec;
    use crate::connectors::json_selection::ApplyToError;
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

    // TSH-22359: Reproduction of reported parse failure with nested object
    // subselections inside ->match branches. The user wrote comma-separated
    // properties inside a SubSelection context (e.g. `address { street: street,
    // city: city }`), but SubSelection is whitespace-delimited, not
    // comma-delimited. LitObject uses commas; SubSelection does not.

    #[test]
    fn tsh_22359_flat_scalars_in_match_with_commas_parses_ok() {
        // Test 1 from the ticket (verbatim): flat scalar fields inside ->match.
        // Commas separate LitObject properties, which is correct.
        let sel = selection!(
            r#"
            $.results {
                ... resultType->match(
                    ["book", { __typename: "FlatBook", id: id, title: title }],
                    ["author", { __typename: "FlatAuthor", id: id, name: name }]
                )
            }
            "#,
            ConnectSpec::V0_4
        );
        let (result, errors) = sel.apply_to(&json!({
            "results": [
                { "resultType": "book", "id": "1", "title": "Great Expectations" },
                { "resultType": "author", "id": "2", "name": "Dickens" }
            ]
        }));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(
            result,
            Some(json!([
                { "__typename": "FlatBook", "id": "1", "title": "Great Expectations" },
                { "__typename": "FlatAuthor", "id": "2", "name": "Dickens" }
            ]))
        );
    }

    #[test]
    fn tsh_22359_flat_scalars_simplified_with_shorthand() {
        // Same as ticket Test 1, but using V0_4 shorthand: `id` instead of
        // `id: id`, `title` instead of `title: title`, etc.
        let sel = selection!(
            r#"
            $.results {
                ... resultType->match(
                    ["book", { __typename: "FlatBook", id, title }],
                    ["author", { __typename: "FlatAuthor", id, name }]
                )
            }
            "#,
            ConnectSpec::V0_4
        );
        let (result, errors) = sel.apply_to(&json!({
            "results": [
                { "resultType": "book", "id": "1", "title": "Great Expectations" },
                { "resultType": "author", "id": "2", "name": "Dickens" }
            ]
        }));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(
            result,
            Some(json!([
                { "__typename": "FlatBook", "id": "1", "title": "Great Expectations" },
                { "__typename": "FlatAuthor", "id": "2", "name": "Dickens" }
            ]))
        );
    }

    #[test]
    fn tsh_22359_nested_subselection_with_commas_fails_to_parse() {
        // Test 2 from the ticket: nested object fields inside ->match.
        // The user wrote `address: address { street: street, city: city }`
        // which fails because `address` parses as a LitExpr (bare identifier)
        // and the following `{ ... }` is not consumed as part of it.
        let input = r#"
            $.results {
                ... resultType->match(
                    ["book", { __typename: "NestedBook", id: id, title: title, address: address { street: street, city: city } }],
                    ["author", { __typename: "NestedAuthor", id: id, name: name, address: address { street: street, city: city } }]
                )
            }
        "#;
        let result = crate::connectors::json_selection::JSONSelection::parse_with_spec(
            input,
            ConnectSpec::V0_4,
        );
        assert!(
            result.is_err(),
            "expected parse error, but it parsed successfully"
        );
    }

    #[test]
    fn tsh_22359_fix_litobj_commas_with_subselection_spaces() {
        // Fix option 1: outer LitObject uses commas, inner SubSelection uses
        // spaces. `address: address { street city }` parses as
        // LitExpr::Path(PathList::Key("address", PathList::Selection(...))).
        let sel = selection!(
            r#"
            $.results {
                ... resultType->match(
                    ["book", { __typename: "NestedBook", id: id, title: title, address: address { street city } }],
                    ["author", { __typename: "NestedAuthor", id: id, name: name, address: address { street city } }]
                )
            }
            "#,
            ConnectSpec::V0_4
        );
        let (result, errors) = sel.apply_to(&json!({
            "results": [
                { "resultType": "book", "id": "1", "title": "Great Expectations", "address": { "street": "48 Doughty St", "city": "London" } },
                { "resultType": "author", "id": "2", "name": "Dickens", "address": { "street": "1 Gads Hill", "city": "Higham" } }
            ]
        }));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(
            result,
            Some(json!([
                { "__typename": "NestedBook", "id": "1", "title": "Great Expectations", "address": { "street": "48 Doughty St", "city": "London" } },
                { "__typename": "NestedAuthor", "id": "2", "name": "Dickens", "address": { "street": "1 Gads Hill", "city": "Higham" } }
            ]))
        );
    }

    #[test]
    fn tsh_22359_fix_shorthand_with_nested_subselection() {
        // Fix option 2 (cleanest): V0_4 shorthand properties. `id` means
        // `id: id`, and `address { street city }` is a shorthand property
        // whose value is a path with SubSelection.
        let sel = selection!(
            r#"
            $.results {
                ... resultType->match(
                    ["book", { __typename: "NestedBook", id, title, address { street city } }],
                    ["author", { __typename: "NestedAuthor", id, name, address { street city } }]
                )
            }
            "#,
            ConnectSpec::V0_4
        );
        let (result, errors) = sel.apply_to(&json!({
            "results": [
                { "resultType": "book", "id": "1", "title": "Great Expectations", "address": { "street": "48 Doughty St", "city": "London" } },
                { "resultType": "author", "id": "2", "name": "Dickens", "address": { "street": "1 Gads Hill", "city": "Higham" } }
            ]
        }));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(
            result,
            Some(json!([
                { "__typename": "NestedBook", "id": "1", "title": "Great Expectations", "address": { "street": "48 Doughty St", "city": "London" } },
                { "__typename": "NestedAuthor", "id": "2", "name": "Dickens", "address": { "street": "1 Gads Hill", "city": "Higham" } }
            ]))
        );
    }

    #[rstest::rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    #[case::v0_4(ConnectSpec::V0_4)]
    fn match_should_return_none_when_pattern_argument_evaluates_to_none(#[case] spec: ConnectSpec) {
        assert_eq!(
            selection!("$.a->match([$.missing, 'default'])", spec).apply_to(&json!({
                "a": "test",
            })),
            (
                None,
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .missing not found in object",
                        "path": ["missing"],
                        "range": [14, 21],
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Method ->match did not match any [candidate, value] pair",
                        "path": ["a", "->match"],
                        "range": [5, 34],
                        "spec": spec.to_string(),
                    }))
                ]
            ),
        );
    }
}
