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

impl_arrow_method!(LastMethod, last_method, last_shape);
/// The "last" method is a utility function that can be run against an array to grab the final item from it
/// or a string to get the last character.
/// The simplest possible example:
///
/// $->echo([1,2,3])->last     results in 3
/// $->echo("hello")->last     results in "o"
fn last_method(
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
        JSON::Array(array) => (array.last().cloned(), Vec::new()),
        JSON::String(s) => s.as_str().chars().last().map_or_else(
            || (None, Vec::new()),
            |last| (Some(JSON::String(last.to_string().into())), Vec::new()),
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
fn last_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    if method_args.is_some() {
        return Shape::error(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    match input_shape.case() {
        ShapeCase::String(Some(value)) => {
            value.chars().last().map_or_else(Shape::none, |last_char| {
                Shape::string_value(
                    last_char.to_string().as_str(),
                    method_name.shape_location(context.source_id()),
                )
            })
        }

        ShapeCase::String(None) => Shape::one(
            [
                Shape::string(method_name.shape_location(context.source_id())),
                Shape::none(),
            ],
            method_name.shape_location(context.source_id()),
        ),

        ShapeCase::Array { prefix, tail } => {
            if tail.is_none() {
                prefix.last().cloned().unwrap_or_else(Shape::none)
            } else if let Some(last) = prefix.last() {
                Shape::one(
                    [last.clone(), tail.clone(), Shape::none()],
                    method_name.shape_location(context.source_id()),
                )
            } else {
                Shape::one(
                    [tail.clone(), Shape::none()],
                    method_name.shape_location(context.source_id()),
                )
            }
        }

        ShapeCase::Name(_, _) => {
            input_shape.any_item(method_name.shape_location(context.source_id()))
        }
        ShapeCase::Unknown => Shape::unknown(method_name.shape_location(context.source_id())),

        _ => Shape::error_with_partial(
            format!(
                "Method ->{} requires an array or string input",
                method_name.as_ref()
            ),
            input_shape.clone(),
            input_shape.locations().cloned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn last_should_get_last_element_from_array() {
        assert_eq!(
            selection!("$->last").apply_to(&json!([1, 2, 3])),
            (Some(json!(3)), Vec::new()),
        );
    }

    #[test]
    fn last_should_get_none_when_no_items_exist() {
        assert_eq!(selection!("$->last").apply_to(&json!([])), (None, vec![]),);
    }

    #[test]
    fn last_should_get_last_char_from_string() {
        assert_eq!(
            selection!("$->last").apply_to(&json!("hello")),
            (Some(json!("o")), vec![]),
        );
    }
}
