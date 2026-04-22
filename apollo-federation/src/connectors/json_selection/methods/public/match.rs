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

    // TSH-22359: Reproduction of a reported parse failure when users place
    // nested object subselections inside `->match` branches.
    //
    // Under connect/v0.2 and v0.3 the top-level `JSONSelection` grammar
    // distinguishes between:
    //
    //   • `LitObject`    — comma-separated key/value list, appearing as a
    //                      `LitExpr` inside a path or method argument.
    //   • `SubSelection` — whitespace-separated `NamedSelection` list,
    //                      appearing as the body of a path tail or the
    //                      top-level selection.
    //
    // That distinction tripped users writing nested object shapes inside a
    // `->match` result arm: the outer `{ ... }` (inside the match tuple) is
    // a LitObject and so uses commas, but the inner `address { ... }` is a
    // path tail, making its `{ ... }` a SubSelection that must use
    // whitespace. Mixing the styles — e.g. `address: address { street: street,
    // city: city }` — failed to parse.
    //
    // The unification landed in this commit collapses the distinction at
    // v0.4: `LitObject ::= SubSelection`, and `NamedSelectionList` accepts
    // either comma- or whitespace-separated items (picked once per list,
    // not mixed). Test 2 is therefore flipped from a "fails to parse"
    // assertion to a "parses and evaluates correctly" assertion, and the
    // ticket's original input is now a user-facing fix of its own. The
    // whitespace-inner and shorthand workarounds (tests 4 and 5) remain
    // valid style choices rather than requirements.

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
    fn tsh_22359_nested_subselection_with_commas_parses_and_evaluates() {
        // Test 2 from the ticket: the user's original input, which used
        // commas inside the nested `{ ... }`. Under the legacy v0.2/v0.3
        // grammar this was a parse error (see the V0_3 parity test below).
        // Under the unified v0.4 grammar the inner `{ ... }` is a
        // SubSelection that accepts either comma- or whitespace-separated
        // NamedSelections, so the original input parses and apply_to
        // produces exactly the nested object the ticket author wanted.
        let sel = selection!(
            r#"
            $.results {
                ... resultType->match(
                    ["book", { __typename: "NestedBook", id: id, title: title, address: address { street: street, city: city } }],
                    ["author", { __typename: "NestedAuthor", id: id, name: name, address: address { street: street, city: city } }]
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
    fn tsh_22359_fix_litobj_commas_with_subselection_spaces() {
        // Pre-unification workaround: outer LitObject uses commas, inner
        // SubSelection uses spaces. `address: address { street city }` parses
        // as LitExpr::Path(PathList::Key("address", PathList::Selection(...))).
        // Post-unification this is no longer a workaround but a style choice;
        // the test continues to pin that mixed comma/whitespace across the
        // two levels still produces the expected output.
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
        // Pre-unification workaround (cleanest): v0.4 shorthand properties.
        // `id` means `id: id`, and `address { street city }` is a shorthand
        // property whose value is a path with SubSelection. Post-unification
        // this remains the terse canonical form.
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

    // Post-unification coverage. Each of the following exercises a surface
    // that the LitObject/SubSelection unification at v0.4 either newly
    // permits (commas inside a SubSelection) or explicitly preserves
    // (the `$(...)` wrapper still works; v0.3 semantics are unchanged).

    #[test]
    fn tsh_22359_commas_all_the_way_down_at_v0_4() {
        // Post-unification: the inner `{ street: street, city: city }` is now
        // a SubSelection that accepts either separator style. Commas at every
        // level parse and produce the same output as the whitespace-inner
        // variant in `tsh_22359_fix_litobj_commas_with_subselection_spaces`.
        let sel = selection!(
            r#"
            $.results {
                ... resultType->match(
                    ["book", { __typename: "NestedBook", id, title, address: address { street: street, city: city } }],
                    ["author", { __typename: "NestedAuthor", id, name, address: address { street: street, city: city } }]
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
    fn tsh_22359_dollar_wrapper_still_works_at_v0_4() {
        // Backwards compatibility: the `$(LitExpr)` wrapper is no longer
        // required at v0.4 for top-level literal heads, but existing
        // selections that use it must continue to parse and evaluate
        // identically. This test pins a LitObject and a LitArray both inside
        // `$(...)` wrappers at v0.4, confirming they still round-trip through
        // `->match` result arms.
        let sel = selection!(
            r#"
            $.results {
                ... resultType->match(
                    ["book", $({ __typename: "FlatBook", id: id, title: title })],
                    ["author", $({ __typename: "FlatAuthor", id: id, name: name })]
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
    fn tsh_22359_nested_subselection_with_commas_fails_to_parse_at_v0_3() {
        // Spec-version parity: the original ticket input still fails to
        // parse under v0.3, because the unification is strictly additive at
        // v0.4 and the legacy LitObject/SubSelection grammar is preserved
        // for older specs.
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
            ConnectSpec::V0_3,
        );
        assert!(
            result.is_err(),
            "expected v0.3 parse error, but it parsed successfully"
        );
    }

    // Mixed-separator diagnostics: the unified v0.4 grammar requires each
    // NamedSelectionList to use commas throughout or whitespace throughout,
    // never both. The parser emits a specific fatal error pointing at the
    // offending token rather than a generic "Eof" / "trailing characters"
    // failure. These tests pin both directions of inconsistency.

    #[test]
    fn tsh_22359_mixed_separator_space_then_comma_errors_clearly() {
        let err = crate::connectors::json_selection::JSONSelection::parse_with_spec(
            "{ a b, c }",
            ConnectSpec::V0_4,
        )
        .expect_err("mixed separator should be an error");
        assert!(
            err.message.contains("separated by whitespace")
                && err.message.ends_with("Unexpected comma"),
            "expected whitespace-then-comma diagnostic ending in 'Unexpected comma', got: {}",
            err.message
        );
        // `JSONSelectionParseError`'s Display format is `"{message}: {fragment}"`,
        // so the message ending in "Unexpected comma" renders immediately
        // before the fragment. Verify the final rendering reads naturally.
        let rendered = err.to_string();
        assert!(
            rendered.ends_with("Unexpected comma: , c }"),
            "rendered error should end with the offending fragment, got: {rendered}"
        );
        // Offset points at the offending comma (inside `{ a b, c }`, byte 5).
        assert_eq!(err.offset, 5, "offset should point at the stray comma");
        assert!(
            err.fragment.starts_with(','),
            "fragment should start at the comma, got: {:?}",
            err.fragment
        );
    }

    #[test]
    fn tsh_22359_mixed_separator_comma_then_space_errors_clearly() {
        let err = crate::connectors::json_selection::JSONSelection::parse_with_spec(
            "{ a, b c }",
            ConnectSpec::V0_4,
        )
        .expect_err("mixed separator should be an error");
        assert!(
            err.message.contains("separated by commas")
                && err.message.ends_with("Missing comma before item"),
            "expected comma-then-whitespace diagnostic ending in 'Missing comma before item', got: {}",
            err.message
        );
        let rendered = err.to_string();
        assert!(
            rendered.ends_with("Missing comma before item: c }"),
            "rendered error should end with the offending item, got: {rendered}"
        );
        // Offset points at `c` (in `{ a, b c }`, `c` is at byte 7).
        assert_eq!(
            err.offset, 7,
            "offset should point at the item missing its comma"
        );
        assert!(
            err.fragment.starts_with('c'),
            "fragment should start at the offending item, got: {:?}",
            err.fragment
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
