use std::iter::empty;

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

impl_arrow_method!(ValuesMethod, values_method, values_shape);
/// Given an object, returns an array of its values (aka property values).
/// Simple example:
///
/// $->echo({"a": 1, "b": 2, "c": 3})       returns [1, 2, 3]
fn values_method(
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
        JSON::Object(map) => {
            let values = map.values().cloned().collect();
            (Some(JSON::Array(values)), Vec::new())
        }
        _ => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an object input, not {}",
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
fn values_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Object { fields, rest, .. } => {
            Shape::array(fields.values().cloned(), rest.clone(), empty())
        }
        _ => Shape::error(
            "Method ->values requires an object input",
            method_name.shape_location(context.source_id()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    #[test]
    fn values_should_return_an_array_of_property_values_from_an_object() {
        assert_eq!(
            selection!("$->values").apply_to(&json!({
                "a": 1,
                "b": "two",
                "c": false,
            })),
            (Some(json!([1, "two", false])), vec![]),
        );
    }

    #[test]
    fn values_should_return_an_empty_array_given_empty_object() {
        assert_eq!(
            selection!("$->values").apply_to(&json!({})),
            (Some(json!([])), vec![]),
        );
    }

    #[test]
    fn values_should_error_given_a_non_object() {
        assert_eq!(
            selection!("notAnObject->values").apply_to(&json!({
                "notAnObject": null,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->values requires an object input, not null",
                    "path": ["notAnObject", "->values"],
                    "range": [13, 19],
                }))]
            ),
        );
    }
}
