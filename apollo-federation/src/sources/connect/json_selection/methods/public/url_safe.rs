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

impl_arrow_method!(UrlSafeMethod, url_safe_method, url_safe_shape);
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
fn url_safe_method(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    _input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let JSON::String(safe_string) = data {
        let s = match safe_string {
            SafeString::Safe(byte_string) => byte_string,
            SafeString::Unsafe(byte_string) => byte_string,
        };
        let s = Some(JSON::String(SafeString::Safe(s.to_owned())));
        return (s, vec![]);
    }

    (Some(data.clone()), vec![])
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn url_safe_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        return first_arg.compute_output_shape(
            input_shape,
            dollar_shape,
            named_var_shapes,
            source_id,
        );
    }
    Shape::error(
        format!("Method ->{} requires one argument", method_name.as_ref()),
        method_name.shape_location(source_id),
    )
}
