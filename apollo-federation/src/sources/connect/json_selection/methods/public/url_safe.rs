use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use serde_json_bytes::json;
use shape::Shape;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;

pub(crate) static URL_SAFE: &str = "[[:URL_SAFE:]]";

impl_arrow_method!(UrlSafeMethod, url_safe_method, url_safe_shape);

fn url_safe_method(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    _input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let data = match data {
        JSON::String(byte_string) => json!({
            "value": byte_string,
            URL_SAFE: true
        }),
        _ => data.clone(),
    };

    (Some(data), vec![])
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn url_safe_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    Shape::string(method_name.shape_location(source_id))
}
