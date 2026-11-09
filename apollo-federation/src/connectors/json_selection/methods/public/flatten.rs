use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;
use shape::location::Location;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::json_selection::methods::common::could_satisfy;
use crate::connectors::json_selection::methods::common::may_be_missing;
use crate::connectors::json_selection::methods::common::or_missing;
use crate::connectors::json_selection::methods::common::present_arg;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(FlattenMethod, flatten_method, flatten_shape);
/// "Flatten" is an array method that flattens nested arrays, like the JavaScript
/// Array.prototype.flat function. It replaces every element that is itself an
/// array with that array's elements. By default it flattens one level, and an
/// optional depth argument flattens that many levels.
///
/// For example:
///
/// $->echo([[1, 2], [3]])->flatten           result is [1, 2, 3]
/// $->echo([[[1]], [2]])->flatten            result is [[1], 2]
/// $->echo([[[1]], [2]])->flatten(2)         result is [1, 2]
///
/// Elements that are not arrays (including null and strings) are kept as they
/// are. A depth of 0 or less returns the input unchanged. Like ->map and
/// ->filter, a value that is not an array is treated as a one-element array.
/// Like most methods, it produces no value when the depth argument has none.
///
/// Combined with ->map, it flattens a list of lists produced for each element
/// of a nested structure, for example the items of every section:
///
/// sections->map(@.items)->flatten
///
/// Joining two arrays is [a, b]->flatten.
fn flatten_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();
    if arg_count > 1 {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires at most one argument, but {arg_count} were provided",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    }

    // Everything the depth argument reports stays in `errors`, which is
    // returned on every path from here on, so that diagnostics attached to a
    // valid depth (for example with ->withWarning) are not lost.
    let mut errors = Vec::new();

    let depth = match method_args.and_then(|args| args.args.first()) {
        None => 1,
        Some(depth_arg) => {
            let (value_opt, arg_errors) = depth_arg.apply_to_path(data, vars, input_path, spec);
            errors.extend(arg_errors);

            let depth_opt = match &value_opt {
                Some(JSON::Number(number)) => number.as_i64(),
                _ => None,
            };

            match depth_opt {
                Some(depth) => depth,
                // A depth with no value gives no value, with only the errors of
                // evaluating it, like the base argument of ->parseInt.
                None if value_opt.is_none() => return (None, errors),
                None => {
                    errors.push(ApplyToError::new(
                        format!("Method ->{} depth must be an integer", method_name.as_ref()),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    ));
                    return (None, errors);
                }
            }
        }
    };

    if depth <= 0 {
        return (Some(data.clone()), errors);
    }

    let JSON::Array(array) = data else {
        // Like ->map and ->filter, treat a non-array value as a one-element
        // array.
        return (Some(JSON::Array(vec![data.clone()])), errors);
    };

    let mut output = Vec::with_capacity(array.len());
    for element in array {
        flatten_into(&mut output, element, depth);
    }
    (Some(JSON::Array(output)), errors)
}

/// Appends `value` to `output`, or, if it is an array and `depth` is positive,
/// its elements flattened to `depth - 1` further levels.
fn flatten_into(output: &mut Vec<JSON>, value: &JSON, depth: i64) {
    match value {
        JSON::Array(inner) if depth > 0 => {
            for element in inner {
                flatten_into(output, element, depth - 1);
            }
        }
        other => output.push(other.clone()),
    }
}

// How many levels a call flattens, as far as the shape can tell.
enum Depth {
    Known(i64),
    Unknown,
}

