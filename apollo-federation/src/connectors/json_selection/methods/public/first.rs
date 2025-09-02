use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(FirstMethod, first_method, first_shape);
/// The "first" method is a utility function that can be run against an array to grab the 0th item from it
/// or a string to get the first character.
/// The simplest possible example:
///
/// $->echo([1,2,3])->first     results in 1
/// $->echo("hello")->first     results in "h"
fn first_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
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

    match data {
        JSON::Array(array) => (array.first().cloned(), Vec::new()),
        JSON::String(s) => s.as_str().chars().next().map_or_else(
            || (None, Vec::new()),
            |first| (Some(JSON::String(first.to_string().into())), Vec::new()),
        ),
        _ => (
            Some(data.clone()),
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an array or string input",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        ),
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn first_shape(
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

    // Location is not solely based on the method, but also the type the method is being applied to
    let locations = input_shape.locations().cloned().chain(location);

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
        ShapeCase::Unknown => Shape::unknown(locations),
        // When there is no obvious first element, ->first gives us the input
        // value itself, which has input_shape.
        _ => Shape::error_with_partial(
            format!(
                "Method ->{} requires an array or string input",
                method_name.as_ref()
            ),
            input_shape.clone(),
            locations,
        ),
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

    #[test]
    fn first_should_get_first_char_from_string() {
        assert_eq!(
            selection!("$->first").apply_to(&json!("hello")),
            (Some(json!("h")), vec![]),
        );
    }
}
