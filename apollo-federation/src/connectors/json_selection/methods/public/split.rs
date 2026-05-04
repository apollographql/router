use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_type_name;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(SplitMethod, split_method, split_shape);
/// Splits a string into an array of substrings using a separator string,
/// analogous to JavaScript's `String.prototype.split`.
///
/// $('a,b,c')->split(',')         results in ["a", "b", "c"]
/// $('hello')->split('')          results in ["h", "e", "l", "l", "o"]
/// $('a,b,c')->split(',', 2)      results in ["a", "b"]
/// $('abc')->split('x')           results in ["abc"]
fn split_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let mut errors = Vec::new();

    // Require at least one argument (the separator)
    let Some(MethodArgs { args, .. }) = method_args else {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires a separator argument",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    let Some(separator_arg) = args.first() else {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires a separator argument",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    // Evaluate the separator argument
    let (separator_opt, sep_errors) = separator_arg.apply_to_path(data, vars, input_path, spec);
    errors.extend(sep_errors);

    let separator: &str = match separator_opt.as_ref() {
        Some(JSON::String(s)) => s.as_str(),
        Some(other) => {
            errors.push(ApplyToError::new(
                format!(
                    "Method ->{} requires a string separator, got {}",
                    method_name.as_ref(),
                    json_type_name(other)
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            ));
            return (None, errors);
        }
        None => {
            errors.push(ApplyToError::new(
                format!(
                    "Method ->{} requires a string separator, but received null",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            ));
            return (None, errors);
        }
    };

    // Evaluate the optional limit argument
    let limit: Option<usize> = if let Some(limit_arg) = args.get(1) {
        let (limit_opt, limit_errors) = limit_arg.apply_to_path(data, vars, input_path, spec);
        errors.extend(limit_errors);
        match limit_opt {
            Some(JSON::Number(n)) => match n.as_u64() {
                Some(n) => Some(n as usize),
                None => {
                    errors.push(ApplyToError::new(
                        format!(
                            "Method ->{} limit argument must be a non-negative integer, got {n}",
                            method_name.as_ref(),
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    ));
                    return (None, errors);
                }
            },
            Some(other) => {
                errors.push(ApplyToError::new(
                    format!(
                        "Method ->{} limit argument must be a non-negative integer, got {}",
                        method_name.as_ref(),
                        json_type_name(&other)
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                ));
                return (None, errors);
            }
            None => None,
        }
    } else {
        None
    };

    // Validate no extra arguments
    if args.len() > 2 {
        errors.push(ApplyToError::new(
            format!(
                "Method ->{} accepts at most 2 arguments (separator, limit), got {}",
                method_name.as_ref(),
                args.len()
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
        return (None, errors);
    }

    // Input must be a string
    let JSON::String(input_str) = data else {
        errors.push(ApplyToError::new(
            format!(
                "Method ->{} requires a string input, got {}",
                method_name.as_ref(),
                json_type_name(data)
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
        return (None, errors);
    };

    let s = input_str.as_str();

    // Empty separator splits into individual characters
    // (matching JS behavior where "abc".split("") gives ["a", "b", "c"]),
    // unlike Rust's split("") which yields empty strings at boundaries.
    let parts: Vec<&str> = if separator.is_empty() {
        s.char_indices()
            .map(|(i, c)| &s[i..i + c.len_utf8()])
            .collect()
    } else {
        s.split(separator).collect()
    };

    // Apply limit if provided
    let parts: Vec<&str> = match limit {
        Some(n) => parts.into_iter().take(n).collect(),
        None => parts,
    };

    let result: Vec<JSON> = parts
        .into_iter()
        .map(|part| JSON::String(part.to_string().into()))
        .collect();

    (Some(JSON::Array(result)), errors)
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn split_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let location = method_name.shape_location(context.source_id());

    // Validate that separator argument exists
    let Some(args) = method_args else {
        return Shape::error(
            format!(
                "Method ->{} requires a separator argument",
                method_name.as_ref()
            ),
            location,
        );
    };

    let Some(separator_arg) = args.args.first() else {
        return Shape::error(
            format!(
                "Method ->{} requires a separator argument",
                method_name.as_ref()
            ),
            location,
        );
    };

    // Validate argument count
    if args.args.len() > 2 {
        return Shape::error(
            format!(
                "Method ->{} accepts at most 2 arguments (separator, limit), got {}",
                method_name.as_ref(),
                args.args.len()
            ),
            location,
        );
    }

    // Validate separator argument shape
    let sep_shape =
        separator_arg.compute_output_shape(context, input_shape.clone(), dollar_shape.clone());
    if !(sep_shape.is_unknown() || matches!(sep_shape.case(), ShapeCase::Name(_, _))) {
        let mismatches = Shape::string([]).validate(&sep_shape);
        if mismatches.is_some() {
            return Shape::error(
                format!(
                    "Method ->{} requires a string separator",
                    method_name.as_ref()
                ),
                location,
            );
        }
    }

    // Validate limit argument shape if present
    if let Some(limit_arg) = args.args.get(1) {
        let limit_shape =
            limit_arg.compute_output_shape(context, input_shape.clone(), dollar_shape);
        if !(limit_shape.is_unknown() || matches!(limit_shape.case(), ShapeCase::Name(_, _))) {
            let mismatches = Shape::int([]).validate(&limit_shape);
            if mismatches.is_some() {
                return Shape::error(
                    format!(
                        "Method ->{} limit argument must be a non-negative integer",
                        method_name.as_ref()
                    ),
                    location,
                );
            }
        }
    }

    // Validate input shape
    if !(input_shape.is_unknown() || matches!(input_shape.case(), ShapeCase::Name(_, _))) {
        let mismatches = Shape::string([]).validate(&input_shape);
        if mismatches.is_some() {
            return Shape::error(
                format!("Method ->{} requires a string input", method_name.as_ref()),
                input_shape
                    .locations()
                    .cloned()
                    .chain(method_name.shape_location(context.source_id())),
            );
        }
    }

    // split always returns an array of strings
    Shape::list(
        Shape::string([]),
        method_name.shape_location(context.source_id()),
    )
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    // --- Basic splitting ---

    #[rstest::rstest]
    #[case(json!("a,b,c"), ",", json!(["a", "b", "c"]))]
    #[case(json!("hello world"), " ", json!(["hello", "world"]))]
    #[case(json!("one::two::three"), "::", json!(["one", "two", "three"]))]
    #[case(json!("no-match"), ",", json!(["no-match"]))]
    #[case(json!(",a,b,"), ",", json!(["", "a", "b", ""]))]
    #[case(json!(",,"), ",", json!(["", "", ""]))]
    #[case(json!(""), ",", json!([""]))]
    #[case(json!("abc"), "abc", json!(["", ""]))]
    fn split_basic(#[case] input: JSON, #[case] separator: &str, #[case] expected: JSON) {
        assert_eq!(
            selection!(&format!("$->split('{separator}')")).apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    // --- Empty separator (split into characters) ---

    #[rstest::rstest]
    #[case(json!("hello"), json!(["h", "e", "l", "l", "o"]))]
    #[case(json!(""), json!([]))]
    #[case(json!("a"), json!(["a"]))]
    fn split_empty_separator(#[case] input: JSON, #[case] expected: JSON) {
        assert_eq!(
            selection!("$->split('')").apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    // --- Unicode support ---

    #[rstest::rstest]
    #[case(json!("café"), "", json!(["c", "a", "f", "é"]))]
    #[case(json!("a🎉b🎉c"), "🎉", json!(["a", "b", "c"]))]
    fn split_unicode(#[case] input: JSON, #[case] separator: &str, #[case] expected: JSON) {
        assert_eq!(
            selection!(&format!("$->split('{separator}')")).apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    // --- Limit parameter ---

    #[rstest::rstest]
    #[case(json!("a,b,c,d"), ",", 2, json!(["a", "b"]))]
    #[case(json!("a,b,c,d"), ",", 0, json!([]))]
    #[case(json!("a,b,c"), ",", 10, json!(["a", "b", "c"]))]
    #[case(json!("a,b,c"), ",", 1, json!(["a"]))]
    fn split_with_limit(
        #[case] input: JSON,
        #[case] separator: &str,
        #[case] limit: usize,
        #[case] expected: JSON,
    ) {
        assert_eq!(
            selection!(&format!("$->split('{separator}', {limit})")).apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    // --- Error cases ---

    #[rstest::rstest]
    #[case(json!(42), "number")]
    #[case(json!(true), "boolean")]
    #[case(json!(null), "null")]
    #[case(json!([1, 2, 3]), "array")]
    #[case(json!({"key": "value"}), "object")]
    fn split_should_error_on_non_string_input(#[case] input: JSON, #[case] expected_type: &str) {
        let result = selection!("$->split(',')").apply_to(&input);
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains(&format!("requires a string input, got {expected_type}"))
        );
    }

    #[test]
    fn split_should_error_without_arguments() {
        let result = selection!("$->split").apply_to(&json!("a,b"));
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(result.1[0].message().contains("requires a separator"));
    }

    #[test]
    fn split_should_error_with_empty_parens() {
        let result = selection!("$->split()").apply_to(&json!("a,b"));
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(result.1[0].message().contains("requires a separator"));
    }

    #[test]
    fn split_should_error_with_non_string_separator() {
        let result = selection!("$->split(42)").apply_to(&json!("a,b"));
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains("requires a string separator, got number")
        );
    }

    #[test]
    fn split_should_error_with_too_many_arguments() {
        let result = selection!("$->split(',', 2, 3)").apply_to(&json!("a,b,c"));
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains("accepts at most 2 arguments")
        );
    }

    #[test]
    fn split_should_error_with_negative_limit() {
        let result = selection!("$->split(',', -1)").apply_to(&json!("a,b,c"));
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains("must be a non-negative integer")
        );
    }

    #[test]
    fn split_should_error_with_float_limit() {
        let result = selection!("$->split(',', 2.5)").apply_to(&json!("a,b,c"));
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains("must be a non-negative integer")
        );
    }

    // --- Round-trip with joinNotNull ---

    #[test]
    fn split_then_join_roundtrip() {
        assert_eq!(
            selection!("$->split(',')->joinNotNull(',')").apply_to(&json!("a,b,c")),
            (Some(json!("a,b,c")), vec![]),
        );
    }

    // --- Variable and property access ---

    #[test]
    fn split_from_data_property() {
        let data = json!({"tags": "rust,graphql,apollo"});
        assert_eq!(
            selection!("tags->split(',')").apply_to(&data),
            (Some(json!(["rust", "graphql", "apollo"])), vec![]),
        );
    }

    #[test]
    fn split_with_dynamic_separator() {
        let data = json!({"text": "a|b|c", "sep": "|"});
        assert_eq!(
            selection!("text->split($.sep)").apply_to(&data),
            (Some(json!(["a", "b", "c"])), vec![]),
        );
    }
}
