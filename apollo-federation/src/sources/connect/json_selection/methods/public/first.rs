use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::PathList;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

impl_arrow_method!(FirstMethod, first_method, first_shape);
/// The "first" method is a utility function that can be run against an array to grab the 0th item from it.
/// The simplest possible example:
///
/// $->echo([1,2,3])->first     results in 1
fn first_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
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
            )],
        );
    }

    match data {
        JSON::Array(array) => {
            if let Some(first) = array.first() {
                tail.apply_to_path(first, vars, input_path)
            } else {
                (None, vec![])
            }
        }

        JSON::String(s) => {
            if let Some(first) = s.as_str().chars().next() {
                tail.apply_to_path(&JSON::String(first.to_string().into()), vars, input_path)
            } else {
                (None, vec![])
            }
        }

        _ => tail.apply_to_path(data, vars, input_path),
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn first_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    let location = method_name.shape_location(source_id);
    if method_args.is_some() {
        return Shape::error(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            location,
        );
    }

    // Location is not solely based on the method, but also the type the method is being applied to
    let locations = input_shape.locations.iter().cloned().chain(location);

    match input_shape.case() {
        ShapeCase::String(Some(value)) => Shape::string_value(&value[0..1], locations),
        ShapeCase::String(None) => Shape::string(locations),
        ShapeCase::Array { prefix, tail } => {
            if let Some(first) = prefix.first() {
                first.clone()
            } else if tail.is_none() {
                Shape::none()
            } else {
                Shape::one([tail.clone(), Shape::none()], locations)
            }
        }
        ShapeCase::Name(_, _) => input_shape.item(0, locations),
        // When there is no obvious first element, ->first gives us the input
        // value itself, which has input_shape.
        _ => input_shape.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn first_should_get_first_element_from_array() {
        assert_eq!(
            selection!("$->first").apply_to(&json!([1, 2, 3])),
            (Some(json!(1)), vec![]),
        );
    }

    #[test]
    fn first_should_get_none_when_no_items_exist() {
        assert_eq!(selection!("$->first").apply_to(&json!([])), (None, vec![]),);
    }
}
