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

impl_arrow_method!(SizeMethod, size_method, size_shape);
/// Returns the number of items in an array, length of a string, or the number of properties in an array.
/// The simplest possible example:
///
/// $->echo([1,2,3,4,5])->size                      would result in 5
/// $->echo("hello")->size                          would result in 5
/// $->echo({"a": true, "b": true})->size           would result in 2
fn size_method(
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
        JSON::Array(array) => {
            let size = array.len() as i64;
            (Some(JSON::Number(size.into())), Vec::new())
        }
        JSON::String(s) => {
            let size = s.as_str().len() as i64;
            (Some(JSON::Number(size.into())), Vec::new())
        }
        // Though we can't ask for ->first or ->last or ->at(n) on an object, we
        // can safely return how many properties the object has for ->size.
        JSON::Object(map) => {
            let size = map.len() as i64;
            (Some(JSON::Number(size.into())), Vec::new())
        }
        _ => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an array, string, or object input, not {}",
                    method_name.as_ref(),
                    json_type_name(data),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        ),
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn size_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    mut input_shape: Shape,
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
        ShapeCase::String(Some(value)) => Shape::int_value(
            value.len() as i64,
            method_name.shape_location(context.source_id()),
        ),
        ShapeCase::String(None) => Shape::int(method_name.shape_location(context.source_id())),
        ShapeCase::Name(_, _) => Shape::int(method_name.shape_location(context.source_id())), // TODO: catch errors after name resolution
        ShapeCase::Array { prefix, tail } => {
            if tail.is_none() {
                Shape::int_value(
                    prefix.len() as i64,
                    method_name.shape_location(context.source_id()),
                )
            } else {
                Shape::int(method_name.shape_location(context.source_id()))
            }
        }
        ShapeCase::Object { fields, rest, .. } => {
            if rest.is_none() {
                Shape::int_value(
                    fields.len() as i64,
                    method_name.shape_location(context.source_id()),
                )
            } else {
                Shape::int(method_name.shape_location(context.source_id()))
            }
        }
        _ => Shape::error(
            format!(
                "Method ->{} requires an array, string, or object input",
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

    use crate::connectors::ApplyToError;
    use crate::selection;

    #[test]
    fn size_should_return_0_when_empty_array() {
        assert_eq!(
            selection!("$->size").apply_to(&json!([])),
            (Some(json!(0)), vec![]),
        );
    }

    #[test]
    fn size_should_return_number_of_items_in_array() {
        assert_eq!(
            selection!("$->size").apply_to(&json!([1, 2, 3])),
            (Some(json!(3)), vec![]),
        );
    }

    #[test]
    fn size_should_error_when_applied_to_null() {
        assert_eq!(
            selection!("$->size").apply_to(&json!(null)),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->size requires an array, string, or object input, not null",
                    "path": ["->size"],
                    "range": [3, 7],
                }))]
            ),
        );
    }

    #[test]
    fn size_should_error_when_applied_to_bool() {
        assert_eq!(
            selection!("$->size").apply_to(&json!(true)),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->size requires an array, string, or object input, not boolean",
                    "path": ["->size"],
                    "range": [3, 7],
                }))]
            ),
        );
    }

    #[test]
    fn size_should_error_when_applied_to_number() {
        assert_eq!(
            selection!("count->size").apply_to(&json!({
                "count": 123,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->size requires an array, string, or object input, not number",
                    "path": ["count", "->size"],
                    "range": [7, 11],
                }))]
            ),
        );
    }

    #[test]
    fn size_should_return_length_of_string() {
        assert_eq!(
            selection!("$->size").apply_to(&json!("hello")),
            (Some(json!(5)), vec![]),
        );
    }

    #[test]
    fn size_should_return_0_on_empty_string() {
        assert_eq!(
            selection!("$->size").apply_to(&json!("")),
            (Some(json!(0)), vec![]),
        );
    }

    #[test]
    fn size_should_return_number_of_properties_of_an_object() {
        assert_eq!(
            selection!("$->size").apply_to(&json!({ "a": 1, "b": 2, "c": 3 })),
            (Some(json!(3)), vec![]),
        );
    }

    #[test]
    fn size_should_return_0_on_empty_object() {
        assert_eq!(
            selection!("$->size").apply_to(&json!({})),
            (Some(json!(0)), vec![]),
        );
    }
}
