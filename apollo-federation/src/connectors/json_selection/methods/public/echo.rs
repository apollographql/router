use serde_json_bytes::Value as JSON;
use shape::Shape;

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

impl_arrow_method!(EchoMethod, echo_method, echo_shape);
/// Echo simply returns back whichever value is provided in it's arg.
/// The simplest possible case is $.echo("hello world") which would result in "hello world"
///
/// However, it will also reflect back any type passed into it allowing you to act on those:
///
/// $->echo([1,2,3])->first         would result in "1"
///
/// It's also worth noting that you can use $ to refer to to the selection and pass that into echo and you can also use @ to refer to the value that echo is being run on.
///
/// For example, assuming my selection is { firstName: "John", children: ["Jack"] }...
///
/// $->echo($.firstName)            would result in "John"
/// $.children->echo(@->first)      would result in "Jack"
fn echo_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args
        && let Some(arg) = args.first()
    {
        return arg.apply_to_path(data, vars, input_path, spec);
    }
    (
        None,
        vec![ApplyToError::new(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            input_path.to_vec(),
            method_name.range(),
            spec,
        )],
    )
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn echo_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        return first_arg.compute_output_shape(context, input_shape, dollar_shape);
    }
    Shape::error(
        format!("Method ->{} requires one argument", method_name.as_ref()),
        method_name.shape_location(context.source_id()),
    )
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::connectors::ConnectSpec;
    use crate::connectors::json_selection::ApplyToError;
    use crate::selection;

    #[test]
    fn echo_should_output_value_when_applied_to_null() {
        assert_eq!(
            selection!("$->echo('oyez')").apply_to(&json!(null)),
            (Some(json!("oyez")), vec![]),
        );
    }

    #[test]
    fn echo_should_output_value_when_applied_to_array() {
        assert_eq!(
            selection!("$->echo('oyez')").apply_to(&json!([1, 2, 3])),
            (Some(json!("oyez")), vec![]),
        );
    }

    #[test]
    fn echo_should_allow_selection_from_array_value() {
        assert_eq!(
            selection!("$->echo([1, 2, 3]) { id: $ }").apply_to(&json!(null)),
            (Some(json!([{ "id": 1 }, { "id": 2 }, { "id": 3 }])), vec![]),
        );
    }

    #[test]
    fn echo_should_allow_arrow_methods_off_returned_value() {
        assert_eq!(
            selection!("$->echo([1, 2, 3])->last { id: $ }").apply_to(&json!(null)),
            (Some(json!({ "id": 3 })), vec![]),
        );
    }

    #[test]
    fn echo_should_allow_at_sign_to_input_value_from_selection_function() {
        assert_eq!(
            selection!("$.nested.value->echo(['before', @, 'after'])").apply_to(&json!({
                "nested": {
                    "value": 123,
                },
            })),
            (Some(json!(["before", 123, "after"])), vec![]),
        );
    }

    #[test]
    fn echo_should_allow_dollar_sign_to_input_applied_to_value() {
        assert_eq!(
            selection!("$.nested.value->echo(['before', $, 'after'])").apply_to(&json!({
                "nested": {
                    "value": 123,
                },
            })),
            (
                Some(json!(["before", {
            "nested": {
                "value": 123,
            },
        }, "after"])),
                vec![]
            ),
        );
    }

    #[test]
    fn echo_should_allow_selection_functions_result_passed_as_value() {
        assert_eq!(
            selection!("results->echo(@->first)").apply_to(&json!({
                "results": [
                    [1, 2, 3],
                    "ignored",
                ],
            })),
            (Some(json!([1, 2, 3])), vec![]),
        );
    }

    #[test]
    fn echo_should_allow_arrow_functions_on_result_of_echo() {
        assert_eq!(
            selection!("results->echo(@->first)->last").apply_to(&json!({
                "results": [
                    [1, 2, 3],
                    "ignored",
                ],
            })),
            (Some(json!(3)), vec![]),
        );
    }

    #[test]
    fn echo_should_not_error_with_trailing_commas() {
        let nested_value_data = json!({
            "nested": {
                "value": 123,
            },
        });

        let expected = (Some(json!({ "wrapped": 123 })), vec![]);

        let check = |selection: &str| {
            assert_eq!(selection!(selection).apply_to(&nested_value_data), expected,);
        };

        check("nested.value->echo({ wrapped: @ })");
        check("nested.value->echo({ wrapped: @,})");
        check("nested.value->echo({ wrapped: @,},)");
        check("nested.value->echo({ wrapped: @},)");
        check("nested.value->echo({ wrapped: @ , } , )");
    }

    #[test]
    fn echo_should_flatted_object_list_using_at_sign() {
        // Turn a list of { name, hobby } objects into a single { names: [...],
        // hobbies: [...] } object.
        assert_eq!(
            selection!(
                r#"
            people->echo({
                names: @.name,
                hobbies: @.hobby,
            })
            "#
            )
            .apply_to(&json!({
                "people": [
                    { "name": "Alice", "hobby": "reading" },
                    { "name": "Bob", "hobby": "fishing" },
                    { "hobby": "painting", "name": "Charlie" },
                ],
            })),
            (
                Some(json!({
                    "names": ["Alice", "Bob", "Charlie"],
                    "hobbies": ["reading", "fishing", "painting"],
                })),
                vec![],
            ),
        );
    }

    #[rstest::rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn echo_should_return_none_when_argument_evaluates_to_none(#[case] spec: ConnectSpec) {
        assert_eq!(
            selection!("$->echo($.missing)", spec).apply_to(&json!({})),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .missing not found in object",
                    "path": ["missing"],
                    "range": [10, 17],
                    "spec": spec.to_string(),
                }))]
            ),
        );
    }
}
