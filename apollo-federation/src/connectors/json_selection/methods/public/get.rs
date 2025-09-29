use serde_json_bytes::ByteString;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::vec_push;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::lit_expr::LitExpr;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(GetMethod, get_method, get_shape);
/// For a string, gets the char at the specified index.
/// For an array, gets the item at the specified index.
/// For an object, gets the property with the specified name.
///
/// Examples:
/// $->echo("hello")->get(0)                    returns "h"
/// $->echo([1,2,3])->get(0)                    returns 1
/// $->echo({"a": "hello"})->get("a")           returns "hello"
fn get_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(index_literal) = method_args.and_then(|MethodArgs { args, .. }| args.first()) else {
        return (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires an argument", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    match data {
        JSON::String(input_value) => handle_string_method(
            method_name,
            index_literal,
            input_value,
            vars,
            input_path,
            data,
            spec,
        ),
        JSON::Array(input_value) => handle_array_method(
            method_name,
            index_literal,
            input_value,
            vars,
            input_path,
            data,
            spec,
        ),
        JSON::Object(input_value) => handle_object_method(
            method_name,
            index_literal,
            input_value,
            vars,
            input_path,
            data,
            spec,
        ),
        _ => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} must be applied to a string, array, or object",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        ),
    }
}

fn handle_string_method(
    method_name: &WithRange<String>,
    index_literal: &WithRange<LitExpr>,
    input_value: &ByteString,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    data: &JSON,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let (index, index_apply_to_errors) = index_literal.apply_to_path(data, vars, input_path, spec);

    match index {
        Some(JSON::Number(index_value)) => {
            let Some(index_value) = index_value.as_i64() else {
                return (
                    None,
                    vec_push(
                        index_apply_to_errors,
                        ApplyToError::new(
                            format!(
                                "Method ->{} failed to convert number index to integer",
                                method_name.as_ref()
                            ),
                            input_path.to_vec(),
                            method_name.range(),
                            spec,
                        ),
                    ),
                );
            };

            // Create this error "just in time" to avoid unneeded memory allocation but also allows us to capture current index_value before it is manipulated for negative indexes
            let out_of_bounds_error = |index_apply_to_errors| {
                vec_push(
                    index_apply_to_errors,
                    ApplyToError::new(
                        format!(
                            "Method ->{} index {index_value} out of bounds in string of length {}",
                            method_name.as_ref(),
                            input_value.as_str().len()
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    ),
                )
            };

            if let Some(value) = get_string(index_value, input_value.as_str()) {
                (Some(JSON::String(value.into())), index_apply_to_errors)
            } else {
                (None, out_of_bounds_error(index_apply_to_errors))
            }
        }
        Some(index_value) => (
            None,
            vec_push(
                index_apply_to_errors,
                ApplyToError::new(
                    format!(
                        "Method ->{} on a string requires a integer index, got {index_value}",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                ),
            ),
        ),
        None => (None, index_apply_to_errors),
    }
}

fn get_string(index: i64, value: &str) -> Option<&str> {
    let index_value = if index < 0 {
        value.len() as i64 + index
    } else {
        index
    };

    if index_value < 0 {
        return None;
    }

    let index_value = index_value as usize;
    value.get(index_value..=index_value)
}

fn handle_array_method(
    method_name: &WithRange<String>,
    index_literal: &WithRange<LitExpr>,
    input_value: &[JSON],
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    data: &JSON,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let (index, index_apply_to_errors) = index_literal.apply_to_path(data, vars, input_path, spec);

    match index {
        Some(JSON::Number(index_value)) => {
            let Some(index_value) = index_value.as_i64() else {
                return (
                    None,
                    vec_push(
                        index_apply_to_errors,
                        ApplyToError::new(
                            format!(
                                "Method ->{} failed to convert number index to integer",
                                method_name.as_ref()
                            ),
                            input_path.to_vec(),
                            method_name.range(),
                            spec,
                        ),
                    ),
                );
            };

            // Create this error "just in time" to avoid unneeded memory allocation but also allows us to capture current index_value before it is manipulated for negative indexes
            let out_of_bounds_error = |index_apply_to_errors| {
                vec_push(
                    index_apply_to_errors,
                    ApplyToError::new(
                        format!(
                            "Method ->{} index {index_value} out of bounds in array of length {}",
                            method_name.as_ref(),
                            input_value.len()
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    ),
                )
            };

            // Negative values should count from the back of the string so we add it to the length when it is negative
            if let Some(value) = get_array(index_value, input_value) {
                (Some(value.clone()), index_apply_to_errors)
            } else {
                (None, out_of_bounds_error(index_apply_to_errors))
            }
        }
        Some(index_value) => (
            None,
            vec_push(
                index_apply_to_errors,
                ApplyToError::new(
                    format!(
                        "Method ->{} on an array requires a integer index, got {index_value}",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                ),
            ),
        ),
        None => (None, index_apply_to_errors),
    }
}

fn get_array<T>(index: i64, array: &[T]) -> Option<&T> {
    let index = if index < 0 {
        array.len() as i64 + index
    } else {
        index
    };

    if index < 0 {
        return None;
    }

    let index = index as usize;
    array.get(index)
}

fn handle_object_method(
    method_name: &WithRange<String>,
    index_literal: &WithRange<LitExpr>,
    input_value: &serde_json_bytes::Map<ByteString, JSON>,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    data: &JSON,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let (index, index_apply_to_errors) = index_literal.apply_to_path(data, vars, input_path, spec);

    match index {
        Some(JSON::String(index_value)) => {
            let index_value = index_value.as_str();

            if let Some(value) = input_value.get(index_value) {
                (Some(value.clone()), index_apply_to_errors)
            } else {
                (
                    None,
                    vec_push(
                        index_apply_to_errors,
                        ApplyToError::new(
                            format!(
                                "Method ->{} property {index_value} not found in object",
                                method_name.as_ref()
                            ),
                            input_path.to_vec(),
                            method_name.range(),
                            spec,
                        ),
                    ),
                )
            }
        }
        Some(index_value) => (
            None,
            vec_push(
                index_apply_to_errors,
                ApplyToError::new(
                    format!(
                        "Method ->{} on an object requires a string index, got {index_value}",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                ),
            ),
        ),
        None => (None, index_apply_to_errors),
    }
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn get_shape(
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
                "Method ->{} requires only one argument, but {arg_count} were provided",
                method_name.as_ref(),
            ),
            vec![],
        );
    }

    let Some(index_literal) = method_args.and_then(|args| args.args.first()) else {
        return Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(context.source_id()),
        );
    };

    let index_shape =
        index_literal.compute_output_shape(context, input_shape.clone(), dollar_shape);

    if Shape::string([]).accepts(&input_shape) {
        handle_string_shape(method_name, &input_shape, &index_shape, context.source_id())
    } else if Shape::tuple([], []).accepts(&input_shape) {
        handle_array_shape(method_name, &input_shape, &index_shape, context.source_id())
    } else if Shape::empty_object([]).accepts(&input_shape) {
        handle_object_shape(method_name, &input_shape, &index_shape, context.source_id())
    } else if input_shape.accepts(&Shape::unknown([])) {
        handle_unknown_shape(method_name, &index_shape, context.source_id())
    } else {
        Shape::error(
            format!(
                "Method ->{} must be applied to a string, array, or object",
                method_name.as_ref()
            )
            .as_str(),
            method_name.shape_location(context.source_id()),
        )
    }
}

fn handle_string_shape(
    method_name: &WithRange<String>,
    input_shape: &Shape,
    index_shape: &Shape,
    source_id: &SourceId,
) -> Shape {
    // Handle Strings: Get a character at a integer index
    let index_value = if Shape::int([]).accepts(index_shape) {
        let ShapeCase::Int(Some(index_value)) = index_shape.case() else {
            return Shape::string(method_name.shape_location(source_id));
        };
        index_value
    } else if index_shape.accepts(&Shape::unknown([])) {
        return Shape::string(method_name.shape_location(source_id));
    } else {
        return Shape::error(
            format!(
                "Method ->{} must be provided an integer argument when applied to a string",
                method_name.as_ref()
            )
            .as_str(),
            method_name.shape_location(source_id),
        );
    };

    let ShapeCase::String(Some(input_value)) = input_shape.case() else {
        return Shape::string(method_name.shape_location(source_id));
    };

    let out_of_bounds_error = || {
        Shape::error(
            format!(
                "Method ->{} index {index_value} out of bounds in string of length {}",
                method_name.as_ref(),
                input_value.len()
            )
            .as_str(),
            method_name.shape_location(source_id),
        )
    };

    if let Some(value) = get_string(*index_value, input_value) {
        Shape::string_value(value, method_name.shape_location(source_id))
    } else {
        out_of_bounds_error()
    }
}

fn handle_array_shape(
    method_name: &WithRange<String>,
    input_shape: &Shape,
    index_shape: &Shape,
    source_id: &SourceId,
) -> Shape {
    // Handle Arrays: Get an array item at a integer index
    let index_value = if Shape::int([]).accepts(index_shape) {
        let ShapeCase::Int(Some(index_value)) = index_shape.case() else {
            return input_shape.any_item(method_name.shape_location(source_id));
        };
        index_value
    } else if index_shape.accepts(&Shape::unknown([])) {
        return input_shape.any_item(method_name.shape_location(source_id));
    } else {
        return Shape::error(
            format!(
                "Method ->{} must be provided an integer argument when applied to an array",
                method_name.as_ref()
            )
            .as_str(),
            method_name.shape_location(source_id),
        );
    };

    let ShapeCase::Array { prefix, tail } = input_shape.case() else {
        return input_shape.any_item(method_name.shape_location(source_id));
    };

    let out_of_bounds_error = || {
        Shape::error(
            format!(
                "Method ->{} index {index_value} out of bounds in array of length {}",
                method_name.as_ref(),
                prefix.len()
            )
            .as_str(),
            method_name.shape_location(source_id),
        )
    };

    if let Some(item) = get_array(*index_value, prefix) {
        item.clone()
    } else if !tail.is_none() {
        // If we have a tail, we cannot know for sure if the item exists at the index or not
        // This is because a tail implies that there are 0 to many items of that type
        input_shape.any_item(method_name.shape_location(source_id))
    } else {
        out_of_bounds_error()
    }
}

fn handle_object_shape(
    method_name: &WithRange<String>,
    input_shape: &Shape,
    index_shape: &Shape,
    source_id: &SourceId,
) -> Shape {
    // Handle Objects: Get an object property at a string index
    let index_value = if Shape::string([]).accepts(index_shape) {
        let ShapeCase::String(Some(index_value)) = index_shape.case() else {
            return input_shape.any_field(method_name.shape_location(source_id));
        };
        index_value
    } else if index_shape.accepts(&Shape::unknown([])) {
        return input_shape.any_field(method_name.shape_location(source_id));
    } else {
        return Shape::error(
            format!(
                "Method ->{} must be provided an string argument when applied to an object",
                method_name.as_ref()
            )
            .as_str(),
            method_name.shape_location(source_id),
        );
    };

    let ShapeCase::Object { fields, rest } = input_shape.case() else {
        return input_shape.any_field(method_name.shape_location(source_id));
    };

    if let Some(item) = fields.get(index_value) {
        item.clone()
    } else if !rest.is_none() {
        // If we have a rest, we cannot know for sure if the item exists at the index or not
        // This is because a rest implies that there are 0 to many items of that type
        input_shape.any_field(method_name.shape_location(source_id))
    } else {
        Shape::error(
            format!(
                "Method ->{} property {index_value} not found in object",
                method_name.as_ref()
            )
            .as_str(),
            method_name.shape_location(source_id),
        )
    }
}

fn handle_unknown_shape(
    method_name: &WithRange<String>,
    index_shape: &Shape,
    source_id: &SourceId,
) -> Shape {
    if Shape::int([]).accepts(index_shape)
        || index_shape.accepts(&Shape::unknown([]))
        || Shape::string([]).accepts(index_shape)
    {
        Shape::unknown(method_name.shape_location(source_id))
    } else {
        Shape::error(
            format!(
                "Method ->{} must be provided an integer or string argument",
                method_name.as_ref()
            )
            .as_str(),
            method_name.shape_location(source_id),
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::assert_debug_snapshot;
    use crate::connectors::json_selection::ApplyToError;
    use crate::selection;

    #[test]
    fn get_should_return_item_from_array_at_specified_index() {
        assert_eq!(
            selection!("$->get(1)").apply_to(&json!([1, 2, 3])),
            (Some(json!(2)), vec![]),
        );
    }

    #[test]
    fn get_should_return_item_from_array_at_specified_negative_index() {
        assert_eq!(
            selection!("$->get(-1)").apply_to(&json!([1, 2, 3])),
            (Some(json!(3)), vec![]),
        );
    }

    #[test]
    fn get_should_return_item_from_arrays_when_used_in_map() {
        assert_eq!(
            selection!("numbers->map(@->get(-2))").apply_to(&json!({
                "numbers": [
                    [1, 2, 3],
                    [5, 6],
                ],
            })),
            (Some(json!([2, 5])), vec![]),
        );
    }

    #[test]
    fn get_should_return_error_when_specified_array_index_does_not_exist() {
        assert_eq!(
            selection!("$->get(3)").apply_to(&json!([1, 2, 3])),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get index 3 out of bounds in array of length 3",
                    "path": ["->get"],
                    "range": [3, 6],
                }))]
            ),
        );
    }

    #[test]
    fn get_should_return_error_when_specified_array_negative_index_does_not_exist() {
        assert_eq!(
            selection!("$->get(-4)").apply_to(&json!([1, 2, 3])),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get index -4 out of bounds in array of length 3",
                    "path": ["->get"],
                    "range": [3, 6],
                }))]
            ),
        );
    }

    #[test]
    fn get_should_return_error_when_no_argument_provided() {
        assert_eq!(
            selection!("$->get").apply_to(&json!([1, 2, 3])),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get requires an argument",
                    "path": ["->get"],
                    "range": [3, 6],
                }))]
            ),
        );
    }

    #[test]
    fn get_should_return_error_when_string_index_applied_to_array() {
        assert_eq!(
            selection!("$->get('bogus')").apply_to(&json!([1, 2, 3])),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get on an array requires a integer index, got \"bogus\"",
                    "path": ["->get"],
                    "range": [3, 6],
                }))]
            ),
        );
    }

    #[test]
    fn get_should_return_char_from_string_at_specified_index() {
        assert_eq!(
            selection!("$->get(2)").apply_to(&json!("oyez")),
            (Some(json!("e")), vec![]),
        );
    }

    #[test]
    fn get_should_return_char_from_string_at_specified_negative_index() {
        assert_eq!(
            selection!("$->get(-1)").apply_to(&json!("oyez")),
            (Some(json!("z")), vec![]),
        );
    }

    #[test]
    fn get_should_return_error_when_specified_string_index_does_not_exist() {
        assert_eq!(
            selection!("$->get(4)").apply_to(&json!("oyez")),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get index 4 out of bounds in string of length 4",
                    "path": ["->get"],
                    "range": [3, 6],
                }))]
            ),
        );
    }

    #[test]
    fn get_should_return_error_when_calculated_string_index_does_not_exist() {
        let expected = (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get index -10 out of bounds in string of length 4",
                "path": ["->get"],
                "range": [3, 6],
            }))],
        );
        assert_eq!(
            selection!("$->get($->echo(-5)->mul(2))").apply_to(&json!("oyez")),
            expected,
        );
        assert_eq!(
            // The extra spaces here should not affect the error.range, as long
            // as we don't accidentally capture trailing spaces in the range.
            selection!("$->get($->echo(-5)->mul(2)  )").apply_to(&json!("oyez")),
            expected,
        );
    }

    #[test]
    fn get_should_correct_call_methods_with_extra_spaces() {
        // All these extra spaces certainly do affect the error.range, but it's
        // worth testing that we get all the ranges right, even with so much
        // space that could be accidentally captured.
        let selection_with_spaces = selection!(" $ -> get ( $ -> echo ( - 5 ) -> mul ( 2 ) ) ");
        assert_eq!(
            selection_with_spaces.apply_to(&json!("oyez")),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get index -10 out of bounds in string of length 4",
                    "path": ["->get"],
                    "range": [6, 9],
                }))]
            )
        );
        assert_debug_snapshot!(selection_with_spaces);
    }

    #[test]
    fn get_should_return_error_when_passing_bool_as_index() {
        assert_eq!(
            selection!("$->get(true)").apply_to(&json!("input")),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get on a string requires a integer index, got true",
                    "path": ["->get"],
                    "range": [3, 6],
                }))]
            ),
        );
    }

    #[test]
    fn get_should_return_item_from_object_at_specified_property() {
        assert_eq!(
            selection!("$->get('a')").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(1)), vec![]),
        );
    }

    #[test]
    fn get_should_throw_error_when_object_property_does_not_exist() {
        assert_eq!(
            selection!("$->get('d')").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get property d not found in object",
                    "path": ["->get"],
                    "range": [3, 6],
                }))]
            ),
        );
    }

    #[test]
    fn get_should_be_able_to_chain_method_off_of_selected_object_property() {
        assert_eq!(
            selection!("$->get('a')->add(10)").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(11)), vec![]),
        );
    }

    #[rstest::rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn get_should_return_none_when_argument_evaluates_to_none(#[case] spec: ConnectSpec) {
        assert_eq!(
            selection!("$.arr->get($.missing)", spec).apply_to(&json!({
                "arr": [1, 2, 3],
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .missing not found in object",
                    "path": ["missing"],
                    "range": [13, 20],
                    "spec": spec.to_string(),
                }))]
            ),
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use indexmap::IndexMap;
    use serde_json::Number;
    use shape::location::Location;

    use super::*;
    use crate::connectors::Key;
    use crate::connectors::PathSelection;
    use crate::connectors::json_selection::PathList;
    use crate::connectors::json_selection::lit_expr::LitExpr;

    fn get_location() -> Location {
        Location {
            source_id: SourceId::new("test".to_string()),
            span: 0..7,
        }
    }

    fn get_test_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        get_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("get".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::unknown([]),
        )
    }

    #[test]
    fn get_shape_should_error_on_no_args() {
        let location = get_location();
        assert_eq!(
            get_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("get".to_string(), Some(location.span)),
                None,
                Shape::string([]),
                Shape::none(),
            ),
            Shape::error(
                "Method ->get requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_on_too_many_args() {
        assert_eq!(
            get_test_shape(
                vec![
                    WithRange::new(LitExpr::Number(Number::from(0)), None),
                    WithRange::new(LitExpr::Number(Number::from(1)), None)
                ],
                Shape::string([])
            ),
            Shape::error(
                "Method ->get requires only one argument, but 2 were provided".to_string(),
                []
            )
        );
    }

    #[test]
    fn get_shape_should_return_char_for_string_with_valid_int_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(1)), None)],
                Shape::string_value("hello", [])
            ),
            Shape::string_value("e", [get_location()])
        );
    }

    #[test]
    fn get_shape_should_return_char_for_string_with_negative_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(-1)), None)],
                Shape::string_value("hello", [])
            ),
            Shape::string_value("o", [get_location()])
        );
    }

    #[test]
    fn get_shape_should_return_string_for_string_without_known_value() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(0)), None)],
                Shape::string([])
            ),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn get_shape_should_error_for_string_with_out_of_bounds_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(10)), None)],
                Shape::string_value("hello", [])
            ),
            Shape::error(
                "Method ->get index 10 out of bounds in string of length 5".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_string_with_negative_out_of_bounds_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(-10)), None)],
                Shape::string_value("hello", [])
            ),
            Shape::error(
                "Method ->get index -10 out of bounds in string of length 5".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_empty_string_out_of_bounds() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(0)), None)],
                Shape::string_value("", [])
            ),
            Shape::error(
                "Method ->get index 0 out of bounds in string of length 0".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_string_with_string_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::String("invalid".to_string()), None)],
                Shape::string([])
            ),
            Shape::error(
                "Method ->get must be provided an integer argument when applied to a string"
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_return_string_or_none_for_string_with_unknown_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(
                    LitExpr::Path(PathSelection {
                        path: PathList::Key(
                            Key::field("a").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    }),
                    None
                )],
                Shape::string([])
            ),
            Shape::string([get_location()])
        );
    }

    #[test]
    fn get_shape_should_return_element_for_array_with_valid_int_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(1)), None)],
                Shape::array(
                    [Shape::int([]), Shape::string([]), Shape::bool([])],
                    Shape::none(),
                    []
                )
            ),
            Shape::string([])
        );
    }

    #[test]
    fn get_shape_should_return_shape_for_list_with_valid_int_index() {
        let input_shape = Shape::list(Shape::string([]), []);
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(1)), None)],
                Shape::list(Shape::string([]), [])
            ),
            input_shape.any_item([])
        );
    }

    #[test]
    fn get_shape_should_return_element_for_array_with_negative_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(-1)), None)],
                Shape::array(
                    [Shape::int([]), Shape::string([]), Shape::bool([])],
                    Shape::none(),
                    []
                )
            ),
            Shape::bool([])
        );
    }

    #[test]
    fn get_shape_should_error_for_array_with_out_of_bounds_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(5)), None)],
                Shape::array([Shape::int([]), Shape::string([])], Shape::none(), [])
            ),
            Shape::error(
                "Method ->get index 5 out of bounds in array of length 2".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_array_with_negative_out_of_bounds_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(-5)), None)],
                Shape::array([Shape::int([]), Shape::string([])], Shape::none(), [])
            ),
            Shape::error(
                "Method ->get index -5 out of bounds in array of length 2".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_array_with_out_of_bounds_index_on_empty_array() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(0)), None)],
                Shape::array([], Shape::none(), [])
            ),
            Shape::error(
                "Method ->get index 0 out of bounds in array of length 0".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_array_with_string_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::String("invalid".to_string()), None)],
                Shape::array([Shape::int([])], Shape::none(), [])
            ),
            Shape::error(
                "Method ->get must be provided an integer argument when applied to an array"
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_return_unknown_or_none_for_array_with_unknown_index() {
        let input_shape = Shape::array([Shape::int([])], Shape::none(), []);
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(
                    LitExpr::Path(PathSelection {
                        path: PathList::Key(
                            Key::field("a").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    }),
                    None
                )],
                input_shape.clone()
            ),
            input_shape.any_item([])
        );
    }

    #[test]
    fn get_shape_should_return_property_shape_for_object_with_valid_string_key() {
        let mut fields = IndexMap::default();
        fields.insert("key".to_string(), Shape::int([]));
        fields.insert("other".to_string(), Shape::string([]));

        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::String("key".to_string()), None)],
                Shape::object(fields, Shape::none(), [])
            ),
            Shape::int([])
        );
    }

    #[test]
    fn get_shape_should_return_shape_for_dict_with_valid_string_key() {
        let input_shape = Shape::dict(Shape::int([]), []);
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::String("key".to_string()), None)],
                input_shape.clone()
            ),
            input_shape.any_field([])
        );
    }

    #[test]
    fn get_shape_should_error_for_object_with_missing_key() {
        let mut fields = IndexMap::default();
        fields.insert("existing".to_string(), Shape::int([]));

        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::String("missing".to_string()), None)],
                Shape::object(fields, Shape::none(), [])
            ),
            Shape::error(
                "Method ->get property missing not found in object".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_object_with_int_index() {
        let fields = IndexMap::default();

        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(42)), None)],
                Shape::object(fields, Shape::none(), [])
            ),
            Shape::error(
                "Method ->get must be provided an string argument when applied to an object"
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_object_with_bool_key() {
        let fields = IndexMap::default();

        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Bool(false), None)],
                Shape::object(fields, Shape::none(), [])
            ),
            Shape::error(
                "Method ->get must be provided an string argument when applied to an object"
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_object_with_null_key() {
        let fields = IndexMap::default();

        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Null, None)],
                Shape::object(fields, Shape::none(), [])
            ),
            Shape::error(
                "Method ->get must be provided an string argument when applied to an object"
                    .to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_return_unknown_for_object_with_unknown_key() {
        let fields = IndexMap::default();
        let input_shape = Shape::object(fields, Shape::none(), []);
        let test_shape = get_test_shape(
            vec![WithRange::new(
                LitExpr::Path(PathSelection {
                    path: PathList::Key(
                        Key::field("a").into_with_range(),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                }),
                None
            )],
            input_shape.clone()
        );

        assert_eq!(
            test_shape,
            input_shape.any_field(test_shape.locations.iter().cloned()),
        );
    }

    #[test]
    fn get_shape_should_return_unknown_for_unknown_input_with_valid_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(0)), None)],
                Shape::unknown([])
            ),
            Shape::unknown([get_location()])
        );
    }

    #[test]
    fn get_shape_should_return_string_or_unknown_for_unknown_input_with_string_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::String("key".to_string()), None)],
                Shape::unknown([])
            ),
            Shape::unknown([get_location()])
        );
    }

    #[test]
    fn get_shape_should_error_for_unknown_input_with_bool_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                Shape::unknown([])
            ),
            Shape::error(
                "Method ->get must be provided an integer or string argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_return_unknown_for_unknown_input_with_unknown_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(
                    LitExpr::Path(PathSelection {
                        path: PathList::Key(
                            Key::field("a").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    }),
                    None
                )],
                Shape::unknown([])
            ),
            Shape::unknown([get_location()])
        );
    }

    #[test]
    fn get_shape_should_error_for_bool_input_with_int_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(0)), None)],
                Shape::bool([])
            ),
            Shape::error(
                "Method ->get must be applied to a string, array, or object".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_null_input_with_int_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(0)), None)],
                Shape::null([])
            ),
            Shape::error(
                "Method ->get must be applied to a string, array, or object".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn get_shape_should_error_for_number_input_with_int_index() {
        assert_eq!(
            get_test_shape(
                vec![WithRange::new(LitExpr::Number(Number::from(0)), None)],
                Shape::int([])
            ),
            Shape::error(
                "Method ->get must be applied to a string, array, or object".to_string(),
                [get_location()]
            )
        );
    }
}
