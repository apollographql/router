use apollo_compiler::collections::IndexSet;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;

use crate::sources::connect::json_selection::helpers::json_type_name;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::location::merge_ranges;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::PathList;
use crate::sources::connect::json_selection::VarsWithPathsMap;

pub(super) fn echo_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(method_args) = method_args {
        if let Some(arg) = method_args.args.first() {
            return arg
                .apply_to_path(data, vars, input_path, errors)
                .and_then(|value| tail.apply_to_path(&value, vars, input_path, errors));
        }
    }
    errors.insert(ApplyToError::new(
        format!("Method ->{} requires one argument", method_name.node()),
        input_path.to_vec(),
        method_name.range(),
    ));
    None
}

pub(super) fn map_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(args) = method_args {
        if let Some(first_arg) = args.args.first() {
            if let JSON::Array(array) = data {
                let mut output = Vec::with_capacity(array.len());

                for (i, element) in array.iter().enumerate() {
                    let input_path = input_path.append(JSON::Number(i.into()));
                    if let Some(applied) =
                        first_arg.apply_to_path(element, vars, &input_path, errors)
                    {
                        if let Some(value) = tail.apply_to_path(&applied, vars, &input_path, errors)
                        {
                            output.push(value);
                            continue;
                        }
                    }
                    output.push(JSON::Null);
                }

                Some(JSON::Array(output))
            } else {
                first_arg.apply_to_path(data, vars, input_path, errors)
            }
        } else {
            errors.insert(ApplyToError::new(
                format!("Method ->{} requires one argument", method_name.node()),
                input_path.to_vec(),
                method_name.range(),
            ));
            None
        }
    } else {
        errors.insert(ApplyToError::new(
            format!("Method ->{} requires one argument", method_name.node()),
            input_path.to_vec(),
            method_name.range(),
        ));
        None
    }
}

pub(super) fn match_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    // Takes any number of pairs [key, value], and returns value for the first
    // key that equals the data. If none of the pairs match, returns None.
    // Typically, the final pair will use @ as its key to ensure some default
    // value is returned.
    if let Some(method_args) = method_args {
        for pair in &method_args.args {
            if let LitExpr::Array(pair) = pair.node() {
                if pair.len() == 2 {
                    if let Some(candidate) = pair[0].apply_to_path(data, vars, input_path, errors) {
                        if candidate == *data {
                            return pair[1]
                                .apply_to_path(data, vars, input_path, errors)
                                .and_then(|value| {
                                    tail.apply_to_path(&value, vars, input_path, errors)
                                });
                        }
                    };
                }
            }
        }
    }
    errors.insert(ApplyToError::new(
        format!(
            "Method ->{} did not match any [candidate, value] pair",
            method_name.node(),
        ),
        input_path.to_vec(),
        merge_ranges(
            method_name.range(),
            method_args.and_then(|args| args.range()),
        ),
    ));
    None
}

pub(super) fn first_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if method_args.is_some() {
        errors.insert(ApplyToError::new(
            format!(
                "Method ->{} does not take any arguments",
                method_name.node()
            ),
            input_path.to_vec(),
            method_name.range(),
        ));
        return None;
    }

    match data {
        JSON::Array(array) => array
            .first()
            .and_then(|first| tail.apply_to_path(first, vars, input_path, errors)),
        JSON::String(s) => s.as_str().chars().next().and_then(|first| {
            tail.apply_to_path(
                &JSON::String(first.to_string().into()),
                vars,
                input_path,
                errors,
            )
        }),
        _ => tail.apply_to_path(data, vars, input_path, errors),
    }
}

pub(super) fn last_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if method_args.is_some() {
        errors.insert(ApplyToError::new(
            format!(
                "Method ->{} does not take any arguments",
                method_name.node()
            ),
            input_path.to_vec(),
            method_name.range(),
        ));
        return None;
    }

    match data {
        JSON::Array(array) => array
            .last()
            .and_then(|last| tail.apply_to_path(last, vars, input_path, errors)),
        JSON::String(s) => s.as_str().chars().last().and_then(|last| {
            tail.apply_to_path(
                &JSON::String(last.to_string().into()),
                vars,
                input_path,
                errors,
            )
        }),
        _ => tail.apply_to_path(data, vars, input_path, errors),
    }
}

