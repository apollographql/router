use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_type_name;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(EntriesMethod, entries_method, entries_shape);
/// Returns the keys and values given an object.
///
/// The simplest possible example:
///
/// $->echo({"a": 1, "b": "two", "c": false, })->entries     
/// would result in [{ "key": "a", "value": 1 }, { "key": "b", "value": "two" }, { "key": "c", "value": false },]
///
/// You can also use .key to grab just the keys:
/// $->echo({"a": 1, "b": "two", "c": false, })->entries.key     
/// would result in ["a", "b", "c"]
///
/// or you can also use .value to grab just the values:
/// $->echo({"a": 1, "b": "two", "c": false, })->entries.key     
/// would result in [1, "two", false]
fn entries_method(
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
            let entries = map
                .iter()
                .map(|(key, value)| {
                    let mut key_value_pair = JSONMap::new();
                    key_value_pair.insert(ByteString::from("key"), JSON::String(key.clone()));
                    key_value_pair.insert(ByteString::from("value"), value.clone());
                    JSON::Object(key_value_pair)
                })
                .collect();
            (Some(JSON::Array(entries)), Vec::new())
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
fn entries_shape(
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
        ShapeCase::Object { fields, rest, .. } => {
            let entry_shapes = fields
                .iter()
                .map(|(key, value)| {
                    let mut key_value_pair = Shape::empty_map();
                    key_value_pair.insert(
                        "key".to_string(),
                        Shape::string_value(key.as_str(), Vec::new()),
                    );
                    key_value_pair.insert("value".to_string(), value.clone());
                    Shape::object(
                        key_value_pair,
                        Shape::none(),
                        method_name.shape_location(context.source_id()),
                    )
                })
                .collect::<Vec<_>>();

            if rest.is_none() {
                Shape::array(
                    entry_shapes,
                    rest.clone(),
                    method_name.shape_location(context.source_id()),
                )
            } else {
                let mut tail_key_value_pair = Shape::empty_map();
                tail_key_value_pair.insert("key".to_string(), Shape::string(Vec::new()));
                tail_key_value_pair.insert("value".to_string(), rest.clone());
                Shape::array(
                    entry_shapes,
                    Shape::object(
                        tail_key_value_pair,
                        Shape::none(),
                        method_name.shape_location(context.source_id()),
                    ),
                    method_name.shape_location(context.source_id()),
                )
            }
        }
        ShapeCase::Name(_, _) => {
            let mut entries = Shape::empty_map();
            entries.insert("key".to_string(), Shape::string(Vec::new()));
            entries.insert("value".to_string(), input_shape.any_field(Vec::new()));
            Shape::list(
                Shape::object(
                    entries,
                    Shape::none(),
                    method_name.shape_location(context.source_id()),
                ),
                method_name.shape_location(context.source_id()),
            )
        }
        _ => Shape::error(
            format!("Method ->{} requires an object input", method_name.as_ref()),
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
    fn entries_should_return_keys_and_values_when_applied_to_object() {
        assert_eq!(
            selection!("$->entries").apply_to(&json!({
                "a": 1,
                "b": "two",
                "c": false,
            })),
            (
                Some(json!([
                    { "key": "a", "value": 1 },
                    { "key": "b", "value": "two" },
                    { "key": "c", "value": false },
                ])),
                vec![],
            ),
        );
    }

    #[test]
    fn entries_should_return_only_keys_when_key_is_requested() {
        assert_eq!(
            // This is just like $->keys, given the automatic array mapping of
            // .key, though you probably want to use ->keys directly because it
            // avoids cloning all the values unnecessarily.
            selection!("$->entries.key").apply_to(&json!({
                "one": 1,
                "two": 2,
                "three": 3,
            })),
            (Some(json!(["one", "two", "three"])), vec![]),
        );
    }

    #[test]
    fn entries_should_return_only_values_when_values_is_requested() {
        assert_eq!(
            // This is just like $->values, given the automatic array mapping of
            // .value, though you probably want to use ->values directly because
            // it avoids cloning all the keys unnecessarily.
            selection!("$->entries.value").apply_to(&json!({
                "one": 1,
                "two": 2,
                "three": 3,
            })),
            (Some(json!([1, 2, 3])), vec![]),
        );
    }

    #[test]
    fn entries_should_return_empty_array_when_applied_to_empty_object() {
        assert_eq!(
            selection!("$->entries").apply_to(&json!({})),
            (Some(json!([])), vec![]),
        );
    }

    #[test]
    fn entries_should_error_when_applied_to_non_object() {
        assert_eq!(
            selection!("notAnObject->entries").apply_to(&json!({
                "notAnObject": true,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->entries requires an object input, not boolean",
                    "path": ["notAnObject", "->entries"],
                    "range": [13, 20],
                }))]
            ),
        );
    }
}
