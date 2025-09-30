use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
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
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut has_non_boolean_error = false;

        for (i, element) in array.iter().enumerate() {
            let input_path = input_path.append(JSON::Number(i.into()));
            let (applied_opt, arg_errors) =
                first_arg.apply_to_path(element, vars, &input_path, spec);
            errors.extend(arg_errors);

            match applied_opt {
                Some(JSON::Bool(true)) => {
                    output.push(element.clone());
                }
                Some(JSON::Bool(false)) => {
                    // Condition is false or errored, exclude the element
                }
                Some(_) | None => {
                    // Condition returned a non-boolean value, this is an error
                    has_non_boolean_error = true;
                    errors.push(ApplyToError::new(
                        format!(
                            "->{} condition must return a boolean value",
                            method_name.as_ref()
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
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
            first_arg.apply_to_path(data, vars, input_path, spec);

        match condition_result {
            Some(JSON::Bool(true)) => (Some(JSON::Array(vec![data.clone()])), condition_errors),
            Some(JSON::Bool(false)) => (Some(JSON::Array(vec![])), condition_errors),
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
fn filter_shape(
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

    // Compute the shape of the filter condition argument
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

    Shape::list(input_shape.any_item([]), input_shape.locations)
}

#[cfg(test)]
mod method_tests {
    use serde_json_bytes::json;

    use crate::connectors::ConnectSpec;
    use crate::connectors::json_selection::ApplyToError;
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
                .contains("->filter condition must return a boolean value")
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

    #[rstest::rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn filter_should_return_none_when_argument_evaluates_to_none(#[case] spec: ConnectSpec) {
        assert_eq!(
            selection!("$.a->filter($.missing)", spec).apply_to(&json!({
                "a": [1, 2, 3],
            })),
            (
                None,
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .missing not found in object",
                        "path": ["missing"],
                        "range": [14, 21],
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "->filter condition must return a boolean value",
                        "path": ["a", "->filter", 0],
                        "range": [5, 11],
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "->filter condition must return a boolean value",
                        "path": ["a", "->filter", 1],
                        "range": [5, 11],
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "->filter condition must return a boolean value",
                        "path": ["a", "->filter", 2],
                        "range": [5, 11],
                        "spec": spec.to_string(),
                    }))
                ]
            ),
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
            span: 0..6,
        }
    }

    fn get_shape(args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        filter_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new("filter".to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::unknown([]),
        )
    }

    #[test]
    fn filter_shape_should_return_list_on_valid_boolean_condition() {
        let input_shape = Shape::list(Shape::int([]), []);
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                input_shape.clone()
            ),
            input_shape
        );
    }

    #[test]
    fn filter_shape_should_return_list_for_array_input() {
        let item_shape = Shape::string([]);
        let input_shape = Shape::list(item_shape, []);
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                input_shape.clone()
            ),
            Shape::list(input_shape.any_item([]), input_shape.locations)
        );
    }

    #[test]
    fn filter_shape_should_return_list_for_single_item_input() {
        let input_shape = Shape::string([]);
        assert_eq!(
            get_shape(
                vec![WithRange::new(LitExpr::Bool(true), None)],
                input_shape.clone()
            ),
            Shape::list(input_shape.any_item([]), input_shape.locations)
        );
    }

    #[test]
    fn filter_shape_should_error_on_non_boolean_condition() {
        assert_eq!(
            get_shape(
                vec![WithRange::new(
                    LitExpr::String("not_bool".to_string()),
                    None
                )],
                Shape::string([])
            ),
            Shape::error(
                "->filter condition must return a boolean value".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn filter_shape_should_error_on_no_args() {
        assert_eq!(
            get_shape(vec![], Shape::string([])),
            Shape::error(
                "Method ->filter requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn filter_shape_should_error_on_too_many_args() {
        assert_eq!(
            get_shape(
                vec![
                    WithRange::new(LitExpr::Bool(true), None),
                    WithRange::new(LitExpr::Bool(false), None)
                ],
                Shape::string([])
            ),
            Shape::error(
                "Method ->filter requires only one argument, but 2 were provided".to_string(),
                []
            )
        );
    }

    #[test]
    fn filter_shape_should_error_on_none_args() {
        let location = get_location();
        assert_eq!(
            filter_shape(
                &ShapeContext::new(location.source_id),
                &WithRange::new("filter".to_string(), Some(location.span)),
                None,
                Shape::string([]),
                Shape::none(),
            ),
            Shape::error(
                "Method ->filter requires one argument".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn filter_shape_should_handle_unknown_condition_shape() {
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
        assert_eq!(result, Shape::list(input_shape.any_item([]), []));
    }
}
