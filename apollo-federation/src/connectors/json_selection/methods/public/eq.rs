use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::location::SourceId;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::impl_arrow_method;

impl_arrow_method!(EqMethod, eq_method, eq_shape);
/// Returns true if argument is equal to the applied to value or false if they are not equal.
/// Simple examples:
///
/// $(123)->eq(123)       results in true
/// $(123)->eq(456)       results in false
fn eq_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        if let [arg] = args.as_slice() {
            let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path);
            let matches = value_opt.is_some_and(|value| match (data, &value) {
                // Number comparisons: Always convert to float so 1 == 1.0
                (JSON::Number(left), JSON::Number(right)) => {
                    left.as_f64().unwrap_or(0.0) == right.as_f64().unwrap_or(0.0)
                }
                // Everything else
                _ => &value == data,
            });

            return (Some(JSON::Bool(matches)), arg_errors);
        }
    }
    (
        None,
        vec![ApplyToError::new(
            format!(
                "Method ->{} requires exactly one argument",
                method_name.as_ref()
            ),
            input_path.to_vec(),
            method_name.range(),
        )],
    )
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn eq_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
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
            method_name.shape_location(source_id),
        );
    };
    let arg_shape = first_arg.compute_output_shape(
        input_shape.clone(),
        dollar_shape,
        named_var_shapes,
        source_id,
    );

    // Ensures that the arguments are of the same type... this includes covering cases like int/float and unknown/name
    if !(input_shape.accepts(&arg_shape) || arg_shape.accepts(&input_shape)) {
        return Shape::error_with_partial(
            format!(
                "Method ->{} requires the applied to value and argument to be the same type to be comparable.",
                method_name.as_ref()
            ),
            Shape::bool_value(false, method_name.shape_location(source_id)),
            method_name.shape_location(source_id),
        );
    }

    Shape::bool(method_name.shape_location(source_id))
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn eq_should_return_true_when_applied_to_equals_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq(123)
                "#
            )
            .apply_to(&json!({ "value": 123 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_false_when_applied_to_does_not_equal_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq(1234)
                "#
            )
            .apply_to(&json!({ "value": 123 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_true_when_applied_to_numbers_of_different_types() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq(1)
                "#
            )
            .apply_to(&json!({ "value": 1.0 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_true_when_applied_to_negative_numbers_of_different_types() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq(-1)
                "#
            )
            .apply_to(&json!({ "value": -1.0 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_true_when_applied_to_equals_string_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq("hello")
                "#
            )
            .apply_to(&json!({ "value": "hello" })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_false_when_applied_to_does_not_equal_string_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq("world")
                "#
            )
            .apply_to(&json!({ "value": "hello" })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_true_when_applied_to_equals_bool_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq(true)
                "#
            )
            .apply_to(&json!({ "value": true })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_false_when_applied_to_does_not_equal_bool_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq(false)
                "#
            )
            .apply_to(&json!({ "value": true })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_true_when_applied_to_equals_object_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq({"name": "John", "age": 30})
                "#
            )
            .apply_to(&json!({ "value": {"name": "John", "age": 30} })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_false_when_applied_to_does_not_equal_object_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq({"name": "Jane", "age": 25})
                "#
            )
            .apply_to(&json!({ "value": {"name": "John", "age": 30} })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_true_when_applied_to_equals_array_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq([1, 2, 3])
                "#
            )
            .apply_to(&json!({ "value": [1, 2, 3] })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn eq_should_return_false_when_applied_to_does_not_equal_array_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->eq([4, 5, 6])
                "#
            )
            .apply_to(&json!({ "value": [1, 2, 3] })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }
}
