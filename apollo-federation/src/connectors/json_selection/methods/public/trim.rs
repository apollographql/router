use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_type_name;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

#[derive(Clone, Copy)]
enum TrimMode {
    Both,
    Start,
    End,
}

impl_arrow_method!(TrimMethod, trim_method, trim_shape);
/// Removes leading and trailing Unicode whitespace from a string, matching
/// Rust's `str::trim` semantics (locale-independent, uses the Unicode
/// `White_Space` derived core property).
///
/// $('  hello  ')->trim     results in "hello"
/// $('\t\nfoo\n\t')->trim   results in "foo"
/// $('abc')->trim           results in "abc"
fn trim_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    apply_trim(
        TrimMode::Both,
        method_name,
        method_args,
        data,
        input_path,
        spec,
    )
}

impl_arrow_method!(TrimStartMethod, trim_start_method, trim_shape);
/// Removes leading Unicode whitespace from a string, matching Rust's
/// `str::trim_start` semantics.
///
/// $('  hello  ')->trimStart    results in "hello  "
fn trim_start_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    apply_trim(
        TrimMode::Start,
        method_name,
        method_args,
        data,
        input_path,
        spec,
    )
}

impl_arrow_method!(TrimEndMethod, trim_end_method, trim_shape);
/// Removes trailing Unicode whitespace from a string, matching Rust's
/// `str::trim_end` semantics.
///
/// $('  hello  ')->trimEnd      results in "  hello"
fn trim_end_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    apply_trim(
        TrimMode::End,
        method_name,
        method_args,
        data,
        input_path,
        spec,
    )
}

