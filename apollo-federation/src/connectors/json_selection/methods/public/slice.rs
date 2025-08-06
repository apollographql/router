use std::iter::empty;

use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(SliceMethod, slice_method, slice_shape);
/// Extracts part of an array given a set of indices and returns a new array.
/// Can also be used on a string to get chars at the specified indices.
/// The simplest possible example:
///
/// $->echo([0,1,2,3,4,5])->slice(1, 3)     would result in [1,2]
/// $->echo("hello")->slice(1,3)            would result in "el"
fn slice_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let length = if let JSON::Array(array) = data {
        array.len() as i64
    } else if let JSON::String(s) = data {
        s.as_str().len() as i64
    } else {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an array or string input",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    if let Some(MethodArgs { args, .. }) = method_args {
        let mut errors = Vec::new();

        let start = args
            .first()
            .and_then(|arg| {
                let (value_opt, apply_errors) = arg.apply_to_path(data, vars, input_path, spec);
                errors.extend(apply_errors);
                value_opt
            })
            .and_then(|n| n.as_i64())
            .unwrap_or(0)
            .max(0)
            .min(length) as usize;

        let end = args
            .get(1)
            .and_then(|arg| {
                let (value_opt, apply_errors) = arg.apply_to_path(data, vars, input_path, spec);
                errors.extend(apply_errors);
                value_opt
            })
            .and_then(|n| n.as_i64())
            .unwrap_or(length)
            .max(0)
            .min(length) as usize;

        let array = match data {
            JSON::Array(array) => {
                if end - start > 0 {
                    JSON::Array(
                        array
                            .iter()
                            .skip(start)
                            .take(end - start)
                            .cloned()
                            .collect(),
                    )
                } else {
                    JSON::Array(Vec::new())
                }
            }

            JSON::String(s) => {
                if end - start > 0 {
                    JSON::String(s.as_str()[start..end].to_string().into())
                } else {
                    JSON::String("".to_string().into())
                }
            }

            _ => unreachable!(),
        };

        (Some(array), errors)
    } else {
        // TODO Should calling ->slice or ->slice() without arguments be an
        // error? In JavaScript, array->slice() copies the array, but that's not
        // so useful in an immutable value-typed language like JSONSelection.
        (Some(data.clone()), Vec::new())
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn slice_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    mut input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    // There are more clever shapes we could compute here (when start and end
    // are statically known integers and input_shape is an array or string with
    // statically known prefix elements, for example) but for now we play it
    // safe (and honest) by returning a new variable-length array whose element
    // shape is a union of the original element (prefix and tail) shapes.
    match input_shape.case() {
        ShapeCase::Array { prefix, tail } => {
            let mut one_shapes = prefix.clone();
            if !tail.is_none() {
                one_shapes.push(tail.clone());
            }
            Shape::array([], Shape::one(one_shapes, empty()), input_shape.locations)
        }
        ShapeCase::String(_) => Shape::string(input_shape.locations),
        ShapeCase::Name(_, _) => input_shape, // TODO: add a way to validate inputs after name resolution
        _ => Shape::error(
            format!(
                "Method ->{} requires an array or string input",
                method_name.as_ref()
            ),
            {
                input_shape
                    .locations
                    .extend(method_name.shape_location(context.source_id()));
                input_shape.locations
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn slice_should_grab_parts_of_array_by_specified_indices() {
        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!([1, 2, 3, 4, 5])),
            (Some(json!([2, 3])), vec![]),
        );
    }

    #[test]
    fn slice_should_stop_at_end_when_array_is_shorter_than_specified_end_index() {
        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!([1, 2])),
            (Some(json!([2])), vec![]),
        );
    }

    #[test]
    fn slice_should_return_empty_array_when_array_is_shorter_than_specified_indices() {
        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!([1])),
            (Some(json!([])), vec![]),
        );
    }

    #[test]
    fn slice_should_return_empty_array_when_provided_empty_array() {
        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!([])),
            (Some(json!([])), vec![]),
        );
    }

    #[test]
    fn slice_should_return_blank_when_string_is_empty() {
        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!("")),
            (Some(json!("")), vec![]),
        );
    }

    #[test]
    fn slice_should_return_part_of_string() {
        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!("hello")),
            (Some(json!("el")), vec![]),
        );
    }

    #[test]
    fn slice_should_return_part_of_string_when_slice_indices_are_larger_than_string() {
        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!("he")),
            (Some(json!("e")), vec![]),
        );
    }

    #[test]
    fn slice_should_return_empty_string_when_indices_are_completely_out_of_string_bounds() {
        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!("h")),
            (Some(json!("")), vec![]),
        );
    }
}
