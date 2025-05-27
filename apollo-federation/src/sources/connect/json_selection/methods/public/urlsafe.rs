use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;
use crate::sources::connect::json_selection::safe_json::SafeString;
use crate::sources::connect::json_selection::safe_json::Value as JSON;

impl_arrow_method!(UrlSafeMethod, urlsafe_method, urlsafe_method_shape);
/// Takes an array of scalar values and joins them into a single string using a
/// separator, skipping null values.
///
/// This method is specifically useful when dealing with lists of entity
/// references in Federation, which can contain null. It's rare that you'll want
/// to send a `null` to an upstream service when fetching a batch of entities,
/// so this is a useful and convenient method.
///
/// $->echo(["hello", null, "world"])->joinNotNull(", ") would result in "hello, world"
fn urlsafe_method(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    _input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let JSON::String(safe_string) = data {
        let safe_string = match safe_string {
            SafeString::Trusted(byte_string) => byte_string,
            SafeString::AutoEncoded(byte_string) => byte_string,
        };
        let json_str = Some(JSON::String(SafeString::Trusted(safe_string.to_owned())));
        return (json_str, vec![]);
    }

    (Some(data.clone()), vec![])
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn urlsafe_method_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        first_arg.compute_output_shape(input_shape, dollar_shape, named_var_shapes, source_id)
    } else {
        Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(source_id),
        )
    }
}

#[cfg(test)]
mod tests {
    mod url_safe {
        use serde_json_bytes::json;

        use crate::selection;

        #[test]
        fn should_not_encode_when_safe_string() {
            let applied = selection!("$->urlSafe").apply_to(&json!("cra/zy,safe")).0;
            assert_eq!(applied, Some(json!("cra/zy,safe")));
        }

        #[test]
        fn should_enconde_when_non_safe_join() {
            let applied = selection!("$->joinNotNull(',')")
                .apply_to(&json!(["a", "b", "c"]))
                .0;
            assert_eq!(applied, Some(json!("a%2Cb%2Cc")));
        }

        #[test]
        fn should_not_encode_when_safe_join_on_unsafe_char() {
            let applied = selection!("$->map(@->urlSafe)->joinNotNull(',')")
                .apply_to(&json!(["23/4", "13/2"]))
                .0;
            assert_eq!(applied, Some(json!("23/4%2C13/2")));
        }

        #[test]
        fn should_not_encode_when_safe_join_on_strings() {
            let applied = selection!("$->joinNotNull(','->urlSafe)")
                .apply_to(&json!(["a", "b", "c"]))
                .0;
            assert_eq!(applied, Some(json!("a,b,c")));
        }
    }
}
