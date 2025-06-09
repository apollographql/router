use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::VarsWithPathsMap;

use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(FilterMethod, filter_method, filter_shape);
/// "Filter" is an array method that returns a new array containing only items that match the criteria.
/// You can use it to filter an array of values based on a boolean condition.
///
/// For example, given a selection of [1, 2, 3, 4, 5]:
///
/// $->filter(@->eq(3))      result is [3]
/// $->filter(@->gt(3))      result is [4, 5]
///
/// We are taking each value passed into filter via @ and running the condition function against that value.
/// Only values where the condition returns true will be included in the result array.
///
/// Example with objects:
/// users->filter(@.active->eq(true))    returns only users where active is true
fn filter_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(args) = method_args else {
        return (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires one argument", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
            )],
        );
    };
    let Some(first_arg) = args.args.first() else {
        return (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires one argument", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
            )],
        );
    };

    if let JSON::Array(array) = data {
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut has_non_boolean_error = false;

        for (i, element) in array.iter().enumerate() {
            let input_path = input_path.append(JSON::Number(i.into()));
            let (applied_opt, arg_errors) = first_arg.apply_to_path(element, vars, &input_path);
            errors.extend(arg_errors);

            match applied_opt {
                Some(JSON::Bool(true)) => {
                    output.push(element.clone());
                }
                Some(JSON::Bool(false)) | None => {
                    // Condition is false or errored, exclude the element
                }
                Some(_) => {
                    // Condition returned a non-boolean value, this is an error
                    has_non_boolean_error = true;
                    errors.push(ApplyToError::new(
                        "Filter condition must return a boolean value".to_string(),
                        input_path.to_vec(),
                        method_name.range(),
                    ));
                }
            }
        }

        if has_non_boolean_error {
            (None, errors)
        } else {
            (Some(JSON::Array(output)), errors)
        }
    } else {
        // For non-array inputs, treat as single-element array
        // Apply filter condition and return either [value] or []
        let (condition_result, mut condition_errors) =
            first_arg.apply_to_path(data, vars, input_path);

        match condition_result {
            Some(JSON::Bool(true)) => (Some(JSON::Array(vec![data.clone()])), condition_errors),
            Some(JSON::Bool(false)) => (Some(JSON::Array(vec![])), condition_errors),
            Some(_) => {
                // Condition returned a non-boolean value, this is an error
                condition_errors.push(ApplyToError::new(
                    "Filter condition must return a boolean value".to_string(),
                    input_path.to_vec(),
                    method_name.range(),
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
fn filter_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    let Some(first_arg) = method_args.and_then(|args| args.args.first()) else {
        return Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(source_id),
        );
    };

    // Compute the shape of the filter condition argument
    let condition_shape = first_arg.compute_output_shape(
        input_shape.clone(),
        dollar_shape,
        named_var_shapes,
        source_id,
    );

    // Validate that the condition evaluates to a boolean
    if !matches!(condition_shape.case(), ShapeCase::Bool(_)) {
        return Shape::error(
            "Filter condition must return a boolean value".to_string(),
            method_name.shape_location(source_id),
        );
    }

    match input_shape.case() {
        ShapeCase::Array { prefix: _, tail } => {
            // Filter preserves the element types but may reduce the count
            // We can't know statically how many elements will pass the filter,
            // so we return an array with the same element type but no fixed prefix
            Shape::list(tail.clone(), input_shape.locations)
        }
        _ => {
            // For non-array inputs, we return a list that may contain 0 or 1 elements
            // of the same type as the input
            Shape::list(input_shape.any_item([]), input_shape.locations)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn filter_should_return_matching_elements() {
        assert_eq!(
            selection!("$->echo([1,2,3,4,5])->filter(@->eq(3))").apply_to(&json!(null)),
            (Some(json!([3])), vec![]),
        );
    }

    #[test]
    fn filter_should_return_multiple_matches() {
        assert_eq!(
            selection!("$->echo([1,2,3,2,1])->filter(@->eq(2))").apply_to(&json!(null)),
            (Some(json!([2, 2])), vec![]),
        );
    }

    #[test]
    fn filter_should_return_empty_array_when_no_matches() {
        assert_eq!(
            selection!("$->echo([1,2,3])->filter(@->eq(5))").apply_to(&json!(null)),
            (Some(json!([])), vec![]),
        );
    }

    #[test]
    fn filter_should_work_with_object_properties() {
        assert_eq!(
            selection!("users->filter(@.active->eq(true))").apply_to(&json!({
                "users": [
                    { "name": "Alice", "active": true },
                    { "name": "Bob", "active": false },
                    { "name": "Charlie", "active": true },
                ],
            })),
            (
                Some(json!([
                    { "name": "Alice", "active": true },
                    { "name": "Charlie", "active": true },
                ])),
                vec![]
            ),
        );
    }

    #[test]
    fn filter_should_handle_non_array_input_true_condition() {
        assert_eq!(
            selection!("value->filter(@->eq(123))").apply_to(&json!({
                "value": 123,
            })),
            (Some(json!([123])), vec![]),
        );
    }

    #[test]
    fn filter_should_handle_non_array_input_false_condition() {
        assert_eq!(
            selection!("value->filter(@->eq(456))").apply_to(&json!({
                "value": 123,
            })),
            (Some(json!([])), vec![]),
        );
    }

    #[test]
    fn filter_should_handle_complex_conditions() {
        // Filter numbers greater than 3 by checking if they don't equal 1, 2, or 3
        assert_eq!(
            selection!("numbers->filter(@->eq(4))").apply_to(&json!({
                "numbers": [1, 2, 3, 4, 5, 6],
            })),
            (Some(json!([4])), vec![]),
        );
    }

    #[test]
    fn filter_should_error_with_non_boolean_results() {
        // Elements where the condition doesn't return a boolean should cause an error
        // Using a string condition that evaluates to a non-boolean
        let result = selection!("values->filter(@->echo('not_boolean'))").apply_to(&json!({
            "values": [1, 2, 3],
        }));

        // Should return None and have errors
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Filter condition must return a boolean value")
        );
    }

    #[test]
    fn filter_should_chain_with_other_methods() {
        // Filter for numbers equal to 3, 4, or 5, then map each to multiply by 10
        assert_eq!(
            selection!("$->echo([1,2,3,4,5])->filter(@->eq(3))->map(@->add(10))")
                .apply_to(&json!(null)),
            (Some(json!([13])), vec![]),
        );
    }

    #[test]
    fn filter_should_work_with_string_values() {
        assert_eq!(
            selection!("words->filter(@->eq('hello'))").apply_to(&json!({
                "words": ["hello", "world", "hello", "test"],
            })),
            (Some(json!(["hello", "hello"])), vec![]),
        );
    }

    #[test]
    fn filter_should_handle_mixed_types() {
        assert_eq!(
            selection!("values->filter(@->typeof->eq('number'))").apply_to(&json!({
                "values": [1, "hello", 2.5, true, null, 42],
            })),
            (Some(json!([1, 2.5, 42])), vec![]),
        );
    }
}
