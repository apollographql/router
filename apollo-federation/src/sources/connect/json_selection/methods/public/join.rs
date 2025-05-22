use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use serde_json_bytes::ByteString;
// use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::location::SourceId;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::VarsWithPathsMap;
use crate::sources::connect::json_selection::immutable::InputPath;
// use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;
use crate::sources::connect::json_selection::safe_json::SafeString;
use crate::sources::connect::json_selection::safe_json::Value as JSON;
// use crate::sources::connect::json_selection::safe_json::string::Join;

impl_arrow_method!(JoinMethod, join_method, join_shape);
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
fn join_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let mut warnings = vec![];

    let Some((resolved_separator, _)) = method_args
        .and_then(|args| args.args.first())
        .take_if(|lit| matches!(lit.clone().take(), LitExpr::String(_)))
        .map(|arg| arg.apply_to_path(data, vars, input_path))
        else {
            warnings.push(ApplyToError::new(
            format!(
                "Method ->{} requires a string argument",
                method_name.as_ref()
            ),
            input_path.to_vec(),
                method_name.range(),
            ));
            return (None, warnings);
        };

    let separator = if let Some(JSON::String(safe_string)) = resolved_separator {
        safe_string
    } else {
        SafeString::Safe("".into())
    };

    fn to_string(value: &JSON) -> (SafeString, Option<String>) {
        match value {
            JSON::Bool(b) => (
                SafeString::Safe(b.then_some("true").unwrap_or("false").into()),
                None,
            ),
            JSON::Number(number) => (SafeString::Safe(number.to_string().into()), None),
            JSON::String(string) => (string.clone(), None),
            JSON::Null => (SafeString::Safe("".into()), None),
            JSON::Array(_) | JSON::Object(_) => (
                SafeString::Safe("".into()),
                Some("Method ->join requires an array of scalars values as input".to_string()),
            ),
        }
    }

    let joined = match data {
        JSON::Array(values) => {
            let mut iter = values.into_iter().map(to_string).map(|(s, w)| {
                if let Some(w) = w {
                    warnings.push(ApplyToError::new(
                        w,
                        input_path.to_vec(),
                        method_name.range(),
                    ));
                }
                s
            });
            let first = match iter.next() {
                Some(s) => s,
                None => SafeString::Unsafe("".into()),
            };

            let mut result = first;
            for s in iter {
                result = result + &separator;
                result = result + &s;
            }
            result
        }
        // Single values are emitted as strings with no separator
        _ => {
            let (value, err) = to_string(data);
            if let Some(err) = err {
                warnings.push(ApplyToError::new(
                    err.to_string(),
                    input_path.to_vec(),
                    method_name.range(),
                ));
            }
            value
        }
    };

    (Some(JSON::String(joined)), warnings)
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn join_shape(
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
