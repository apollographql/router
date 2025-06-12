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

impl_arrow_method!(GteMethod, gte_method, gte_shape);
/// Returns true if the applied to value is greater than or equal to the argument value.
/// Simple examples:
///
/// $(3)->gte(3)       results in true
/// $(4)->gte(3)       results in true
/// $(2)->gte(3)       results in false
/// $("a")->gte("b")   results in false
/// $("c")->gte("b")   results in true
fn gte_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        let (value_opt, arg_errors) = first_arg.apply_to_path(data, vars, input_path);
        // We have to do this because Value doesn't implement PartialOrd
        let matches = value_opt.is_some_and(|value| {
            match (data, &value) {
                // Number comparisons
                (JSON::Number(left), JSON::Number(right)) => {
                    left.as_f64().unwrap_or(0.0) >= right.as_f64().unwrap_or(0.0)
                }
                // String comparisons
                (JSON::String(left), JSON::String(right)) => left >= right,
                // Boolean comparisons
                (JSON::Bool(left), JSON::Bool(right)) => left >= right,
                // Null comparisons (null == null)
                (JSON::Null, JSON::Null) => true,
                // Mixed types or uncomparable types (including arrays and objects) return false
                _ => false,
            }
        });

        return (Some(JSON::Bool(matches)), arg_errors);
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
fn gte_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    Shape::bool(method_name.shape_location(source_id))
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn gte_should_return_true_when_applied_to_number_is_greater_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gte(3)
                "#
            )
            .apply_to(&json!({ "value": 4 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_return_true_when_applied_to_number_equals_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gte(3)
                "#
            )
            .apply_to(&json!({ "value": 3 })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_return_false_when_applied_to_number_is_less_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gte(3)
                "#
            )
            .apply_to(&json!({ "value": 2 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_return_true_when_applied_to_string_is_greater_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gte("b")
                "#
            )
            .apply_to(&json!({ "value": "c" })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_return_true_when_applied_to_string_equals_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gte("a")
                "#
            )
            .apply_to(&json!({ "value": "a" })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_return_false_when_applied_to_string_is_less_than_argument() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gte("b")
                "#
            )
            .apply_to(&json!({ "value": "a" })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_compare_null_values() {
        assert_eq!(
            selection!(
                r#"
                    result: value->gte(null)
                "#
            )
            .apply_to(&json!({ "value": null })),
            (
                Some(json!({
                    "result": true,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_compare_boolean_values() {
        // true >= false should be true
        assert_eq!(
            selection!(
                r#"
                    result: value->gte(false)
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

        // false >= true should be false
        assert_eq!(
            selection!(
                r#"
                    result: value->gte(true)
                "#
            )
            .apply_to(&json!({ "value": false })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_return_false_for_arrays_and_objects() {
        // Arrays should return false
        assert_eq!(
            selection!(
                r#"
                    result: value->gte([1,2])
                "#
            )
            .apply_to(&json!({ "value": [1,2,3] })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );

        // Objects should return false
        assert_eq!(
            selection!(
                r#"
                    result: value->gte({"a": 1})
                "#
            )
            .apply_to(&json!({ "value": {"a": 1, "b": 2} })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn gte_should_return_false_for_mixed_types() {
        // Mixed types should return false
        assert_eq!(
            selection!(
                r#"
                    result: value->gte("string")
                "#
            )
            .apply_to(&json!({ "value": 42 })),
            (
                Some(json!({
                    "result": false,
                })),
                vec![],
            ),
        );
    }
}
