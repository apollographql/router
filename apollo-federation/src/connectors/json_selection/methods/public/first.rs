use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(FirstMethod, first_method, first_shape);
/// The "first" method is a utility function that can be run against an array to grab the 0th item from it
/// or a string to get the first character.
/// The simplest possible example:
///
/// $->echo([1,2,3])->first     results in 1
/// $->echo("hello")->first     results in "h"
fn first_method(
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
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    }

    match data {
        JSON::Array(array) => (array.first().cloned(), Vec::new()),
        JSON::String(s) => s.as_str().chars().next().map_or_else(
            || (None, Vec::new()),
            |first| (Some(JSON::String(first.to_string().into())), Vec::new()),
        ),
        _ => (
            Some(data.clone()),
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an array or string input",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        ),
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn first_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    let location = method_name.shape_location(context.source_id());
    if method_args.is_some() {
        return Shape::error(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            location,
        );
    }

    // Location is not solely based on the method, but also the type the method is being applied to
    let locations = input_shape.locations().cloned().chain(location);

    match input_shape.case() {
        // Match runtime: the first char (not byte), or no value for "".
        ShapeCase::String(Some(value)) => value.chars().next().map_or_else(Shape::none, |first| {
            Shape::string_value(first.encode_utf8(&mut [0; 4]), locations)
        }),
        ShapeCase::String(None) => Shape::string(locations),
        ShapeCase::Array { prefix, tail } => {
            if let Some(first) = prefix.first() {
                first.clone()
            } else if tail.is_none() {
                Shape::none()
            } else {
                Shape::one([tail.clone(), Shape::none()], locations)
            }
        }
        ShapeCase::Name(_, _) => input_shape.item(0, locations),
        ShapeCase::Unknown => Shape::unknown(locations),
        // When there is no obvious first element, ->first gives us the input
        // value itself, which has input_shape.
        _ => Shape::error_with_partial(
            format!(
                "Method ->{} requires an array or string input",
                method_name.as_ref()
            ),
            input_shape.clone(),
            locations,
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn first_should_get_first_element_from_array() {
        assert_eq!(
            selection!("$->first").apply_to(&json!([1, 2, 3])),
            (Some(json!(1)), vec![]),
        );
    }

    #[test]
    fn first_should_get_none_when_no_items_exist() {
        assert_eq!(selection!("$->first").apply_to(&json!([])), (None, vec![]),);
    }

    #[test]
    fn first_should_get_first_char_from_string() {
        assert_eq!(
            selection!("$->first").apply_to(&json!("hello")),
            (Some(json!("h")), vec![]),
        );
    }

    // The string-input shape must agree with runtime, which takes the first
    // char (not byte) and produces no value for an empty string.
    mod string_shape {
        use serde_json_bytes::json;
        use shape::Shape;
        use shape::location::SourceId;

        use crate::connectors::ConnectSpec;
        use crate::connectors::json_selection::ShapeContext;
        use crate::selection;

        fn first_shape_of(input: &serde_json_bytes::Value) -> Shape {
            let context = ShapeContext::new(SourceId::Other("first shape test".into()))
                .with_spec(ConnectSpec::V0_4);
            selection!("$.text->first", ConnectSpec::V0_4)
                .compute_output_shape(&context, Shape::from_json_bytes(input))
        }

        #[test]
        fn string_first_shape_accepts_empty_string() {
            let input = json!({ "text": "" });
            let selection = selection!("$.text->first", ConnectSpec::V0_4);
            assert_eq!(selection.apply_to(&input), (None, vec![]));
            let shape = first_shape_of(&input);
            assert!(shape.is_none(), "shape={}", shape.pretty_print());
        }

        #[test]
        fn string_first_shape_accepts_multibyte_string() {
            let input = json!({ "text": "é🙂" });
            let selection = selection!("$.text->first", ConnectSpec::V0_4);
            assert_eq!(selection.apply_to(&input), (Some(json!("é")), vec![]));
            let shape = first_shape_of(&input);
            assert!(
                shape.accepts_json_bytes(&json!("é")),
                "shape={}",
                shape.pretty_print()
            );
            assert!(
                !shape.accepts_json_bytes(&json!("é🙂")),
                "shape={}",
                shape.pretty_print()
            );
        }

        #[test]
        fn literal_string_first_shape_does_not_panic() {
            // Literal strings get known-value shapes from the selection alone,
            // which is what connector validation and expansion compute.
            for (source, expected) in [("$('')->first", None), ("$('🙂x')->first", Some("🙂"))]
            {
                let shape = selection!(source, ConnectSpec::V0_4).shape();
                match expected {
                    None => assert!(shape.is_none(), "{source}: {}", shape.pretty_print()),
                    Some(first) => assert!(
                        shape.accepts_json_bytes(&json!(first)),
                        "{source}: {}",
                        shape.pretty_print()
                    ),
                }
            }
        }
    }
}
