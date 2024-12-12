// The future.rs module contains methods that are not yet exposed for use in
// JSONSelection strings in connector schemas, but have proposed implementations
// and tests. After careful review, they may one day move to public.rs.

use serde_json::Number;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;

use crate::sources::connect::json_selection::apply_to::ApplyToResultMethods;
use crate::sources::connect::json_selection::helpers::json_type_name;
use crate::sources::connect::json_selection::helpers::vec_push;
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

pub(super) fn typeof_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
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
            )],
        )
    } else {
        let typeof_string = JSON::String(json_type_name(data).to_string().into());
        tail.apply_to_path(&typeof_string, vars, input_path)
    }
}

pub(super) fn eq_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        if args.len() == 1 {
            let (value_opt, arg_errors) = args[0].apply_to_path(data, vars, input_path);
            let matches = if let Some(value) = value_opt {
                data == &value
            } else {
                false
            };
            return tail
                .apply_to_path(&JSON::Bool(matches), vars, input_path)
                .prepend_errors(arg_errors);
        }
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

// Like ->match, but expects the first element of each pair
// to evaluate to a boolean, returning the second element of
// the first pair whose first element is true. This makes
// providing a final catch-all case easy, since the last
// pair can be [true, <default>].
pub(super) fn match_if_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let mut errors = Vec::new();

    if let Some(MethodArgs { args, .. }) = method_args {
        for pair in args {
            if let LitExpr::Array(pair) = pair.as_ref() {
                if pair.len() == 2 {
                    let (condition_opt, condition_errors) =
                        pair[0].apply_to_path(data, vars, input_path);
                    errors.extend(condition_errors);

                    if let Some(JSON::Bool(true)) = condition_opt {
                        return pair[1]
                            .apply_to_path(data, vars, input_path)
                            .and_then_collecting_errors(|value| {
                                tail.apply_to_path(value, vars, input_path)
                            })
                            .prepend_errors(errors);
                    };
                }
            }
        }
    }
    (
        None,
        vec_push(
            errors,
            ApplyToError::new(
                format!(
                    "Method ->{} did not match any [condition, value] pair",
                    method_name.as_ref(),
                ),
                input_path.to_vec(),
                merge_ranges(
                    method_name.range(),
                    method_args.and_then(|args| args.range()),
                ),
            ),
        ),
    )
}

pub(super) fn map_values_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        if let JSON::Object(map) = data {
            let mut new_map = JSONMap::new();
            let mut errors = Vec::new();
            for (key, value) in map {
                let new_key = key.clone();
                let (new_value_opt, value_errors) =
                    first_arg.apply_to_path(value, vars, input_path);
                errors.extend(value_errors);
                if let Some(new_value) = new_value_opt {
                    new_map.insert(new_key, new_value);
                }
            }
            tail.apply_to_path(&JSON::Object(new_map), vars, input_path)
                .prepend_errors(errors)
        } else {
            (
                None,
                vec![ApplyToError::new(
                    format!(
                        "Method ->{} requires an object input, not {}",
                        method_name.as_ref(),
                        json_type_name(data),
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                )],
            )
        }
    } else {
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
}

pub(super) fn arithmetic_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    op: impl Fn(&Number, &Number) -> Option<Number>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        if let JSON::Number(result) = data {
            let mut result = result.clone();
            let mut errors = Vec::new();
            for arg in args {
                let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path);
                errors.extend(arg_errors);
                if let Some(JSON::Number(n)) = value_opt {
                    if let Some(new_result) = op(&result, &n) {
                        result = new_result;
                    } else {
                        return (
                            None,
                            vec_push(
                                errors,
                                ApplyToError::new(
                                    format!(
                                        "Method ->{} failed on argument {}",
                                        method_name.as_ref(),
                                        n
                                    ),
                                    input_path.to_vec(),
                                    arg.range(),
                                ),
                            ),
                        );
                    }
                } else {
                    return (
                        None,
                        vec_push(
                            errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{} requires numeric arguments",
                                    method_name.as_ref()
                                ),
                                input_path.to_vec(),
                                arg.range(),
                            ),
                        ),
                    );
                }
            }
            (Some(JSON::Number(result)), errors)
        } else {
            (
                None,
                vec![ApplyToError::new(
                    format!(
                        "Method ->{} requires numeric arguments",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                )],
            )
        }
    } else {
        (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires at least one argument",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        )
    }
}

macro_rules! infix_math_op {
    ($name:ident, $op:tt) => {
        fn $name(a: &Number, b: &Number) -> Option<Number> {
            if a.is_f64() || b.is_f64() {
                Number::from_f64(a.as_f64().unwrap() $op b.as_f64().unwrap())
            } else if let (Some(a_i64), Some(b_i64)) = (a.as_i64(), b.as_i64()) {
                Some(Number::from(a_i64 $op b_i64))
            } else {
                None
            }
        }
    };
}
infix_math_op!(add_op, +);
infix_math_op!(sub_op, -);
infix_math_op!(mul_op, *);
infix_math_op!(div_op, /);
infix_math_op!(rem_op, %);

