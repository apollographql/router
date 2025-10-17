use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::json_type_name;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(TypeOfMethod, typeof_method, typeof_shape);
/// Given a JSON structure, returns a string representing the "type"
///
/// Some examples:
/// $->echo(true)       would result in "boolean"
/// $->echo([1, 2, 3])       would result in "array"
/// $->echo("hello")       would result in "string"
/// $->echo(5)       would result in "number"
fn typeof_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if method_args.is_some() {
        (
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
        )
    } else {
        let typeof_string = JSON::String(json_type_name(data).to_string().into());
        (Some(typeof_string), Vec::new())
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn typeof_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    // TODO Compute this union type once and clone it here.
    let locations = method_name.shape_location(context.source_id());
    Shape::one(
        [
            Shape::string_value("null", locations.clone()),
            Shape::string_value("boolean", locations.clone()),
            Shape::string_value("number", locations.clone()),
            Shape::string_value("string", locations.clone()),
            Shape::string_value("array", locations.clone()),
            Shape::string_value("object", locations.clone()),
        ],
        locations,
    )
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    #[test]
    fn typeof_should_return_appropriate_when_applied_to_data() {
        fn check(selection: &str, data: &JSON, expected_type: &str) {
            assert_eq!(
                selection!(selection).apply_to(data),
                (Some(json!(expected_type)), vec![]),
            );
        }

        check("$->typeof", &json!(null), "null");
        check("$->typeof", &json!(true), "boolean");
        check("@->typeof", &json!(false), "boolean");
        check("$->typeof", &json!(123), "number");
        check("$->typeof", &json!(123.45), "number");
        check("$->typeof", &json!("hello"), "string");
        check("$->typeof", &json!([1, 2, 3]), "array");
        check("$->typeof", &json!({ "key": "value" }), "object");
    }
}
