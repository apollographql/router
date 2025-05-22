use std::iter::empty;

use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::helpers::json_type_name;
use crate::sources::connect::json_selection::helpers::vec_push;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;
use crate::sources::connect::json_selection::location::merge_ranges;

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
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(index_literal) = method_args.and_then(|MethodArgs { args, .. }| args.first()) else {
        return (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires an argument", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
            )],
        );
    };

    match index_literal.apply_to_path(data, vars, input_path) {
        (Some(JSON::Number(n)), index_errors) => match (data, n.as_i64()) {
            (JSON::Array(array), Some(i)) => {
                // Negative indices count from the end of the array
                if let Some(element) = array.get(if i < 0 {
                    (array.len() as i64 + i) as usize
                } else {
                    i as usize
                }) {
                    (Some(element.clone()), index_errors)
                } else {
                    (
                        None,
                        vec_push(
                            index_errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{}({}) index out of bounds",
                                    method_name.as_ref(),
                                    i,
                                ),
                                input_path.to_vec(),
                                index_literal.range(),
                            ),
                        ),
                    )
                }
            }

            (JSON::String(s), Some(i)) => {
                let s_str = s.as_str();
                let ilen = s_str.len() as i64;
                // Negative indices count from the end of the array
                let index = if i < 0 { ilen + i } else { i };
                if index >= 0 && index < ilen {
                    let uindex = index as usize;
                    let single_char_string = s_str[uindex..uindex + 1].to_string();
                    (Some(JSON::String(single_char_string.into())), index_errors)
                } else {
                    (
                        None,
                        vec_push(
                            index_errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{}({}) index out of bounds",
                                    method_name.as_ref(),
                                    i,
                                ),
                                input_path.to_vec(),
                                index_literal.range(),
                            ),
                        ),
                    )
                }
            }

            (_, None) => (
                None,
                vec_push(
                    index_errors,
                    ApplyToError::new(
                        format!(
                            "Method ->{} requires an integer index",
                            method_name.as_ref()
                        ),
                        input_path.to_vec(),
                        index_literal.range(),
                    ),
                ),
            ),
            _ => (
                None,
                vec_push(
                    index_errors,
                    ApplyToError::new(
                        format!(
                            "Method ->{} requires an array or string input, not {}",
                            method_name.as_ref(),
                            json_type_name(data),
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                    ),
                ),
            ),
        },
        (Some(ref key @ JSON::String(ref s)), index_errors) => match data {
            JSON::Object(map) => {
                if let Some(value) = map.get(s.as_str()) {
                    (Some(value.clone()), index_errors)
                } else {
                    (
                        None,
                        vec_push(
                            index_errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{}({}) object key not found",
                                    method_name.as_ref(),
                                    key
                                ),
                                input_path.to_vec(),
                                index_literal.range(),
                            ),
                        ),
                    )
                }
            }
            _ => (
                None,
                vec_push(
                    index_errors,
                    ApplyToError::new(
                        format!(
                            "Method ->{}({}) requires an object input",
                            method_name.as_ref(),
                            key
                        ),
                        input_path.to_vec(),
                        merge_ranges(
                            method_name.range(),
                            method_args.and_then(|args| args.range()),
                        ),
                    ),
                ),
            ),
        },
        (Some(value), index_errors) => (
            None,
            vec_push(
                index_errors,
                ApplyToError::new(
                    format!(
                        "Method ->{}({}) requires an integer or string argument",
                        method_name.as_ref(),
                        value,
                    ),
                    input_path.to_vec(),
                    index_literal.range(),
                ),
            ),
        ),
        (None, index_errors) => (
            None,
            vec_push(
                index_errors,
                ApplyToError::new(
                    format!(
                        "Method ->{} received undefined argument",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    index_literal.range(),
                ),
            ),
        ),
    }
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn get_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if let Some(MethodArgs { args, .. }) = method_args {
        if let Some(index_literal) = args.first() {
            let index_shape = index_literal.compute_output_shape(
                input_shape.clone(),
                dollar_shape,
                named_var_shapes,
                source_id,
            );
            return match index_shape.case() {
                ShapeCase::String(value_opt) => match input_shape.case() {
                    ShapeCase::Object { fields, rest } => {
                        if let Some(literal_name) = value_opt {
                            if let Some(shape) = fields.get(literal_name.as_str()) {
                                return shape.clone();
                            }
                        }
                        let mut value_shapes = fields.values().cloned().collect::<Vec<_>>();
                        if !rest.is_none() {
                            value_shapes.push(rest.clone());
                        }
                        value_shapes.push(Shape::none());
                        Shape::one(value_shapes, Vec::new())
                    }
                    ShapeCase::Array { .. } => Shape::error(
                        format!(
                            "Method ->{} applied to array requires integer index, not string",
                            method_name.as_ref()
                        )
                        .as_str(),
                        index_literal.shape_location(source_id),
                    ),
                    ShapeCase::String(_) => Shape::error(
                        format!(
                            "Method ->{} applied to string requires integer index, not string",
                            method_name.as_ref()
                        )
                        .as_str(),
                        index_literal.shape_location(source_id),
                    ),
                    _ => Shape::error(
                        "Method ->get requires an object, array, or string input",
                        method_name.shape_location(source_id),
                    ),
                },

                ShapeCase::Int(value_opt) => {
                    match input_shape.case() {
                        ShapeCase::Array { prefix, tail } => {
                            if let Some(index) = value_opt {
                                if let Some(item) = prefix.get(*index as usize) {
                                    return item.clone();
                                }
                            }
                            // If tail.is_none(), this will simplify to Shape::none().
                            Shape::one([tail.clone(), Shape::none()], empty())
                        }

                        ShapeCase::String(Some(s)) => {
                            let Some(index) = value_opt else {
                                return Shape::one(
                                    [Shape::string(empty()), Shape::none()],
                                    empty(),
                                );
                            };
                            let index = *index as usize;
                            if index < s.len() {
                                Shape::string_value(&s[index..index + 1], empty())
                            } else {
                                Shape::none()
                            }
                        }
                        ShapeCase::String(None) => {
                            Shape::one([Shape::string(empty()), Shape::none()], empty())
                        }

                        ShapeCase::Object { .. } => Shape::error(
                            format!(
                                "Method ->{} applied to object requires string index, not integer",
                                method_name.as_ref()
                            )
                            .as_str(),
                            index_literal.shape_location(source_id),
                        ),

                        _ => Shape::error(
                            "Method ->get requires an object, array, or string input",
                            method_name.shape_location(source_id),
                        ),
                    }
                }

                _ => Shape::error(
                    format!(
                        "Method ->{} requires an integer or string argument",
                        method_name.as_ref()
                    )
                    .as_str(),
                    index_literal.shape_location(source_id),
                ),
            };
        }
    }

    Shape::error(
        format!("Method ->{} requires an argument", method_name.as_ref()).as_str(),
        method_name.shape_location(source_id),
    )
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::assert_debug_snapshot;
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
                    "message": "Method ->get(3) index out of bounds",
                    "path": ["->get"],
                    "range": [7, 8],
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
                    "message": "Method ->get(-4) index out of bounds",
                    "path": ["->get"],
                    "range": [7, 9],
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
                    "message": "Method ->get(\"bogus\") requires an object input",
                    "path": ["->get"],
                    "range": [3, 15],
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
                    "message": "Method ->get(4) index out of bounds",
                    "path": ["->get"],
                    "range": [7, 8],
                }))]
            ),
        );
    }

    #[test]
    fn get_should_return_error_when_calculated_string_index_does_not_exist() {
        let expected = (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get(-10) index out of bounds",
                "path": ["->get"],
                "range": [7, 26],
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
                    "message": "Method ->get(-10) index out of bounds",
                    "path": ["->get"],
                    "range": [12, 42],
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
                    "message": "Method ->get(true) requires an integer or string argument",
                    "path": ["->get"],
                    "range": [7, 11],
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
                    "message": "Method ->get(\"d\") object key not found",
                    "path": ["->get"],
                    "range": [7, 10],
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
}