fn apply_trim(
    mode: TrimMode,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if method_args.is_some() {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} does not take any arguments",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    }

    let JSON::String(input_str) = data else {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires a string input, got {}",
                    method_name.as_ref(),
                    json_type_name(data)
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    let s = input_str.as_str();
    let trimmed = match mode {
        TrimMode::Both => s.trim(),
        TrimMode::Start => s.trim_start(),
        TrimMode::End => s.trim_end(),
    };

    let output = if trimmed.len() == s.len() {
        input_str.clone()
    } else {
        trimmed.to_string().into()
    };
    (Some(JSON::String(output)), vec![])
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn trim_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    let location = method_name.shape_location(context.source_id());

    if method_args.is_some() {
        return Shape::error(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            location,
        );
    }

    // Input must be a string.
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

    Shape::string(method_name.shape_location(context.source_id()))
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    // --- ->trim: basic cases ---

    #[rstest::rstest]
    #[case(json!("  hello  "), json!("hello"))]
    #[case(json!("hello"), json!("hello"))]
    #[case(json!(""), json!(""))]
    #[case(json!("   "), json!(""))]
    #[case(json!("\t\n foo \n\t"), json!("foo"))]
    #[case(json!("  a  b  "), json!("a  b"))]
    fn trim_basic(
        #[case] input: serde_json_bytes::Value,
        #[case] expected: serde_json_bytes::Value,
    ) {
        assert_eq!(
            selection!("$->trim").apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    // --- ->trimStart: basic cases ---

    #[rstest::rstest]
    #[case(json!("  hello  "), json!("hello  "))]
    #[case(json!("hello  "), json!("hello  "))]
    #[case(json!("  hello"), json!("hello"))]
    #[case(json!(""), json!(""))]
    #[case(json!("   "), json!(""))]
    #[case(json!("\t\n foo"), json!("foo"))]
    fn trim_start_basic(
        #[case] input: serde_json_bytes::Value,
        #[case] expected: serde_json_bytes::Value,
    ) {
        assert_eq!(
            selection!("$->trimStart").apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    // --- ->trimEnd: basic cases ---

    #[rstest::rstest]
    #[case(json!("  hello  "), json!("  hello"))]
    #[case(json!("  hello"), json!("  hello"))]
    #[case(json!("hello  "), json!("hello"))]
    #[case(json!(""), json!(""))]
    #[case(json!("   "), json!(""))]
    #[case(json!("foo \n\t"), json!("foo"))]
    fn trim_end_basic(
        #[case] input: serde_json_bytes::Value,
        #[case] expected: serde_json_bytes::Value,
    ) {
        assert_eq!(
            selection!("$->trimEnd").apply_to(&input),
            (Some(expected), vec![]),
        );
    }

    // --- Unicode whitespace ---

    #[test]
    fn trim_handles_unicode_whitespace() {
        // U+00A0 NO-BREAK SPACE is Unicode whitespace and should be stripped.
        assert_eq!(
            selection!("$->trim").apply_to(&json!("\u{00A0}hello\u{00A0}")),
            (Some(json!("hello")), vec![]),
        );
        // U+2003 EM SPACE likewise.
        assert_eq!(
            selection!("$->trim").apply_to(&json!("\u{2003}hello\u{2003}")),
            (Some(json!("hello")), vec![]),
        );
    }

    #[test]
    fn trim_preserves_interior_content() {
        assert_eq!(
            selection!("$->trim").apply_to(&json!("  café  ")),
            (Some(json!("café")), vec![]),
        );
    }

    // --- Errors: non-string input ---

    #[rstest::rstest]
    #[case(json!(42), "number")]
    #[case(json!(true), "boolean")]
    #[case(json!(null), "null")]
    #[case(json!([1, 2, 3]), "array")]
    #[case(json!({"key": "value"}), "object")]
    fn trim_errors_on_non_string_input(
        #[case] input: serde_json_bytes::Value,
        #[case] expected_type: &str,
    ) {
        let result = selection!("$->trim").apply_to(&input);
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains(&format!("requires a string input, got {expected_type}")),
            "actual: {}",
            result.1[0].message()
        );
    }

    #[rstest::rstest]
    #[case(json!(42), "number")]
    #[case(json!(null), "null")]
    fn trim_start_errors_on_non_string_input(
        #[case] input: serde_json_bytes::Value,
        #[case] expected_type: &str,
    ) {
        let result = selection!("$->trimStart").apply_to(&input);
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains(&format!("requires a string input, got {expected_type}"))
        );
    }

    #[rstest::rstest]
    #[case(json!(42), "number")]
    #[case(json!(null), "null")]
    fn trim_end_errors_on_non_string_input(
        #[case] input: serde_json_bytes::Value,
        #[case] expected_type: &str,
    ) {
        let result = selection!("$->trimEnd").apply_to(&input);
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains(&format!("requires a string input, got {expected_type}"))
        );
    }

    // --- Errors: unexpected arguments ---

    #[rstest::rstest]
    #[case("$->trim('x')")]
    #[case("$->trim()")]
    #[case("$->trimStart('x')")]
    #[case("$->trimStart()")]
    #[case("$->trimEnd('x')")]
    #[case("$->trimEnd()")]
    fn trim_errors_on_any_argument(#[case] expr: &str) {
        let result = selection!(expr).apply_to(&json!("  hi  "));
        assert!(result.0.is_none());
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains("does not take any arguments"),
            "actual: {}",
            result.1[0].message()
        );
    }

    // --- Property access and chaining ---

    #[test]
    fn trim_from_data_property() {
        let data = json!({"name": "  Apollo  "});
        assert_eq!(
            selection!("name->trim").apply_to(&data),
            (Some(json!("Apollo")), vec![]),
        );
    }

    #[test]
    fn trim_chains_with_other_methods() {
        // Trim the padding, then take the size of what remains.
        assert_eq!(
            selection!("$->trim->size").apply_to(&json!("  hello  ")),
            (Some(json!(5)), vec![]),
        );
    }

    #[test]
    fn trim_start_end_compose_to_trim() {
        assert_eq!(
            selection!("$->trimStart->trimEnd").apply_to(&json!("  hello  ")),
            (Some(json!("hello")), vec![]),
        );
    }
}
