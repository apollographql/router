use heck::ToLowerCamelCase;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_type_name;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(
    PropertiesToCamelCaseMethod,
    properties_to_camel_case_method,
    properties_to_camel_case_shape
);

/// Converts all property names of an object to camelCase.
///
/// Handles PascalCase, snake_case, and SCREAMING_SNAKE_CASE inputs.
/// By default, the transformation is recursive (applies to nested objects
/// and objects within arrays). Pass `false` to apply only to the
/// top-level properties.
///
/// Examples:
///
/// $->propertiesToCamelCase
/// given {"property_one": 1, "PropertyTwo": 2, "PROPERTY_THREE": 3}
/// results in {"propertyOne": 1, "propertyTwo": 2, "propertyThree": 3}
///
/// $->propertiesToCamelCase(false)
/// given {"outer_key": {"inner_key": 1}}
/// results in {"outerKey": {"inner_key": 1}}
fn properties_to_camel_case_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();

    if arg_count > 1 {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} takes at most one argument, but {} were provided",
                    method_name.as_ref(),
                    arg_count,
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    }

    let recursive = if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        let (arg_value, arg_errors) = first_arg.apply_to_path(data, vars, input_path, spec);
        if !arg_errors.is_empty() {
            return (None, arg_errors);
        }
        match arg_value {
            Some(JSON::Bool(b)) => b,
            _ => {
                return (
                    None,
                    vec![ApplyToError::new(
                        format!(
                            "Method ->{} requires a boolean argument",
                            method_name.as_ref(),
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    )],
                );
            }
        }
    } else {
        true
    };

    match data {
        JSON::Object(_) => {
            let mut errors = Vec::new();
            let result =
                transform_keys(data, recursive, method_name, input_path, spec, &mut errors);
            (Some(result), errors)
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

fn transform_keys(
    data: &JSON,
    recursive: bool,
    method_name: &WithRange<String>,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
    errors: &mut Vec<ApplyToError>,
) -> JSON {
    match data {
        JSON::Object(map) => {
            let mut new_map = JSONMap::new();
            for (key, value) in map.iter() {
                let camel_key = key.as_str().to_lower_camel_case();
                let new_value = if recursive {
                    transform_keys(value, recursive, method_name, input_path, spec, errors)
                } else {
                    value.clone()
                };
                if new_map.contains_key(camel_key.as_str()) {
                    errors.push(ApplyToError::new(
                        format!(
                            "Method ->{}: key collision after camelCase conversion: \"{}\" and \"{}\" both map to \"{}\"",
                            method_name.as_ref(),
                            new_map.iter().find(|(k, _)| k.as_str() == camel_key).map(|(k, _)| k.as_str()).unwrap_or(""),
                            key.as_str(),
                            camel_key,
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    ));
                }
                new_map.insert(ByteString::from(camel_key), new_value);
            }
            JSON::Object(new_map)
        }
        JSON::Array(arr) if recursive => {
            let new_arr = arr
                .iter()
                .map(|item| transform_keys(item, recursive, method_name, input_path, spec, errors))
                .collect();
            JSON::Array(new_arr)
        }
        _ => data.clone(),
    }
}

fn transform_shape(
    input_shape: Shape,
    recursive: bool,
    locations: impl IntoIterator<Item = shape::location::Location> + Clone,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Object { fields, rest, .. } => {
            let mut new_fields = Shape::empty_map();
            for (key, value) in fields.iter() {
                let camel_key = key.as_str().to_lower_camel_case();
                let new_value = if recursive {
                    transform_shape(value.clone(), recursive, locations.clone())
                } else {
                    value.clone()
                };
                new_fields.insert(camel_key, new_value);
            }
            let new_rest = if recursive {
                match rest.case() {
                    ShapeCase::None => rest.clone(),
                    _ => transform_shape(rest.clone(), recursive, locations.clone()),
                }
            } else {
                rest.clone()
            };
            Shape::object(new_fields, new_rest, locations)
        }
        ShapeCase::Array { prefix, tail, .. } if recursive => {
            let new_prefix: Vec<_> = prefix
                .iter()
                .map(|item| transform_shape(item.clone(), recursive, locations.clone()))
                .collect();
            let new_tail = transform_shape(tail.clone(), recursive, locations.clone());
            Shape::array(new_prefix, new_tail, locations)
        }
        _ => input_shape,
    }
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn properties_to_camel_case_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();

    if arg_count > 1 {
        return Shape::error(
            format!(
                "Method ->{} takes at most one argument, but {} were provided",
                method_name.as_ref(),
                arg_count,
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    // Determine recursive flag from literal argument if possible
    let recursive = if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        use crate::connectors::json_selection::lit_expr::LitExpr;
        match first_arg.as_ref() {
            LitExpr::Bool(b) => *b,
            _ => true, // Default to true for non-literal args
        }
    } else {
        true
    };

    let locations = method_name.shape_location(context.source_id());

    match input_shape.case() {
        ShapeCase::Object { .. } => transform_shape(input_shape, recursive, locations),
        ShapeCase::Name(_, _) | ShapeCase::Unknown => input_shape,
        _ => Shape::error(
            format!("Method ->{} requires an object input", method_name.as_ref()),
            input_shape
                .locations()
                .cloned()
                .chain(method_name.shape_location(context.source_id())),
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::connectors::ApplyToError;
    use crate::selection;

    #[test]
    fn should_convert_snake_case_properties() {
        assert_eq!(
            selection!("$->propertiesToCamelCase").apply_to(&json!({
                "property_one": 1,
                "property_two": 2,
            })),
            (
                Some(json!({
                    "propertyOne": 1,
                    "propertyTwo": 2,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_convert_pascal_case_properties() {
        assert_eq!(
            selection!("$->propertiesToCamelCase").apply_to(&json!({
                "PropertyOne": 1,
                "PropertyTwo": 2,
            })),
            (
                Some(json!({
                    "propertyOne": 1,
                    "propertyTwo": 2,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_convert_screaming_snake_case_properties() {
        assert_eq!(
            selection!("$->propertiesToCamelCase").apply_to(&json!({
                "PROPERTY_ONE": 1,
                "PROPERTY_TWO": 2,
            })),
            (
                Some(json!({
                    "propertyOne": 1,
                    "propertyTwo": 2,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_leave_camel_case_unchanged() {
        assert_eq!(
            selection!("$->propertiesToCamelCase").apply_to(&json!({
                "alreadyCamel": 1,
                "anotherOne": 2,
            })),
            (
                Some(json!({
                    "alreadyCamel": 1,
                    "anotherOne": 2,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_recursively_transform_nested_objects() {
        assert_eq!(
            selection!("$->propertiesToCamelCase").apply_to(&json!({
                "outer_key": {
                    "inner_key": 1,
                },
            })),
            (
                Some(json!({
                    "outerKey": {
                        "innerKey": 1,
                    },
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_recursively_transform_objects_in_arrays() {
        assert_eq!(
            selection!("$->propertiesToCamelCase").apply_to(&json!({
                "array_key": [
                    { "nested_key": 1 },
                    { "another_key": 2 },
                ],
            })),
            (
                Some(json!({
                    "arrayKey": [
                        { "nestedKey": 1 },
                        { "anotherKey": 2 },
                    ],
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_apply_shallowly_when_false_is_passed() {
        assert_eq!(
            selection!("$->propertiesToCamelCase(false)").apply_to(&json!({
                "outer_key": {
                    "inner_key": 1,
                },
            })),
            (
                Some(json!({
                    "outerKey": {
                        "inner_key": 1,
                    },
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_apply_recursively_when_true_is_passed() {
        assert_eq!(
            selection!("$->propertiesToCamelCase(true)").apply_to(&json!({
                "outer_key": {
                    "inner_key": 1,
                },
            })),
            (
                Some(json!({
                    "outerKey": {
                        "innerKey": 1,
                    },
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_handle_empty_object() {
        assert_eq!(
            selection!("$->propertiesToCamelCase").apply_to(&json!({})),
            (Some(json!({})), vec![]),
        );
    }

    #[test]
    fn should_handle_mixed_case_styles() {
        assert_eq!(
            selection!("$->propertiesToCamelCase").apply_to(&json!({
                "snake_case": 1,
                "PascalCase": 2,
                "SCREAMING_CASE": 3,
                "camelCase": 4,
            })),
            (
                Some(json!({
                    "snakeCase": 1,
                    "pascalCase": 2,
                    "screamingCase": 3,
                    "camelCase": 4,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn should_error_on_non_object_input() {
        assert_eq!(
            selection!("notAnObject->propertiesToCamelCase").apply_to(&json!({
                "notAnObject": true,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->propertiesToCamelCase requires an object input, not boolean",
                    "path": ["notAnObject", "->propertiesToCamelCase"],
                    "range": [13, 34],
                }))]
            ),
        );
    }

    #[test]
    fn should_error_on_too_many_arguments() {
        assert_eq!(
            selection!("$->propertiesToCamelCase(true, false)").apply_to(&json!({})),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->propertiesToCamelCase takes at most one argument, but 2 were provided",
                    "path": ["->propertiesToCamelCase"],
                    "range": [3, 24],
                }))]
            ),
        );
    }

    #[test]
    fn should_error_on_non_boolean_argument() {
        assert_eq!(
            selection!("$->propertiesToCamelCase(42)").apply_to(&json!({})),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->propertiesToCamelCase requires a boolean argument",
                    "path": ["->propertiesToCamelCase"],
                    "range": [3, 24],
                }))]
            ),
        );
    }

    #[test]
    fn should_warn_on_key_collision() {
        let (result, errors) = selection!("$->propertiesToCamelCase").apply_to(&json!({
            "foo_bar": 1,
            "fooBar": 2,
        }));
        // Last-write-wins: the result should have fooBar with the last value
        assert!(result.is_some());
        let obj = result.unwrap();
        assert!(obj.get("fooBar").is_some());
        // Should have a collision warning
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message().contains("key collision"));
    }
}

#[cfg(test)]
mod shape_tests {
    use shape::location::Location;
    use shape::location::SourceId;

    use super::*;
    use crate::connectors::json_selection::lit_expr::LitExpr;

    fn get_location() -> Location {
        Location {
            source_id: SourceId::new("test".to_string()),
            span: 0..23,
        }
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        properties_to_camel_case_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("propertiesToCamelCase".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::unknown([]),
        )
    }

    #[test]
    fn shape_should_transform_object_field_names() {
        let mut fields = Shape::empty_map();
        fields.insert("snake_case".to_string(), Shape::int([]));
        fields.insert("PascalCase".to_string(), Shape::string([]));
        let input = Shape::object(fields, Shape::none(), []);

        let result = get_shape(vec![], input);

        match result.case() {
            ShapeCase::Object { fields, .. } => {
                assert!(fields.contains_key("snakeCase"));
                assert!(fields.contains_key("pascalCase"));
                assert!(!fields.contains_key("snake_case"));
                assert!(!fields.contains_key("PascalCase"));
            }
            _ => panic!("Expected object shape"),
        }
    }

    #[test]
    fn shape_should_return_unknown_for_unknown_input() {
        let input = Shape::unknown([]);
        let result = get_shape(vec![], input.clone());
        assert_eq!(result, input);
    }

    #[test]
    fn shape_should_error_on_non_object_input() {
        let result = get_shape(vec![], Shape::string([]));
        assert!(matches!(result.case(), ShapeCase::Error { .. }));
    }

    #[test]
    fn shape_should_error_on_too_many_args() {
        let result = get_shape(
            vec![
                WithRange::new(LitExpr::Bool(true), None),
                WithRange::new(LitExpr::Bool(false), None),
            ],
            Shape::unknown([]),
        );
        assert!(matches!(result.case(), ShapeCase::Error { .. }));
    }

    #[test]
    fn shape_should_handle_no_args_as_none() {
        let location = get_location();
        let result = properties_to_camel_case_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("propertiesToCamelCase".to_string(), Some(location.span)),
            None,
            Shape::unknown([]),
            Shape::none(),
        );
        assert_eq!(result, Shape::unknown([]));
    }

    #[test]
    fn shape_should_recursively_transform_nested_object_fields() {
        let mut inner_fields = Shape::empty_map();
        inner_fields.insert("inner_key".to_string(), Shape::int([]));
        let inner = Shape::object(inner_fields, Shape::none(), []);

        let mut outer_fields = Shape::empty_map();
        outer_fields.insert("outer_key".to_string(), inner);
        let input = Shape::object(outer_fields, Shape::none(), []);

        let result = get_shape(vec![], input);

        match result.case() {
            ShapeCase::Object { fields, .. } => {
                assert!(fields.contains_key("outerKey"));
                assert!(!fields.contains_key("outer_key"));
                let inner_shape = fields.get("outerKey").unwrap();
                match inner_shape.case() {
                    ShapeCase::Object { fields, .. } => {
                        assert!(fields.contains_key("innerKey"));
                        assert!(!fields.contains_key("inner_key"));
                    }
                    _ => panic!("Expected nested object shape"),
                }
            }
            _ => panic!("Expected object shape"),
        }
    }

    #[test]
    fn shape_should_not_transform_nested_fields_when_recursive_false() {
        let mut inner_fields = Shape::empty_map();
        inner_fields.insert("inner_key".to_string(), Shape::int([]));
        let inner = Shape::object(inner_fields, Shape::none(), []);

        let mut outer_fields = Shape::empty_map();
        outer_fields.insert("outer_key".to_string(), inner);
        let input = Shape::object(outer_fields, Shape::none(), []);

        let result = get_shape(vec![WithRange::new(LitExpr::Bool(false), None)], input);

        match result.case() {
            ShapeCase::Object { fields, .. } => {
                assert!(fields.contains_key("outerKey"));
                let inner_shape = fields.get("outerKey").unwrap();
                match inner_shape.case() {
                    ShapeCase::Object { fields, .. } => {
                        assert!(fields.contains_key("inner_key"));
                        assert!(!fields.contains_key("innerKey"));
                    }
                    _ => panic!("Expected nested object shape"),
                }
            }
            _ => panic!("Expected object shape"),
        }
    }

    #[test]
    fn shape_should_recursively_transform_objects_inside_arrays() {
        let mut item_fields = Shape::empty_map();
        item_fields.insert("array_item_key".to_string(), Shape::string([]));
        let item = Shape::object(item_fields, Shape::none(), []);

        let mut outer_fields = Shape::empty_map();
        outer_fields.insert("my_list".to_string(), Shape::list(item, []));
        let input = Shape::object(outer_fields, Shape::none(), []);

        let result = get_shape(vec![], input);

        match result.case() {
            ShapeCase::Object { fields, .. } => {
                assert!(fields.contains_key("myList"));
                let list_shape = fields.get("myList").unwrap();
                let item_shape = list_shape.any_item([]);
                match item_shape.case() {
                    ShapeCase::Object { fields, .. } => {
                        assert!(fields.contains_key("arrayItemKey"));
                        assert!(!fields.contains_key("array_item_key"));
                    }
                    _ => panic!("Expected object shape inside array"),
                }
            }
            _ => panic!("Expected object shape"),
        }
    }

    #[test]
    fn shape_should_not_transform_array_contents_when_recursive_false() {
        let mut item_fields = Shape::empty_map();
        item_fields.insert("array_item_key".to_string(), Shape::string([]));
        let item = Shape::object(item_fields, Shape::none(), []);

        let mut outer_fields = Shape::empty_map();
        outer_fields.insert("my_list".to_string(), Shape::list(item, []));
        let input = Shape::object(outer_fields, Shape::none(), []);

        let result = get_shape(vec![WithRange::new(LitExpr::Bool(false), None)], input);

        match result.case() {
            ShapeCase::Object { fields, .. } => {
                assert!(fields.contains_key("myList"));
                let list_shape = fields.get("myList").unwrap();
                let item_shape = list_shape.any_item([]);
                match item_shape.case() {
                    ShapeCase::Object { fields, .. } => {
                        assert!(fields.contains_key("array_item_key"));
                        assert!(!fields.contains_key("arrayItemKey"));
                    }
                    _ => panic!("Expected object shape inside array"),
                }
            }
            _ => panic!("Expected object shape"),
        }
    }

    #[test]
    fn shape_should_preserve_field_value_types() {
        let mut fields = Shape::empty_map();
        fields.insert("int_field".to_string(), Shape::int([]));
        fields.insert("string_field".to_string(), Shape::string([]));
        fields.insert("bool_field".to_string(), Shape::bool([]));
        let input = Shape::object(fields, Shape::none(), []);

        let result = get_shape(vec![], input);

        match result.case() {
            ShapeCase::Object { fields, .. } => {
                assert!(matches!(
                    fields.get("intField").unwrap().case(),
                    ShapeCase::Int(_)
                ));
                assert!(matches!(
                    fields.get("stringField").unwrap().case(),
                    ShapeCase::String { .. }
                ));
                assert!(matches!(
                    fields.get("boolField").unwrap().case(),
                    ShapeCase::Bool(_)
                ));
            }
            _ => panic!("Expected object shape"),
        }
    }
}