fn flatten_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();
    if arg_count > 1 {
        return Shape::error(
            format!(
                "Method ->{} requires at most one argument, but {arg_count} were provided",
                method_name.as_ref(),
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    let mut maybe_missing = false;
    let depth = match method_args.and_then(|args| args.args.first()) {
        None => Depth::Known(1),
        Some(depth_arg) => {
            // The runtime evaluates the depth against the whole input, not
            // against each element, so its shape is computed the same way.
            let depth_shape =
                depth_arg.compute_output_shape(context, input_shape.clone(), dollar_shape);
            if depth_shape.has_own_errors() {
                return depth_shape;
            }
            maybe_missing = may_be_missing(&depth_shape);
            let Some(depth_shape) = present_arg(context, &depth_shape) else {
                // The method produces no value when its depth has none.
                return Shape::none();
            };
            if !could_satisfy(&Shape::int([]), &depth_shape) {
                return Shape::error(
                    format!("->{} depth must be an integer", method_name.as_ref()),
                    method_name.shape_location(context.source_id()),
                );
            }
            match depth_shape.case() {
                ShapeCase::Int(Some(depth)) => Depth::Known(*depth),
                _ => Depth::Unknown,
            }
        }
    };

    let locations: Vec<Location> = input_shape.locations().cloned().collect();
    let result = match depth {
        // Depth 0 or less returns the input unchanged, whatever it is.
        Depth::Known(depth) if depth <= 0 => input_shape,
        Depth::Known(depth) => flatten_input(&input_shape, depth, &locations),
        // A depth that is only known at runtime returns the input unchanged
        // when it is 0 or less, and an array when it is positive.
        Depth::Unknown => Shape::one(
            [
                input_shape,
                Shape::list(Shape::unknown([]), locations.iter().cloned()),
            ],
            locations.iter().cloned(),
        ),
    };
    or_missing(context, result, maybe_missing)
}

/// The shape of the result of flattening a value of shape `shape` to a known
/// `depth` of 1 or more.
fn flatten_input(shape: &Shape, depth: i64, locations: &[Location]) -> Shape {
    // Errors propagate first, and a missing value never reaches the method.
    if shape.has_own_errors() {
        return shape.clone();
    }
    match shape.case() {
        ShapeCase::None => shape.clone(),
        // Each variant is a list or not, independently of the others.
        ShapeCase::One(variants) => Shape::one(
            variants
                .iter()
                .map(|variant| flatten_input(variant, depth, locations)),
            locations.iter().cloned(),
        ),
        ShapeCase::Array { prefix, tail } => {
            let mut parts = Vec::new();
            for element in prefix.iter().chain((!tail.is_none()).then_some(tail)) {
                parts.extend(element_parts(element, depth));
            }
            if parts.is_empty() {
                // An empty array, or one whose elements all contribute nothing,
                // flattens to the empty array. The input shape would be wrong
                // for a tuple of empty arrays.
                Shape::array([], Shape::none(), locations.iter().cloned())
            } else {
                Shape::list(Shape::one(parts, []), locations.iter().cloned())
            }
        }
        // With a positive depth the result is always an array, whatever the
        // value is.
        ShapeCase::Unknown | ShapeCase::Name(_, _) | ShapeCase::All(_) => {
            Shape::list(Shape::unknown([]), locations.iter().cloned())
        }
        // Any other value is wrapped in a one-element array.
        _ => Shape::list(shape.clone(), locations.iter().cloned()),
    }
}

/// The shapes that one element of a list contributes to the flattened list,
/// when `depth` more levels may be flattened. The caller splices the result
/// into one list of parts, so nested unions and several levels add no `List`
/// wrappers.
///
/// This never calls `Shape::any_item` on an element: for a literal string that
/// would produce the union of its characters, and a string is never flattened.
fn element_parts(element: &Shape, depth: i64) -> Vec<Shape> {
    if depth <= 0 {
        return vec![element.clone()];
    }
    match element.case() {
        ShapeCase::Array { prefix, tail } => {
            let mut parts = Vec::new();
            for inner in prefix.iter().chain((!tail.is_none()).then_some(tail)) {
                parts.extend(element_parts(inner, depth - 1));
            }
            parts
        }
        ShapeCase::One(variants) => variants
            .iter()
            .flat_map(|variant| element_parts(variant, depth))
            .collect(),
        // It could be a list, or anything else.
        ShapeCase::Unknown | ShapeCase::Name(_, _) | ShapeCase::All(_) => {
            vec![Shape::unknown([])]
        }
        _ => vec![element.clone()],
    }
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::Value as JSON;
    use serde_json_bytes::json;

    use crate::connectors::ConnectSpec;
    use crate::connectors::json_selection::ApplyToError;
    use crate::selection;

    fn run(selection: &str, data: JSON) -> (Option<JSON>, Vec<ApplyToError>) {
        selection!(selection).apply_to(&data)
    }

    fn flatten(selection: &str) -> JSON {
        let (value, errors) = run(selection, json!(null));
        assert_eq!(errors, vec![], "unexpected errors for {selection}");
        value.unwrap_or_else(|| panic!("no value for {selection}"))
    }

    // The error ->flatten reports about its depth, located at the method name.
    fn depth_error(selection: &str) -> ApplyToError {
        let start = selection
            .find("flatten")
            .expect("the selection calls ->flatten");
        ApplyToError::from_json(&json!({
            "message": "Method ->flatten depth must be an integer",
            "path": ["->echo", "->flatten"],
            "range": [start, start + "flatten".len()],
            "spec": ConnectSpec::latest().to_string(),
        }))
    }

    #[test]
    fn flatten_should_flatten_one_level() {
        assert_eq!(
            selection!("$->echo([[1,2],[3]])->flatten").apply_to(&json!(null)),
            (Some(json!([1, 2, 3])), vec![]),
        );
    }

    #[test]
    fn flatten_should_flatten_only_one_level() {
        assert_eq!(flatten("$->echo([[[1]],[2]])->flatten"), json!([[1], 2]));
    }

    #[test]
    fn flatten_should_keep_mixed_elements() {
        assert_eq!(
            flatten("$->echo([[1],2,null,[3,[4]]])->flatten"),
            json!([1, 2, null, 3, [4]]),
        );
    }

    #[test]
    fn flatten_should_keep_strings_whole() {
        assert_eq!(
            flatten(r#"$->echo([["abc"],"de"])->flatten"#),
            json!(["abc", "de"]),
        );
    }

    #[test]
    fn flatten_should_drop_empty_inner_lists() {
        assert_eq!(flatten("$->echo([[],[1],[]])->flatten"), json!([1]));
        assert_eq!(flatten("$->echo([[],[]])->flatten"), json!([]));
        assert_eq!(flatten("$->echo([])->flatten"), json!([]));
    }

    #[test]
    fn flatten_should_flatten_deeper_empty_nesting_by_depth() {
        assert_eq!(flatten("$->echo([[[]]])->flatten(1)"), json!([[]]));
        assert_eq!(flatten("$->echo([[[]]])->flatten(2)"), json!([]));
    }

    #[test]
    fn flatten_should_wrap_a_non_list_input() {
        assert_eq!(flatten("$->echo(1)->flatten"), json!([1]));
        assert_eq!(flatten("$->echo({ a: 1 })->flatten"), json!([{ "a": 1 }]));
        assert_eq!(flatten("$->echo('abc')->flatten"), json!(["abc"]));
    }

    #[test]
    fn flatten_should_wrap_a_null_input() {
        // The alternative, for the maintainers: `[]`.
        assert_eq!(flatten("$->flatten"), json!([null]));
    }

    #[test]
    fn flatten_should_join_two_arrays() {
        assert_eq!(
            run("[$.a, $.b]->flatten", json!({ "a": [1], "b": [2, 3] })),
            (Some(json!([1, 2, 3])), vec![]),
        );
    }

    #[test]
    fn flatten_should_flatten_to_the_given_depth() {
        assert_eq!(flatten("$->echo([[[1]],[2]])->flatten(2)"), json!([1, 2]));
        assert_eq!(flatten("$->echo([[1]])->flatten(5)"), json!([1]));
    }

    #[test]
    fn flatten_should_return_the_input_for_a_depth_of_zero_or_less() {
        assert_eq!(flatten("$->echo([[1]])->flatten(0)"), json!([[1]]));
        assert_eq!(flatten("$->echo([[1]])->flatten(-1)"), json!([[1]]));
        // Not wrapped: the no-op holds for any input.
        assert_eq!(flatten("$->echo(1)->flatten(0)"), json!(1));
        assert_eq!(flatten("$->echo(1)->flatten(-1)"), json!(1));
    }

    #[test]
    fn flatten_should_evaluate_a_dynamic_depth_against_the_input() {
        let scalar = |depth: i64| run("$->echo(1)->flatten($.d)", json!({ "d": depth }));
        assert_eq!(scalar(2), (Some(json!([1])), vec![]));
        assert_eq!(scalar(0), (Some(json!(1)), vec![]));
        assert_eq!(scalar(-3), (Some(json!(1)), vec![]));

        let list = |depth: i64| run("$->echo([[[1]]])->flatten($.d)", json!({ "d": depth }));
        assert_eq!(list(1), (Some(json!([[1]])), vec![]));
        assert_eq!(list(2), (Some(json!([1])), vec![]));
    }

    #[test]
    fn flatten_should_error_on_a_depth_that_is_not_an_integer() {
        for selection in [
            r#"$->echo([[1]])->flatten("x")"#,
            "$->echo([[1]])->flatten(1.5)",
            "$->echo([[1]])->flatten(2.0)",
        ] {
            assert_eq!(
                run(selection, json!(null)),
                (None, vec![depth_error(selection)]),
                "{selection}",
            );
        }
    }

    #[test]
    fn flatten_should_report_only_the_lookup_error_for_a_missing_key_depth() {
        // Only the lookup error: no "depth must be an integer" is added on top.
        assert_eq!(
            run("$.xs->flatten($.nope)", json!({ "xs": [[1]] })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .nope not found in object",
                    "path": ["nope"],
                    "range": [16, 20],
                    "spec": ConnectSpec::latest().to_string(),
                }))]
            ),
        );
    }

    // Like the base argument of ->parseInt, a depth with no value gives no
    // value, with no error of ->flatten's own.
    #[test]
    fn flatten_should_produce_no_value_for_an_optional_missing_key_depth() {
        assert_eq!(
            run("$.xs->flatten($.nope?)", json!({ "xs": [[1]] })),
            (None, vec![]),
        );
    }

    #[test]
    fn flatten_should_error_on_more_than_one_argument() {
        let selection = "$->echo([[1]])->flatten(1, 2)";
        let start = selection.find("flatten").unwrap();
        assert_eq!(
            run(selection, json!(null)),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->flatten requires at most one argument, but 2 were provided",
                    "path": ["->echo", "->flatten"],
                    "range": [start, start + "flatten".len()],
                    "spec": ConnectSpec::latest().to_string(),
                }))]
            ),
        );
    }

    // A warning on a valid depth is kept on every path that returns a value: the
    // flattening, the no-op for a depth of 0, and the wrapping of a non-list.
    #[test]
    fn flatten_should_keep_the_diagnostics_of_a_valid_depth() {
        for (selection, expected) in [
            (
                r#"$->echo([[[1]],[2]])->flatten(1->withWarning("w"))"#,
                json!([[1], 2]),
            ),
            (
                r#"$->echo([[1]])->flatten(0->withWarning("w"))"#,
                json!([[1]]),
            ),
            (r#"$->echo(1)->flatten(1->withWarning("w"))"#, json!([1])),
        ] {
            let (value, errors) = run(selection, json!(null));
            assert_eq!(value, Some(expected), "{selection}");
            assert_eq!(errors.len(), 1, "{selection}: {errors:?}");
            assert_eq!(errors[0].message(), "w", "{selection}");
        }
    }

    #[test]
    fn flatten_should_put_the_depth_error_after_the_diagnostics_of_the_argument() {
        let (value, errors) = run(
            r#"$->echo([[1]])->flatten("x"->withWarning("w"))"#,
            json!(null),
        );
        assert_eq!(value, None);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(errors[0].message(), "w");
        assert_eq!(
            errors[1].message(),
            "Method ->flatten depth must be an integer"
        );
    }

    #[test]
    fn flatten_should_flatten_the_result_of_map() {
        assert_eq!(
            run(
                "sections->map(@.items)->flatten",
                json!({
                    "sections": [
                        { "name": "A", "items": [1, 2] },
                        { "name": "B", "items": [] },
                        { "name": "C", "items": [3] },
                    ],
                })
            ),
            (Some(json!([1, 2, 3])), vec![]),
        );
    }

    #[test]
    fn flatten_should_carry_parent_names_with_as() {
        assert_eq!(
            run(
                "sections->map(@->as($s)->echo(@.items->map({ id: @.id, section: $s.name })))->flatten",
                json!({
                    "sections": [
                        { "name": "S1", "items": [{ "id": 1 }] },
                        { "name": "S2", "items": [{ "id": 2 }, { "id": 3 }] },
                    ],
                })
            ),
            (
                Some(json!([
                    { "id": 1, "section": "S1" },
                    { "id": 2, "section": "S2" },
                    { "id": 3, "section": "S2" },
                ])),
                vec![]
            ),
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

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        let method_args = MethodArgs { args, range: None };
        flatten_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("flatten".to_string(), Some(location.span)),
            (!method_args.args.is_empty()).then_some(&method_args),
            input,
            Shape::unknown([]),
        )
    }

    // The pretty-printed shape of `->flatten` (or `->flatten(depth)`) on `input`.
    fn flatten(input: Shape, depth: Option<i64>) -> String {
        let args = depth
            .map(|depth| vec![WithRange::new(LitExpr::Number(depth.into()), None)])
            .unwrap_or_default();
        get_shape(args, input).pretty_print()
    }

    fn int() -> Shape {
        Shape::int([])
    }

    fn string() -> Shape {
        Shape::string([])
    }

    fn list(of: Shape) -> Shape {
        Shape::list(of, [])
    }

    fn one(shapes: impl IntoIterator<Item = Shape>) -> Shape {
        Shape::one(shapes, [])
    }

    fn literal(text: &str) -> Shape {
        Shape::string_value(text, [])
    }

    fn empty() -> Shape {
        Shape::array([], Shape::none(), [])
    }

    fn tuple(shapes: impl IntoIterator<Item = Shape>) -> Shape {
        Shape::closed_tuple(shapes, [])
    }

    #[test]
    fn flatten_shape_should_unwrap_a_list_of_lists() {
        assert_eq!(flatten(list(list(int())), None), "[...Int]");
    }

    #[test]
    fn flatten_shape_should_distribute_over_a_union_element() {
        assert_eq!(
            flatten(list(one([list(int()), string()])), None),
            "[...One<Int, String>]",
        );
        assert_eq!(
            flatten(
                list(one([list(int()), list(string()), Shape::bool([])])),
                None
            ),
            "[...One<Int, String, Bool>]",
        );
    }

    #[test]
    fn flatten_shape_should_flatten_variants_of_different_depths() {
        assert_eq!(
            flatten(list(one([list(list(int())), int()])), Some(1)),
            "[...One<[...Int], Int>]",
        );
    }

    #[test]
    fn flatten_shape_should_follow_a_literal_depth() {
        let input = list(list(list(int())));
        assert_eq!(flatten(input.clone(), Some(1)), "[...[...Int]]");
        assert_eq!(flatten(input.clone(), Some(2)), "[...Int]");
        assert_eq!(flatten(input, Some(5)), "[...Int]");
    }

    #[test]
    fn flatten_shape_should_follow_a_literal_depth_through_a_union() {
        assert_eq!(
            flatten(
                list(one([list(list(int())), list(string()), Shape::bool([])])),
                Some(2)
            ),
            "[...One<Int, String, Bool>]",
        );
    }

    #[test]
    fn flatten_shape_should_never_split_a_literal_string() {
        assert_eq!(flatten(list(literal("abc")), None), r#"[..."abc"]"#);
        assert_eq!(
            flatten(list(one([list(literal("abc")), literal("x")])), None),
            r#"[...One<"abc", "x">]"#,
        );
        assert_eq!(flatten(literal("abc"), None), r#"[..."abc"]"#);
    }

    #[test]
    fn flatten_shape_should_return_the_empty_array_for_an_empty_array() {
        assert_eq!(flatten(empty(), None), "[]");
    }

    #[test]
    fn flatten_shape_should_return_the_empty_array_for_a_tuple_of_empty_arrays() {
        assert_eq!(flatten(tuple([empty(), empty()]), None), "[]");
    }

    #[test]
    fn flatten_shape_should_keep_only_what_an_element_contributes() {
        assert_eq!(
            flatten(tuple([empty(), Shape::int_value(1, [])]), None),
            "[...1]"
        );
    }

    #[test]
    fn flatten_shape_should_keep_inner_empty_arrays_until_they_are_flattened() {
        assert_eq!(flatten(tuple([tuple([empty()])]), Some(1)), "[...[]]");
        assert_eq!(flatten(tuple([tuple([empty()])]), Some(2)), "[]");
        assert_eq!(flatten(tuple([tuple([empty()])]), Some(5)), "[]");
        assert_eq!(
            flatten(tuple([empty(), tuple([empty()])]), Some(1)),
            "[...[]]"
        );
        assert_eq!(flatten(tuple([empty(), tuple([empty()])]), Some(2)), "[]");
    }

    #[test]
    fn flatten_shape_should_return_the_empty_array_for_a_list_of_empty_arrays() {
        assert_eq!(flatten(list(empty()), None), "[]");
    }

    #[test]
    fn flatten_shape_should_distribute_over_a_union_input() {
        assert_eq!(flatten(one([list(list(int())), int()]), None), "[...Int]");
        assert_eq!(
            flatten(one([list(list(int())), Shape::none()]), None),
            "One<[...Int], None>",
        );
    }

    #[test]
    fn flatten_shape_should_wrap_a_value_that_is_not_a_list() {
        assert_eq!(flatten(int(), None), "[...Int]");
        assert_eq!(flatten(Shape::null([]), None), "[...null]");
        assert_eq!(flatten(Shape::unknown([]), None), "[...]");
    }

    #[test]
    fn flatten_shape_should_flatten_a_tuple_input() {
        assert_eq!(
            flatten(tuple([list(int()), string()]), None),
            "[...One<Int, String>]",
        );
    }

    #[test]
    fn flatten_shape_should_return_the_input_for_a_depth_of_zero_or_less() {
        assert_eq!(flatten(list(list(int())), Some(0)), "[...[...Int]]");
        assert_eq!(flatten(list(list(int())), Some(-1)), "[...[...Int]]");
        assert_eq!(flatten(int(), Some(0)), "Int");
        assert_eq!(flatten(int(), Some(-1)), "Int");
        assert_eq!(
            flatten(tuple([list(int()), string()]), Some(0)),
            "[[...Int], String]"
        );
    }

    #[test]
    fn flatten_shape_should_error_on_a_depth_that_is_not_an_integer() {
        let expected = r#"Unknown (err "->flatten depth must be an integer")"#;
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::String("x".to_string()), None)],
                list(list(int())),
            )
            .pretty_print(),
            expected,
        );
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::Number(serde_json::Number::from_f64(1.5).unwrap()),
                    None
                )],
                list(list(int())),
            )
            .pretty_print(),
            expected,
        );
    }

    #[test]
    fn flatten_shape_should_error_on_more_than_one_argument() {
        let literal = |n: i64| WithRange::new(LitExpr::Number(n.into()), None);
        assert_eq!(
            get_shape(vec![literal(1), literal(2)], list(list(int()))).pretty_print(),
            r#"Unknown (err "Method ->flatten requires at most one argument, but 2 were provided")"#,
        );
    }

    // A depth that may have no value makes the result optional from connect/v0.5,
    // and one that never has a value gives no value.
    #[rstest::rstest]
    #[case(ConnectSpec::V0_4, "One<[...[...Int]], [...]>")]
    #[case(ConnectSpec::V0_5, "One<[...[...Int]], [...], None>")]
    fn flatten_shape_should_be_optional_when_the_depth_may_be_missing(
        #[case] spec: ConnectSpec,
        #[case] expected: &str,
    ) {
        let location = get_location();
        let depth_path = LitExpr::Path(crate::connectors::PathSelection {
            path: crate::connectors::json_selection::PathList::Key(
                crate::connectors::Key::field("depth").into_with_range(),
                crate::connectors::json_selection::PathList::Empty.into_with_range(),
            )
            .into_with_range(),
        });
        let maybe_depth = Shape::one([int(), Shape::none()], []);
        let method_args = MethodArgs {
            args: vec![WithRange::new(depth_path, None)],
            range: None,
        };
        let shape = flatten_shape(
            &ShapeContext::new(location.source_id).with_spec(spec),
            &WithRange::new("flatten".to_string(), Some(location.span)),
            Some(&method_args),
            list(list(int())),
            Shape::closed_record([("depth".to_string(), maybe_depth)].into(), []),
        );
        assert_eq!(shape.pretty_print(), expected);
    }

    #[rstest::rstest]
    #[case(ConnectSpec::V0_4, "One<[...[...Int]], [...]>")]
    #[case(ConnectSpec::V0_5, "None")]
    fn flatten_shape_should_handle_a_depth_that_is_always_missing(
        #[case] spec: ConnectSpec,
        #[case] expected: &str,
    ) {
        let location = get_location();
        let dollar = LitExpr::Path(crate::connectors::PathSelection {
            path: crate::connectors::json_selection::PathList::Var(
                crate::connectors::json_selection::known_var::KnownVariable::Dollar
                    .into_with_range(),
                crate::connectors::json_selection::PathList::Empty.into_with_range(),
            )
            .into_with_range(),
        });
        let method_args = MethodArgs {
            args: vec![WithRange::new(dollar, None)],
            range: None,
        };
        let shape = flatten_shape(
            &ShapeContext::new(location.source_id).with_spec(spec),
            &WithRange::new("flatten".to_string(), Some(location.span)),
            Some(&method_args),
            list(list(int())),
            Shape::none(),
        );
        assert_eq!(shape.pretty_print(), expected);
    }
}

