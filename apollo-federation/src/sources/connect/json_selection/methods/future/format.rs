use apollo_compiler::collections::IndexMap;
use regex::Regex;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::helpers::vec_push;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

/// This function uses a Regex to find and replace the first {} encountered with the provided value
/// It will however not replace it if it finds {{ as it should be treated as an escape sequence
///
/// A quick breakdown of the Regex since they can be hard to read:
///     (?P<c1>         is a capture group which is capturing the character before our {
///     (^|[^{{])       is a "or" that is saying we are okay with either the start of a string "^" or NOT a "{" character
///     \{\}            Ultimately what we're looking for is {}
fn replace_next_placeholder(replaced_string: String, value: &str) -> String {
    let regex = Regex::new(r"(?P<c1>(^|[^{{]))\{\}").unwrap();
    regex
        .replace(&replaced_string, |caps: &regex::Captures| {
            format!("{}{}", &caps[1], value)
        })
        .to_string()
}

impl_arrow_method!(FormatMethod, format_method, format_shape);
fn format_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        if let Some(template_literal) = args.first() {
            // First argument will be the template string, it could be an expression!
            match template_literal.apply_to_path(data, vars, input_path) {
                (Some(JSON::String(template_string)), index_errors) => {
                    // Because each argument has `apply_to_path` called on it, we should collect the index errors and send them all back at the end
                    let mut collected_index_errors = Vec::<ApplyToError>::new();
                    collected_index_errors.extend(index_errors);

                    let mut replaced_string = template_string.as_str().to_string();

                    // We'll loop through the arguments starting at index 1 since index 0 was the template string
                    // Each argument from index 1 and on is a replace value
                    for arg in args.iter().skip(1) {
                        // Arguments could be an expression!
                        match arg.apply_to_path(data, vars, input_path) {
                            // For strings, we want to explicitly get a `str` because if we call `to_string`, it gets serialized with quotes included... E.g. hello becomes "hello" which is not what we want
                            (Some(JSON::String(value)), arg_index_errors) => {
                                collected_index_errors.extend(arg_index_errors);
                                replaced_string =
                                    replace_next_placeholder(replaced_string, value.as_str());
                            }
                            // For any other type, we should be fine to just get the serialized version of it
                            (Some(value), arg_index_errors) => {
                                collected_index_errors.extend(arg_index_errors);
                                replaced_string =
                                    replace_next_placeholder(replaced_string, &value.to_string());
                            }
                            (None, index_errors) => {
                                return (
                                    None,
                                    vec_push(
                                        index_errors,
                                        ApplyToError::new(
                                            format!(
                                                "Method ->{} received undefined argument",
                                                method_name.as_ref()
                                            ),
                                            input_path.to_vec(),
                                            template_literal.range(),
                                        ),
                                    ),
                                );
                            }
                        }
                    }
                    // Cleanup escaped {{ or }}
                    replaced_string = replaced_string.replace("{{", "{").replace("}}", "}");
                    return (
                        Some(serde_json_bytes::Value::from(replaced_string)),
                        collected_index_errors,
                    );
                }
                (Some(value), index_errors) => {
                    return (
                        None,
                        vec_push(
                            index_errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{}({}) requires a string as the first argument",
                                    method_name.as_ref(),
                                    value,
                                ),
                                input_path.to_vec(),
                                template_literal.range(),
                            ),
                        ),
                    );
                }
                (None, index_errors) => {
                    return (
                        None,
                        vec_push(
                            index_errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{} received undefined argument",
                                    method_name.as_ref()
                                ),
                                input_path.to_vec(),
                                template_literal.range(),
                            ),
                        ),
                    );
                }
            };
        }
    }

    (
        None,
        vec![ApplyToError::new(
            format!(
                "Method ->{} requires two or more arguments",
                method_name.as_ref()
            ),
            input_path.to_vec(),
            method_name.range(),
        )],
    )
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn format_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        return first_arg.compute_output_shape(
            input_shape,
            dollar_shape,
            named_var_shapes,
            source_id,
        );
    }
    Shape::error(
        format!(
            "Method ->{} requires two or more arguments",
            method_name.as_ref()
        ),
        method_name.shape_location(source_id),
    )
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn format_should_replace_placeholder_with_string() {
        assert_eq!(
            selection!("$->format('testing {}', 'lol')").apply_to(&json!(null)),
            (Some(json!("testing lol")), vec![]),
        );
    }

    #[test]
    fn format_should_replace_placeholder_with_number() {
        assert_eq!(
            selection!("$->format('testing {}', 5)").apply_to(&json!(null)),
            (Some(json!("testing 5")), vec![]),
        );
    }

    #[test]
    fn format_should_replace_placeholder_from_expression() {
        assert_eq!(
            selection!("$->format('testing {}', $->get(0))").apply_to(&json!("hello")),
            (Some(json!("testing h")), vec![]),
        );
    }

    #[test]
    fn format_should_replace_placeholder_when_template_is_expression() {
        assert_eq!(
            selection!("$->format($->echo('testing {}'), 'lol')").apply_to(&json!(null)),
            (Some(json!("testing lol")), vec![]),
        );
    }

    #[test]
    fn format_should_replace_multiple_placeholders() {
        assert_eq!(
            selection!("$->format('{} testing {}', 'before', 'after')").apply_to(&json!(null)),
            (Some(json!("before testing after")), vec![]),
        );
    }

    #[test]
    fn format_should_not_replace_handing_braces() {
        assert_eq!(
            selection!("$->format('{ this is a test }', 'lol')").apply_to(&json!(null)),
            (Some(json!("{ this is a test }")), vec![]),
        );
    }

    #[test]
    fn format_should_escape_double_braces() {
        assert_eq!(
            selection!("$->format('testing {{}}', 'lol')").apply_to(&json!(null)),
            (Some(json!("testing {}")), vec![]),
        );
    }

    #[test]
    fn format_should_replace_and_escape_double_braces() {
        assert_eq!(
            selection!("$->format('testing {{}} {} {{}}', 'lol')").apply_to(&json!(null)),
            (Some(json!("testing {} lol {}")), vec![]),
        );
    }

    #[test]
    fn format_should_escape_double_braces_at_beginning_of_string() {
        assert_eq!(
            selection!("$->format('{{}} testing', 'lol')").apply_to(&json!(null)),
            (Some(json!("{} testing")), vec![]),
        );
    }

    #[test]
    fn format_should_replace_at_beginning_of_string() {
        assert_eq!(
            selection!("$->format('{} testing', 'lol')").apply_to(&json!(null)),
            (Some(json!("lol testing")), vec![]),
        );
    }

    #[test]
    fn match_should_error_when_no_args_provided() {
        let result = selection!("$->format").apply_to(&json!(null));

        assert!(
            result
                .1
                .iter()
                .any(|e| e.message() == "Method ->format requires two or more arguments")
        );
    }

    #[test]
    fn match_should_error_when_first_arg_is_not_string() {
        let result = selection!("$->format(5, 'lol')").apply_to(&json!(null));

        assert!(
            result.1.iter().any(
                |e| e.message() == "Method ->format(5) requires a string as the first argument"
            )
        );
    }
}