pub(super) fn slice_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    let length = if let JSON::Array(array) = data {
        array.len() as i64
    } else if let JSON::String(s) = data {
        s.as_str().len() as i64
    } else {
        errors.insert(ApplyToError::new(
            format!(
                "Method ->{} requires an array or string input",
                method_name.node()
            ),
            input_path.to_vec(),
            method_name.range(),
        ));
        return None;
    };

    if let Some(method_args) = method_args {
        let start = method_args
            .args
            .first()
            .and_then(|arg| arg.apply_to_path(data, vars, input_path, errors))
            .and_then(|n| n.as_i64())
            .unwrap_or(0)
            .max(0)
            .min(length) as usize;
        let end = method_args
            .args
            .get(1)
            .and_then(|arg| arg.apply_to_path(data, vars, input_path, errors))
            .and_then(|n| n.as_i64())
            .unwrap_or(length)
            .max(0)
            .min(length) as usize;

        let array = match data {
            JSON::Array(array) => {
                if end - start > 0 {
                    JSON::Array(
                        array
                            .iter()
                            .skip(start)
                            .take(end - start)
                            .cloned()
                            .collect(),
                    )
                } else {
                    JSON::Array(vec![])
                }
            }
            JSON::String(s) => {
                if end - start > 0 {
                    JSON::String(s.as_str()[start..end].to_string().into())
                } else {
                    JSON::String("".to_string().into())
                }
            }
            _ => unreachable!(),
        };

        tail.apply_to_path(&array, vars, input_path, errors)
    } else {
        Some(data.clone())
    }
}

pub(super) fn size_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if method_args.is_some() {
        errors.insert(ApplyToError::new(
            format!(
                "Method ->{} does not take any arguments",
                method_name.node()
            ),
            input_path.to_vec(),
            method_name.range(),
        ));
        return None;
    }

    match data {
        JSON::Array(array) => {
            let size = array.len() as i64;
            tail.apply_to_path(&JSON::Number(size.into()), vars, input_path, errors)
        }
        JSON::String(s) => {
            let size = s.as_str().len() as i64;
            tail.apply_to_path(&JSON::Number(size.into()), vars, input_path, errors)
        }
        // Though we can't ask for ->first or ->last or ->at(n) on an object, we
        // can safely return how many properties the object has for ->size.
        JSON::Object(map) => {
            let size = map.len() as i64;
            tail.apply_to_path(&JSON::Number(size.into()), vars, input_path, errors)
        }
        _ => {
            errors.insert(ApplyToError::new(
                format!(
                    "Method ->{} requires an array, string, or object input, not {}",
                    method_name.node(),
                    json_type_name(data),
                ),
                input_path.to_vec(),
                method_name.range(),
            ));
            None
        }
    }
}

// Returns a list of [{ key, value }, ...] objects for each key-value pair in
// the object. Returning a list of [[ key, value ], ...] pairs might also seem
// like an option, but GraphQL doesn't handle heterogeneous lists (or tuples) as
// well as it handles objects with named properties like { key, value }.
pub(super) fn entries_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if method_args.is_some() {
        errors.insert(ApplyToError::new(
            format!(
                "Method ->{} does not take any arguments",
                method_name.node()
            ),
            input_path.to_vec(),
            method_name.range(),
        ));
        return None;
    }

    match data {
        JSON::Object(map) => {
            let entries = map
                .iter()
                .map(|(key, value)| {
                    let mut key_value_pair = JSONMap::new();
                    key_value_pair.insert(ByteString::from("key"), JSON::String(key.clone()));
                    key_value_pair.insert(ByteString::from("value"), value.clone());
                    JSON::Object(key_value_pair)
                })
                .collect();
            tail.apply_to_path(&JSON::Array(entries), vars, input_path, errors)
        }
        _ => {
            errors.insert(ApplyToError::new(
                format!(
                    "Method ->{} requires an object input, not {}",
                    method_name.node(),
                    json_type_name(data),
                ),
                input_path.to_vec(),
                method_name.range(),
            ));
            None
        }
    }
}