// The shape of selections that call ->flatten, through the real shape pipeline
// (`apply_to.rs` consults the method's shape from connect v0.3 on). The method
// is not version-gated, so one spec version is enough outside the v0.2 case.
#[cfg(test)]
mod selection_shape_tests {
    use crate::connectors::ConnectSpec;
    use crate::selection;

    fn shape(selection: &str) -> String {
        selection!(selection, ConnectSpec::V0_4)
            .shape()
            .pretty_print()
    }

    #[test]
    fn flatten_selection_shape_flattens_literal_lists() {
        assert_eq!(
            shape("items: $([[1], [2]])->flatten"),
            "{ items: [...One<1, 2>] }",
        );
        assert_eq!(
            shape(r#"items: $([["abc"], "x"])->flatten"#),
            r#"{ items: [...One<"abc", "x">] }"#,
        );
        assert_eq!(shape("items: $([[[]]])->flatten(2)"), "{ items: [] }");
    }

    #[test]
    fn flatten_selection_shape_of_an_unknown_value_is_a_list_of_unknown() {
        assert_eq!(shape("items: a->flatten"), "{ items: [...] }");
    }

    #[test]
    fn flatten_selection_shape_with_a_depth_only_known_at_runtime() {
        assert_eq!(
            shape("items: $([[1]])->flatten($.d)"),
            "{ items: One<[[1]], [...]> }",
        );
    }

    #[test]
    fn flatten_selection_shape_propagates_the_error_of_its_depth() {
        assert!(
            shape(r#"items: $([[1]])->flatten("x")"#)
                .contains("->flatten depth must be an integer")
        );
    }

    #[test]
    fn flatten_selection_shape_is_unknown_before_connect_v0_3() {
        assert_eq!(
            selection!("items: $([[1], [2]])->flatten", ConnectSpec::V0_2)
                .shape()
                .pretty_print(),
            "{ items: Unknown }",
        );
    }
}

// An end-to-end test of the pattern ->flatten exists for: a tree of sections and
// subsections, flattened into one list of items that carry their section's and
// subsection's names (with ->as), then filtered by arguments that may be null,
// and paged.
#[cfg(test)]
mod catalogue_tests {
    use apollo_compiler::collections::IndexMap;
    use serde_json_bytes::Value as JSON;
    use serde_json_bytes::json;

    use crate::selection;

    const TWO_LEVELS: &str = r#"
      $this.sections->map(@->as($s)->echo([
        @.items->map({ id: @.id, name: @.name, section: $s.name, subsection: null }),
        @.subsections->map(@->as($ss)->echo(
          @.items->map({ id: @.id, name: @.name, section: $s.name, subsection: $ss.name })
        ))->flatten
      ])->flatten)->flatten->slice($args.offset, $args.offset->add($args.first))
    "#;

    const THREE_LEVELS: &str = r#"
      $this.sections->map(@->as($s)->echo([
        @.items->map({ id: @.id, section: $s.name, subsection: null }),
        @.subsections->map(@->as($ss)->echo([
          @.items->map({ id: @.id, section: $s.name, subsection: $ss.name }),
          @.subsections->map(@->as($sss)->echo(
            @.items->map({
              id: @.id,
              section: $s.name,
              subsection: [$ss.name, $sss.name]->joinNotNull(" > "),
            })
          ))->flatten
        ])->flatten)->flatten
      ])->flatten)->flatten
    "#;

    // The nullable-argument filter, as `@` is the method's input inside the
    // argument of ->eq: with a null argument the value is compared with itself.
    const NULLABLE_FILTER: &str = r#"
      $this.sections->map(@->as($s)->echo(
        @.items->map({ id: @.id, kind: @.kind, section: $s.name })
      ))->flatten->filter(@.kind->eq($args.kind ?? @))
    "#;

    fn two_level_catalogue() -> JSON {
        json!({ "sections": [
            { "name": "S1", "items": [{ "id": 1, "name": "i1" }, { "id": 2, "name": "i2" }],
              "subsections": [
                { "name": "A", "items": [{ "id": 3, "name": "i3" }] },
                { "name": "B", "items": [{ "id": 4, "name": "i4" }, { "id": 5, "name": "i5" }] } ] },
            { "name": "S2", "items": [{ "id": 6, "name": "i6" }],
              "subsections": [{ "name": "C", "items": [{ "id": 7, "name": "i7" }] }] },
        ]})
    }

    fn run_with(selection: &str, this: JSON, args: JSON) -> JSON {
        let mut vars = IndexMap::default();
        vars.insert("$this".to_string(), this);
        vars.insert("$args".to_string(), args);
        let (result, errors) = selection!(selection.trim()).apply_with_vars(&json!(null), &vars);
        assert_eq!(errors, vec![], "unexpected errors");
        result.expect("the selection produced no value")
    }

    fn ids(items: &JSON) -> Vec<i64> {
        items
            .as_array()
            .expect("an array")
            .iter()
            .map(|item| item["id"].as_i64().expect("an integer id"))
            .collect()
    }

    #[test]
    fn the_two_level_example_flattens_in_order_with_names() {
        let items = run_with(
            TWO_LEVELS,
            two_level_catalogue(),
            json!({ "offset": 0, "first": 20 }),
        );
        assert_eq!(
            items,
            json!([
                { "id": 1, "name": "i1", "section": "S1", "subsection": null },
                { "id": 2, "name": "i2", "section": "S1", "subsection": null },
                { "id": 3, "name": "i3", "section": "S1", "subsection": "A" },
                { "id": 4, "name": "i4", "section": "S1", "subsection": "B" },
                { "id": 5, "name": "i5", "section": "S1", "subsection": "B" },
                { "id": 6, "name": "i6", "section": "S2", "subsection": null },
                { "id": 7, "name": "i7", "section": "S2", "subsection": "C" },
            ])
        );
    }

    #[test]
    fn the_two_level_example_pages() {
        // The first page is covered by the test above.
        for (offset, first, expected) in [(1, 4, vec![2, 3, 4, 5]), (9, 3, vec![])] {
            let items = run_with(
                TWO_LEVELS,
                two_level_catalogue(),
                json!({ "offset": offset, "first": first }),
            );
            assert_eq!(ids(&items), expected, "offset {offset}, first {first}");
        }
    }

    #[test]
    fn a_third_level_carries_the_subsection_path() {
        let this = json!({ "sections": [
            { "name": "S1", "items": [{ "id": 1 }],
              "subsections": [
                { "name": "A", "items": [{ "id": 2 }],
                  "subsections": [{ "name": "A1", "items": [{ "id": 3 }, { "id": 4 }] }] },
                { "name": "B", "items": [], "subsections": [{ "name": "B1", "items": [{ "id": 5 }] }] },
              ] },
            { "name": "S2", "items": [{ "id": 6 }], "subsections": [] },
        ]});
        let items = run_with(THREE_LEVELS, this, json!({}));
        assert_eq!(
            items,
            json!([
                { "id": 1, "section": "S1", "subsection": null },
                { "id": 2, "section": "S1", "subsection": "A" },
                { "id": 3, "section": "S1", "subsection": "A > A1" },
                { "id": 4, "section": "S1", "subsection": "A > A1" },
                { "id": 5, "section": "S1", "subsection": "B > B1" },
                { "id": 6, "section": "S2", "subsection": null },
            ])
        );
    }

    #[test]
    fn a_nullable_filter_combines_with_flatten() {
        let this = json!({ "sections": [
            { "name": "S1", "items": [
                { "id": 1, "kind": "ARTICLE" }, { "id": 2, "kind": "VIDEO" } ] },
            { "name": "S2", "items": [{ "id": 3, "kind": "ARTICLE" }] },
        ]});
        let filtered = |kind: JSON| {
            ids(&run_with(
                NULLABLE_FILTER,
                this.clone(),
                json!({ "kind": kind }),
            ))
        };
        assert_eq!(filtered(json!("ARTICLE")), vec![1, 3]);
        assert_eq!(filtered(json!("VIDEO")), vec![2]);
        assert_eq!(filtered(json!(null)), vec![1, 2, 3]);
    }
}
