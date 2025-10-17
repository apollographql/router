use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(FindMethod, find_method, find_shape);
/// "Find" is an array method that returns the first item that matches the criteria.
/// You can use it to find the first value in an array based on a boolean condition.
/// If no matching item is found, it returns None.
///
/// For example, given a selection of [1, 2, 3, 4, 5]:
///
/// $->find(@->eq(3))      result is 3
/// $->find(@->gt(3))      result is 4
/// $->find(@->eq(10))     result is None (no match)
///
/// We are taking each value passed into find via @ and running the condition function against that value.
/// The first value where the condition returns true will be returned.
///
/// Example with objects:
/// users->find(@.active->eq(true))    returns the first user where active is true
fn find_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(first_arg) = method_args.and_then(|args| args.args.first()) else {
        return (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires one argument", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    if let JSON::Array(array) = data {
        let mut errors = Vec::new();

        for (i, element) in array.iter().enumerate() {
            let input_path = input_path.append(JSON::Number(i.into()));
            let (applied_opt, arg_errors) =
                first_arg.apply_to_path(element, vars, &input_path, spec);
            errors.extend(arg_errors);

            match applied_opt {
                Some(JSON::Bool(true)) => {
                    // Found the first matching element, return it
                    return (Some(element.clone()), errors);
                }
                Some(JSON::Bool(false)) => {
                    // Condition is false or errored, continue searching
                }
                Some(_) | None => {
                    // Condition returned a non-boolean value, this is an error
                    errors.push(ApplyToError::new(
                        format!(
                            "->{} condition must return a boolean value",
                            method_name.as_ref()
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    ));
                    return (None, errors);
                }
            }
        }

        // No matching element found
        (None, errors)
    } else {
        // For non-array inputs, treat as single-element array
        // Apply find condition and return either the value or None
        let (condition_result, mut condition_errors) =
            first_arg.apply_to_path(data, vars, input_path, spec);

        match condition_result {
            Some(JSON::Bool(true)) => (Some(data.clone()), condition_errors),
            Some(JSON::Bool(false)) => (None, condition_errors),
            Some(_) => {
                // Condition returned a non-boolean value, this is an error
                condition_errors.push(ApplyToError::new(
                    format!(
                        "->{} condition must return a boolean value",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                    spec,
                ));
                (None, condition_errors)
            }
            None => {
                // Condition errored, errors are already in condition_errors
                (None, condition_errors)
            }
        }
    }
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn find_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();
    if arg_count > 1 {
        return Shape::error(
            format!(
                "Method ->{} requires only one argument, but {arg_count} were provided",
                method_name.as_ref(),
            ),
            vec![],
        );
    }

    let Some(first_arg) = method_args.and_then(|args| args.args.first()) else {
        return Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(context.source_id()),
        );
    };

    // Compute the shape of the find condition argument
    let condition_shape =
        first_arg.compute_output_shape(context, input_shape.clone(), dollar_shape);

    // Validate that the condition evaluates to a boolean or
    // something that could become a boolean
    if !(matches!(condition_shape.case(), ShapeCase::Bool(_)) ||
        // This allows Unknown and Name shapes, which can produce boolean
        // values at runtime, without any runtime errors.
        condition_shape.accepts(&Shape::unknown([])))
    {
        return Shape::error(
            format!(
                "->{} condition must return a boolean value",
                method_name.as_ref()
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    // Find returns a single item (or None), so we return the item type of the input shape
    Shape::one([Shape::none(), input_shape.any_item([])], [])
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn find_should_return_first_matching_element() {
        assert_eq!(
            selection!("$->echo([1,2,3,4,5])->find(@->eq(3))").apply_to(&json!(null)),
            (Some(json!(3)), vec![]),
        );
    }

    #[test]
    fn find_should_return_first_match_when_multiple_exist() {
        assert_eq!(
            selection!("$->echo([1,2,3,2,1])->find(@->eq(2))").apply_to(&json!(null)),
            (Some(json!(2)), vec![]),
        );
    }

    #[test]
    fn find_should_return_none_when_no_matches() {
        assert_eq!(
            selection!("$->echo([1,2,3])->find(@->eq(5))").apply_to(&json!(null)),
            (None, vec![]),
        );
    }

    #[test]
    fn find_should_work_with_object_properties() {
        assert_eq!(
            selection!("users->find(@.active->eq(true))").apply_to(&json!({
                "users": [
                    { "name": "Alice", "active": false },
                    { "name": "Bob", "active": true },
                    { "name": "Charlie", "active": true },
                ],
            })),
            (Some(json!({ "name": "Bob", "active": true })), vec![]),
        );
    }

    #[test]
    fn find_should_handle_non_array_input_true_condition() {
        assert_eq!(
            selection!("value->find(@->eq(123))").apply_to(&json!({
                "value": 123,
            })),
            (Some(json!(123)), vec![]),
        );
    }

    #[test]
    fn find_should_handle_non_array_input_false_condition() {
        assert_eq!(
            selection!("value->find(@->eq(456))").apply_to(&json!({
                "value": 123,
            })),
            (None, vec![]),
        );
    }

    #[test]
    fn find_should_handle_complex_conditions() {
        // Find the first number greater than 3
        assert_eq!(
            selection!("numbers->find(@->gt(3))").apply_to(&json!({
                "numbers": [1, 2, 3, 4, 5, 6],
            })),
            (Some(json!(4)), vec![]),
        );
    }

    #[test]
    fn find_should_error_with_non_boolean_results() {
        // Elements where the condition doesn't return a boolean should cause an error
        // Using a string condition that evaluates to a non-boolean
        let result = selection!("values->find(@->echo('not_boolean'))").apply_to(&json!({
            "values": [1, 2, 3],
        }));

        // Should return None and have errors
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("->find condition must return a boolean value")
        );
    }

    #[test]
    fn find_should_chain_with_other_methods() {
        // Find the first number equal to 3, then add 10 to it
        assert_eq!(
            selection!("$->echo([1,2,3,4,5])->find(@->eq(3))->add(10)").apply_to(&json!(null)),
            (Some(json!(13)), vec![]),
        );
    }

    #[test]
    fn find_should_work_with_string_values() {
        assert_eq!(
            selection!("words->find(@->eq('hello'))").apply_to(&json!({
                "words": ["world", "hello", "test", "hello"],
            })),
            (Some(json!("hello")), vec![]),
        );
    }

    #[test]
    fn find_should_handle_mixed_types() {
        assert_eq!(
            selection!("values->find(@->typeof->eq('string'))").apply_to(&json!({
                "values": [1, "hello", 2.5, true, null, 42],
            })),
            (Some(json!("hello")), vec![]),
        );
    }

    #[test]
    fn find_should_return_none_for_empty_array() {
        assert_eq!(
            selection!("$->echo([])->find(@->eq(1))").apply_to(&json!(null)),
            (None, vec![]),
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use shape::location::Location;
    use shape::location::SourceId;

    use super::*;
    use crate::connectors::Key;
    use crate::connectors::PathSelection;
    use crate::connectors::json_selection::PathList;
    use crate::connectors::json_selection::lit_expr::LitExpr;

    fn get_location() -> Location {
        Location {
            source_id: SourceId::new("test".to_string()),
            span: 0..4,
        }
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        find_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("find".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::unknown([]),
        )
    }

    #[test]
    fn find_shape_should_return_item_type_on_valid_boolean_condition() {
        let input_shape = Shape::list(Shape::int([]), []);
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                input_shape.clone()
            ),
            Shape::one([Shape::none(), input_shape.any_item([])], [])
        );
    }

    #[test]
    fn find_shape_should_return_item_type_for_array_input() {
        let item_shape = Shape::string([]);
        let input_shape = Shape::list(item_shape, []);
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                input_shape.clone()
            ),
            Shape::one([Shape::none(), input_shape.any_item([])], [])
        );
    }

    #[test]
    fn find_shape_should_return_item_type_for_single_item_input() {
        let input_shape = Shape::string([]);
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                input_shape.clone()
            ),
            Shape::one([Shape::none(), input_shape.any_item([])], [])
        );
    }

    #[test]
    fn find_shape_should_error_on_non_boolean_condition() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::String("not_bool".to_string()),
                    None
                )],
                Shape::string([])
            ),
            Shape::error(
                "->find condition must return a boolean value".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn find_shape_should_error_on_no_args() {
        assert_eq!(
            get_shape(vec![], Shape::string([])),
            Shape::error(
                "Method ->find requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn find_shape_should_error_on_too_many_args() {
        assert_eq!(
            get_shape(
                vec![
                    WithRange::new(LitExpr::Bool(true), None),
                    WithRange::new(LitExpr::Bool(false), None)
                ],
                Shape::string([])
            ),
            Shape::error(
                "Method ->find requires only one argument, but 2 were provided".to_string(),
                []
            )
        );
    }

    #[test]
    fn find_shape_should_error_on_none_args() {
        let location = get_location();
        assert_eq!(
            find_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("find".to_string(), Some(location.span)),
                None,
                Shape::string([]),
                Shape::none(),
            ),
            Shape::error(
                "Method ->find requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn find_shape_should_handle_unknown_condition_shape() {
        let path = LitExpr::Path(PathSelection {
            path: PathList::Key(
                Key::field("a").into_with_range(),
                PathList::Empty.into_with_range(),
            )
            .into_with_range(),
        });
        let input_shape = Shape::list(Shape::int([]), []);
        // Unknown shapes should be accepted as they could produce boolean values at runtime
        let result = get_shape(vec![path.into_with_range()], input_shape.clone());
        assert_eq!(
            result,
            Shape::one([Shape::none(), input_shape.any_item([])], [])
        );
    }
}
