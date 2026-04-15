use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use super::keys_to_camel_case::transform_keys;
use super::keys_to_camel_case::transform_shape;
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

impl_arrow_method!(
    KeysToCamelCaseDeepMethod,
    keys_to_camel_case_deep_method,
    keys_to_camel_case_deep_shape
);

/// Recursively converts all keys of an object (and nested objects) to camelCase.
///
/// Handles PascalCase, snake_case, and SCREAMING_SNAKE_CASE inputs.
/// The transformation applies recursively to nested objects and objects
/// within arrays. For shallow (top-level only) transformation, use
/// `keysToCamelCase`.
///
/// Examples:
///
/// $->keysToCamelCaseDeep
/// given {"outer_key": {"inner_key": 1}}
/// results in {"outerKey": {"innerKey": 1}}
fn keys_to_camel_case_deep_method(
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
                    method_name.as_ref(),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    }

    match data {
        JSON::Object(_) => {
            let mut errors = Vec::new();
            let result = transform_keys(data, true, method_name, input_path, spec, &mut errors);
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

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn keys_to_camel_case_deep_shape(
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
                method_name.as_ref(),
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    let locations = method_name.shape_location(context.source_id());

    match input_shape.case() {
        ShapeCase::Object { .. } => transform_shape(input_shape, true, locations),
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
    fn should_apply_recursively_to_nested_objects() {
        assert_eq!(
            selection!("$->keysToCamelCaseDeep").apply_to(&json!({
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
            selection!("$->keysToCamelCaseDeep").apply_to(&json!({
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
    fn should_error_on_non_object_input() {
        assert_eq!(
            selection!("notAnObject->keysToCamelCaseDeep").apply_to(&json!({
                "notAnObject": true,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->keysToCamelCaseDeep requires an object input, not boolean",
                    "path": ["notAnObject", "->keysToCamelCaseDeep"],
                    "range": [13, 32],
                }))]
            ),
        );
    }

    #[test]
    fn should_error_when_args_provided() {
        assert_eq!(
            selection!("$->keysToCamelCaseDeep(true)").apply_to(&json!({})),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->keysToCamelCaseDeep does not take any arguments",
                    "path": ["->keysToCamelCaseDeep"],
                    "range": [3, 22],
                }))]
            ),
        );
    }

    #[test]
    fn should_handle_empty_object() {
        assert_eq!(
            selection!("$->keysToCamelCaseDeep").apply_to(&json!({})),
            (Some(json!({})), vec![]),
        );
    }

    #[test]
    fn should_warn_on_key_collision() {
        let (result, errors) = selection!("$->keysToCamelCaseDeep").apply_to(&json!({
            "foo_bar": 1,
            "fooBar": 2,
        }));
        assert!(result.is_some());
        let obj = result.unwrap();
        assert!(obj.get("fooBar").is_some());
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

    fn get_deep_shape(input: Shape) -> Shape {
        let location = get_location();
        keys_to_camel_case_deep_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("keysToCamelCaseDeep".to_string(), Some(location.span)),
            None,
            input,
            Shape::unknown([]),
        )
    }

    #[test]
    fn should_transform_nested_object_fields() {
        let mut inner_fields = Shape::empty_map();
        inner_fields.insert("inner_key".to_string(), Shape::int([]));
        let inner = Shape::object(inner_fields, Shape::none(), []);

        let mut outer_fields = Shape::empty_map();
        outer_fields.insert("outer_key".to_string(), inner);
        let input = Shape::object(outer_fields, Shape::none(), []);

        let result = get_deep_shape(input);

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
    fn should_transform_objects_inside_arrays() {
        let mut item_fields = Shape::empty_map();
        item_fields.insert("array_item_key".to_string(), Shape::string([]));
        let item = Shape::object(item_fields, Shape::none(), []);

        let mut outer_fields = Shape::empty_map();
        outer_fields.insert("my_list".to_string(), Shape::list(item, []));
        let input = Shape::object(outer_fields, Shape::none(), []);

        let result = get_deep_shape(input);

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
    fn should_error_when_args_provided() {
        let location = get_location();
        let result = keys_to_camel_case_deep_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("keysToCamelCaseDeep".to_string(), Some(location.span)),
            Some(&MethodArgs {
                args: vec![WithRange::new(LitExpr::Bool(true), None)],
                range: None,
            }),
            Shape::unknown([]),
            Shape::unknown([]),
        );
        assert!(matches!(result.case(), ShapeCase::Error { .. }));
    }

    #[test]
    fn should_return_unknown_for_unknown_input() {
        let input = Shape::unknown([]);
        let result = get_deep_shape(input.clone());
        assert_eq!(result, input);
    }

    #[test]
    fn should_error_on_non_object_input() {
        let result = get_deep_shape(Shape::string([]));
        assert!(matches!(result.case(), ShapeCase::Error { .. }));
    }

    #[test]
    fn should_preserve_field_value_types() {
        let mut fields = Shape::empty_map();
        fields.insert("int_field".to_string(), Shape::int([]));
        fields.insert("string_field".to_string(), Shape::string([]));
        fields.insert("bool_field".to_string(), Shape::bool([]));
        let input = Shape::object(fields, Shape::none(), []);

        let result = get_deep_shape(input);

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