macro_rules! infix_math_method {
    ($name:ident, $op:ident) => {
        pub(super) fn $name(
            method_name: &WithRange<String>,
            method_args: Option<&MethodArgs>,
            data: &JSON,
            vars: &VarsWithPathsMap,
            input_path: &InputPath<JSON>,
            tail: &WithRange<PathList>,
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            arithmetic_method(method_name, method_args, $op, data, vars, input_path)
                .and_then_collecting_errors(|result| tail.apply_to_path(&result, vars, input_path))
        }
    };
}
infix_math_method!(add_method, add_op);
infix_math_method!(sub_method, sub_op);
infix_math_method!(mul_method, mul_op);
infix_math_method!(div_method, div_op);
infix_math_method!(mod_method, rem_op);

pub(super) fn has_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        match args.first() {
            Some(arg) => match arg.apply_to_path(data, vars, input_path) {
                (Some(ref json_index @ JSON::Number(ref n)), arg_errors) => {
                    match (data, n.as_i64()) {
                        (JSON::Array(array), Some(index)) => {
                            let ilen = array.len() as i64;
                            // Negative indices count from the end of the array
                            let index = if index < 0 { ilen + index } else { index };
                            tail.apply_to_path(
                                &JSON::Bool(index >= 0 && index < ilen),
                                vars,
                                &input_path.append(json_index.clone()),
                            )
                            .prepend_errors(arg_errors)
                        }

                        (json_key @ JSON::String(s), Some(index)) => {
                            let ilen = s.as_str().len() as i64;
                            // Negative indices count from the end of the array
                            let index = if index < 0 { ilen + index } else { index };
                            tail.apply_to_path(
                                &JSON::Bool(index >= 0 && index < ilen),
                                vars,
                                &input_path.append(json_key.clone()),
                            )
                            .prepend_errors(arg_errors)
                        }

                        _ => tail
                            .apply_to_path(
                                &JSON::Bool(false),
                                vars,
                                &input_path.append(json_index.clone()),
                            )
                            .prepend_errors(arg_errors),
                    }
                }

                (Some(ref json_key @ JSON::String(ref s)), arg_errors) => match data {
                    JSON::Object(map) => tail
                        .apply_to_path(
                            &JSON::Bool(map.contains_key(s.as_str())),
                            vars,
                            &input_path.append(json_key.clone()),
                        )
                        .prepend_errors(arg_errors),

                    _ => tail
                        .apply_to_path(
                            &JSON::Bool(false),
                            vars,
                            &input_path.append(json_key.clone()),
                        )
                        .prepend_errors(arg_errors),
                },

                (Some(value), arg_errors) => tail
                    .apply_to_path(&JSON::Bool(false), vars, &input_path.append(value.clone()))
                    .prepend_errors(arg_errors),

                (None, arg_errors) => tail
                    .apply_to_path(&JSON::Bool(false), vars, input_path)
                    .prepend_errors(arg_errors),
            },
            None => (
                None,
                vec![ApplyToError::new(
                    format!("Method ->{} requires an argument", method_name.as_ref()),
                    input_path.to_vec(),
                    method_name.range(),
                )],
            ),
        }
    } else {
        (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires an argument", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
            )],
        )
    }
}

