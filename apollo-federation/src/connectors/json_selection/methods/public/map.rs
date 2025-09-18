use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::apply_to::ApplyToResultMethods;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(MapMethod, map_method, map_shape);
/// "Map" is an array transform method very similar to the Array.map function you'd find in other languages.
/// You can use it to transform an array of values to a new array of values.
///
/// For example, given a selection of [1, 2, 3]:
///
/// $->map(@->add(10))      result is [11, 12, 13]
///
/// We are taking each value passed into map via @ and running the "add" function against that value
///
/// I could also "hard code" the values being passed in above using echo:
///
/// $->echo([1,2,3])->map(@->add(10))
fn map_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(args) = method_args else {
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
    let Some(first_arg) = args.args.first() else {
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
        let mut output = Vec::with_capacity(array.len());
        let mut errors = Vec::new();

        for (i, element) in array.iter().enumerate() {
            let input_path = input_path.append(JSON::Number(i.into()));
            let (applied_opt, arg_errors) =
                first_arg.apply_to_path(element, vars, &input_path, spec);
            errors.extend(arg_errors);
            output.insert(i, applied_opt.unwrap_or(JSON::Null));
        }

        (Some(JSON::Array(output)), errors)
    } else {
        // Return a singleton array wrapping the value of applying the
        // ->map method the non-array input data.
        first_arg
            .apply_to_path(data, vars, input_path, spec)
            .and_then_collecting_errors(|value| {
                (Some(JSON::Array(vec![value.clone()])), Vec::new())
            })
    }
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn map_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let Some(first_arg) = method_args.and_then(|args| args.args.first()) else {
        return Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(context.source_id()),
        );
    };
    match input_shape.case() {
        ShapeCase::Array { prefix, tail } => {
            let new_prefix = prefix
                .iter()
                .map(|shape| {
                    first_arg.compute_output_shape(context, shape.clone(), dollar_shape.clone())
                })
                .collect::<Vec<_>>();
            let new_tail = first_arg.compute_output_shape(context, tail.clone(), dollar_shape);
            Shape::array(new_prefix, new_tail, input_shape.locations)
        }
        _ => Shape::list(
            first_arg.compute_output_shape(context, input_shape.any_item([]), dollar_shape),
            input_shape.locations,
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn map_should_transform_when_applied_to_array() {
        assert_eq!(
            selection!("messages->map(@.role)").apply_to(&json!({
                "messages": [
                    { "role": "admin" },
                    { "role": "user" },
                    { "role": "guest" },
                ],
            })),
            (Some(json!(["admin", "user", "guest"])), vec![]),
        );
    }

    #[test]
    fn map_should_transform_when_applied_to_array_with_additional_transform() {
        assert_eq!(
            selection!("$->map(@->add(10))").apply_to(&json!([1, 2, 3])),
            (Some(json!(vec![11, 12, 13])), vec![]),
        );

        assert_eq!(
            selection!("values->map(@->typeof)").apply_to(&json!({
                "values": [1, 2.5, "hello", true, null, [], {}],
            })),
            (
                Some(json!([
                    "number", "number", "string", "boolean", "null", "array", "object"
                ])),
                vec![],
            ),
        );

        assert_eq!(
            selection!("singleValue->map(@->mul(10))").apply_to(&json!({
                "singleValue": 123,
            })),
            (Some(json!([1230])), vec![]),
        );
    }

    #[test]
    fn map_should_transform_when_called_against_selected_array() {
        assert_eq!(
            selection!("$->echo([1,2,3])->map(@->add(10))").apply_to(&json!(null)),
            (Some(json!(vec![11, 12, 13])), vec![]),
        );
    }

    /*
    #[test]
    fn test_map_method() {
        //  TODO: re-test once method type checking is re-enabled
        // {
        //     let single_value_data = json!({
        //         "singleValue": 123,
        //     });
        //     let json_selection = selection!("singleValue->map(@->jsonStringify)");
        //     assert_eq!(
        //         json_selection.apply_to(&single_value_data),
        //         (Some(json!(["123"])), vec![]),
        //     );
        //     let output_shape = json_selection.compute_output_shape(
        //         Shape::from_json_bytes(&single_value_data),
        //         &IndexMap::default(),
        //         &SourceId::new("test"),
        //     );
        //     assert_eq!(output_shape.pretty_print(), "List<String>");
        // }
    }*/
}