// Returns the array or string element at the given index, as Option<JSON>. If
// the index is out of bounds, returns None and reports an error.
pub(super) fn get_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        if let Some(index_literal) = args.first() {
            match index_literal.apply_to_path(data, vars, input_path) {
                (Some(JSON::Number(n)), index_errors) => match (data, n.as_i64()) {
                    (JSON::Array(array), Some(i)) => {
                        // Negative indices count from the end of the array
                        if let Some(element) = array.get(if i < 0 {
                            (array.len() as i64 + i) as usize
                        } else {
                            i as usize
                        }) {
                            tail.apply_to_path(element, vars, input_path)
                                .prepend_errors(index_errors)
                        } else {
                            (
                                None,
                                vec_push(
                                    index_errors,
                                    ApplyToError::new(
                                        format!(
                                            "Method ->{}({}) index out of bounds",
                                            method_name.as_ref(),
                                            i,
                                        ),
                                        input_path.to_vec(),
                                        index_literal.range(),
                                    ),
                                ),
                            )
                        }
                    }

                    (JSON::String(s), Some(i)) => {
                        let s_str = s.as_str();
                        let ilen = s_str.len() as i64;
                        // Negative indices count from the end of the array
                        let index = if i < 0 { ilen + i } else { i };
                        if index >= 0 && index < ilen {
                            let uindex = index as usize;
                            let single_char_string = s_str[uindex..uindex + 1].to_string();
                            tail.apply_to_path(
                                &JSON::String(single_char_string.into()),
                                vars,
                                input_path,
                            )
                            .prepend_errors(index_errors)
                        } else {
                            (
                                None,
                                vec_push(
                                    index_errors,
                                    ApplyToError::new(
                                        format!(
                                            "Method ->{}({}) index out of bounds",
                                            method_name.as_ref(),
                                            i,
                                        ),
                                        input_path.to_vec(),
                                        index_literal.range(),
                                    ),
                                ),
                            )
                        }
                    }

                    (_, None) => (
                        None,
                        vec_push(
                            index_errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{} requires an integer index",
                                    method_name.as_ref()
                                ),
                                input_path.to_vec(),
                                index_literal.range(),
                            ),
                        ),
                    ),
                    _ => (
                        None,
                        vec_push(
                            index_errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{} requires an array or string input, not {}",
                                    method_name.as_ref(),
                                    json_type_name(data),
                                ),
                                input_path.to_vec(),
                                method_name.range(),
                            ),
                        ),
                    ),
                },
                (Some(ref key @ JSON::String(ref s)), index_errors) => match data {
                    JSON::Object(map) => {
                        if let Some(value) = map.get(s.as_str()) {
                            tail.apply_to_path(value, vars, input_path)
                                .prepend_errors(index_errors)
                        } else {
                            (
                                None,
                                vec_push(
                                    index_errors,
                                    ApplyToError::new(
                                        format!(
                                            "Method ->{}({}) object key not found",
                                            method_name.as_ref(),
                                            key
                                        ),
                                        input_path.to_vec(),
                                        index_literal.range(),
                                    ),
                                ),
                            )
                        }
                    }
                    _ => (
                        None,
                        vec_push(
                            index_errors,
                            ApplyToError::new(
                                format!(
                                    "Method ->{}({}) requires an object input",
                                    method_name.as_ref(),
                                    key
                                ),
                                input_path.to_vec(),
                                merge_ranges(
                                    method_name.range(),
                                    method_args.and_then(|args| args.range()),
                                ),
                            ),
                        ),
                    ),
                },
                (Some(value), index_errors) => (
                    None,
                    vec_push(
                        index_errors,
                        ApplyToError::new(
                            format!(
                                "Method ->{}({}) requires an integer or string argument",
                                method_name.as_ref(),
                                value,
                            ),
                            input_path.to_vec(),
                            index_literal.range(),
                        ),
                    ),
                ),
                (None, index_errors) => (
                    None,
                    vec_push(
                        index_errors,
                        ApplyToError::new(
                            format!(
                                "Method ->{} received undefined argument",
                                method_name.as_ref()
                            ),
                            input_path.to_vec(),
                            index_literal.range(),
                        ),
                    ),
                ),
            }
        } else {
            (
                None,
                vec![ApplyToError::new(
                    format!("Method ->{} requires an argument", method_name.as_ref()),
                    input_path.to_vec(),
                    method_name.range(),
                )],
            )
        }
    } else {
        (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires an argument", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
            )],
        )
    }
}

pub(super) fn keys_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
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
            )],
        );
    }

    match data {
        JSON::Object(map) => {
            let keys = map.keys().map(|key| JSON::String(key.clone())).collect();
            tail.apply_to_path(&JSON::Array(keys), vars, input_path)
        }
        _ => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an object input, not {}",
                    method_name.as_ref(),
                    json_type_name(data),
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        ),
    }
}

pub(super) fn values_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
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
            )],
        );
    }

    match data {
        JSON::Object(map) => {
            let values = map.values().cloned().collect();
            tail.apply_to_path(&JSON::Array(values), vars, input_path)
        }
        _ => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an object input, not {}",
                    method_name.as_ref(),
                    json_type_name(data),
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        ),
    }
}

pub(super) fn not_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
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
            )],
        )
    } else {
        tail.apply_to_path(&JSON::Bool(!is_truthy(data)), vars, input_path)
    }
}

fn is_truthy(data: &JSON) -> bool {
    match data {
        JSON::Bool(b) => *b,
        JSON::Number(n) => n.as_f64().map_or(false, |n| n != 0.0),
        JSON::Null => false,
        JSON::String(s) => !s.as_str().is_empty(),
        JSON::Object(_) | JSON::Array(_) => true,
    }
}

pub(super) fn or_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        let mut result = is_truthy(data);
        let mut errors = Vec::new();

        for arg in args {
            if result {
                break;
            }
            let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path);
            errors.extend(arg_errors);
            result = value_opt.map(|value| is_truthy(&value)).unwrap_or(false);
        }

        tail.apply_to_path(&JSON::Bool(result), vars, input_path)
            .prepend_errors(errors)
    } else {
        (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires arguments", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
            )],
        )
    }
}

pub(super) fn and_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        let mut result = is_truthy(data);
        let mut errors = Vec::new();

        for arg in args {
            if !result {
                break;
            }
            let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path);
            errors.extend(arg_errors);
            result = value_opt.map(|value| is_truthy(&value)).unwrap_or(false);
        }

        tail.apply_to_path(&JSON::Bool(result), vars, input_path)
            .prepend_errors(errors)
    } else {
        (
            None,
            vec![ApplyToError::new(
                format!("Method ->{} requires arguments", method_name.as_ref()),
                input_path.to_vec(),
                method_name.range(),
            )],
        )
    }
}
