use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use lazy_static::lazy_static;
use serde_json_bytes::serde_json::Number;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;

use super::helpers::json_type_name;
use super::immutable::InputPath;
use super::lit_expr::LitExpr;
use super::ApplyTo;
use super::ApplyToError;
use super::MethodArgs;
use super::PathList;
use super::VarsWithPathsMap;

type ArrowMethod = fn(
    // Method name
    &str,
    // Arguments passed to this method
    &Option<MethodArgs>,
    // The JSON input value (data)
    &JSON,
    // The variables
    &VarsWithPathsMap,
    // The input_path (may contain integers)
    &InputPath<JSON>,
    // The rest of the PathList
    &PathList,
    // Errors
    &mut IndexSet<ApplyToError>,
) -> Option<JSON>;

lazy_static! {
    pub(super) static ref ARROW_METHODS: IndexMap<String, ArrowMethod> = {
        let mut methods = IndexMap::<String, ArrowMethod>::default();

        // This built-in method returns its first input argument as-is, ignoring
        // the input data. Useful for embedding literal values, as in
        // $->echo("give me this string").
        methods.insert("echo".to_string(), echo_method);

        // Returns the type of the data as a string, e.g. "object", "array",
        // "string", "number", "boolean", or "null". Note that `typeof null` is
        // "object" in JavaScript but "null" for our purposes.
        methods.insert("typeof".to_string(), typeof_method);

        // When invoked against an array, ->map evaluates its first argument
        // against each element of the array and returns an array of the
        // results. When invoked against a non-array, ->map evaluates its first
        // argument against the data and returns the result.
        methods.insert("map".to_string(), map_method);

        // Returns true if the data is deeply equal to the first argument, false
        // otherwise. Equality is solely value-based (all JSON), no references.
        methods.insert("eq".to_string(), eq_method);

        // Takes any number of pairs [candidate, value], and returns value for
        // the first candidate that equals the input data $. If none of the
        // pairs match, a runtime error is reported, but a single-element
        // [<default>] array as the final argument guarantees a default value.
        methods.insert("match".to_string(), match_method);

        // Like ->match, but expects the first element of each pair to evaluate
        // to a boolean, returning the second element of the first pair whose
        // first element is true. This makes providing a final catch-all case
        // easy, since the last pair can be [true, <default>].
        methods.insert("matchIf".to_string(), match_if_method);
        methods.insert("match_if".to_string(), match_if_method);

        // Arithmetic methods
        methods.insert("add".to_string(), add_method);
        methods.insert("sub".to_string(), sub_method);
        methods.insert("mul".to_string(), mul_method);
        methods.insert("div".to_string(), div_method);
        methods.insert("mod".to_string(), mod_method);

        // Array/string methods (note that ->has and ->get also work for array
        // and string indexes)
        methods.insert("first".to_string(), first_method);
        methods.insert("last".to_string(), last_method);
        methods.insert("slice".to_string(), slice_method);
        methods.insert("size".to_string(), size_method);

        // Object methods (note that ->size also works for objects)
        methods.insert("has".to_string(), has_method);
        methods.insert("get".to_string(), get_method);
        methods.insert("keys".to_string(), keys_method);
        methods.insert("values".to_string(), values_method);
        methods.insert("entries".to_string(), entries_method);

        // Logical methods
        methods.insert("not".to_string(), not_method);
        methods.insert("or".to_string(), or_method);
        methods.insert("and".to_string(), and_method);

        methods
    };
}

fn echo_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        if let Some(arg) = args.first() {
            return arg
                .apply_to_path(data, vars, input_path, errors)
                .and_then(|value| tail.apply_to_path(&value, vars, input_path, errors));
        }
    }
    errors.insert(ApplyToError::new(
        format!("Method ->{} requires one argument", method_name),
        input_path.to_vec(),
    ));
    None
}

fn typeof_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(_)) = method_args {
        errors.insert(ApplyToError::new(
            format!("Method ->{} does not take any arguments", method_name),
            input_path.to_vec(),
        ));
        None
    } else {
        let typeof_string = JSON::String(json_type_name(data).to_string().into());
        tail.apply_to_path(&typeof_string, vars, input_path, errors)
    }
}

fn map_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        if let Some(first_arg) = args.first() {
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
                format!("Method ->{} requires one argument", method_name),
                input_path.to_vec(),
            ));
            None
        }
    } else {
        errors.insert(ApplyToError::new(
            format!("Method ->{} requires one argument", method_name),
            input_path.to_vec(),
        ));
        None
    }
}

fn eq_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        if args.len() == 1 {
            let matches = if let Some(value) = args[0].apply_to_path(data, vars, input_path, errors)
            {
                data == &value
            } else {
                false
            };
            return tail.apply_to_path(&JSON::Bool(matches), vars, input_path, errors);
        }
    }
    errors.insert(ApplyToError::new(
        format!("Method ->{} requires exactly one argument", method_name),
        input_path.to_vec(),
    ));
    None
}

fn match_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    // Takes any number of pairs [key, value], and returns value for the first
    // key that equals the data. If none of the pairs match, returns None. A
    // single-element unconditional [value] may appear at the end.
    if let Some(MethodArgs(args)) = method_args {
        for pair in args {
            if let LitExpr::Array(pair) = pair {
                if pair.len() == 1 {
                    return pair[0]
                        .apply_to_path(data, vars, input_path, errors)
                        .and_then(|value| tail.apply_to_path(&value, vars, input_path, errors));
                }

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
            method_name
        ),
        input_path.to_vec(),
    ));
    None
}

// Like ->match, but expects the first element of each pair
// to evaluate to a boolean, returning the second element of
// the first pair whose first element is true. This makes
// providing a final catch-all case easy, since the last
// pair can be [true, <default>].
fn match_if_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        for pair in args {
            if let LitExpr::Array(pair) = pair {
                if pair.len() == 2 {
                    if let Some(JSON::Bool(true)) =
                        pair[0].apply_to_path(data, vars, input_path, errors)
                    {
                        return pair[1]
                            .apply_to_path(data, vars, input_path, errors)
                            .and_then(|value| {
                                tail.apply_to_path(&value, vars, input_path, errors)
                            });
                    };
                }
            }
        }
    }
    errors.insert(ApplyToError::new(
        format!(
            "Method ->{} did not match any [condition, value] pair",
            method_name
        ),
        input_path.to_vec(),
    ));
    None
}

fn arithmetic_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    op: impl Fn(&Number, &Number) -> Option<Number>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        if let JSON::Number(result) = data {
            let mut result = result.clone();
            for arg in args {
                let value_opt = arg.apply_to_path(data, vars, input_path, errors);
                if let Some(JSON::Number(n)) = value_opt {
                    if let Some(new_result) = op(&result, &n) {
                        result = new_result;
                    } else {
                        errors.insert(ApplyToError::new(
                            format!("Method ->{} failed on argument {}", method_name, n),
                            input_path.to_vec(),
                        ));
                        return None;
                    }
                } else {
                    errors.insert(ApplyToError::new(
                        format!("Method ->{} requires numeric arguments", method_name),
                        input_path.to_vec(),
                    ));
                    return None;
                }
            }
            Some(JSON::Number(result))
        } else {
            errors.insert(ApplyToError::new(
                format!("Method ->{} requires numeric arguments", method_name),
                input_path.to_vec(),
            ));
            None
        }
    } else {
        errors.insert(ApplyToError::new(
            format!("Method ->{} requires at least one argument", method_name),
            input_path.to_vec(),
        ));
        None
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
        fn $name(
            method_name: &str,
            method_args: &Option<MethodArgs>,
            data: &JSON,
            vars: &VarsWithPathsMap,
            input_path: &InputPath<JSON>,
            tail: &PathList,
            errors: &mut IndexSet<ApplyToError>,
        ) -> Option<JSON> {
            if let Some(result) = arithmetic_method(
                method_name,
                method_args,
                &$op,
                data,
                vars,
                input_path,
                errors,
            ) {
                tail.apply_to_path(&result, vars, input_path, errors)
            } else {
                None
            }
        }
    };
}
infix_math_method!(add_method, add_op);
infix_math_method!(sub_method, sub_op);
infix_math_method!(mul_method, mul_op);
infix_math_method!(div_method, div_op);
infix_math_method!(mod_method, rem_op);

fn first_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(_)) = method_args {
        errors.insert(ApplyToError::new(
            format!("Method ->{} does not take any arguments", method_name),
            input_path.to_vec(),
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

fn last_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(_)) = method_args {
        errors.insert(ApplyToError::new(
            format!("Method ->{} does not take any arguments", method_name),
            input_path.to_vec(),
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

fn has_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        match args.first() {
            Some(arg) => match &arg.apply_to_path(data, vars, input_path, errors) {
                Some(json_index @ JSON::Number(n)) => match (data, n.as_i64()) {
                    (JSON::Array(array), Some(index)) => {
                        let ilen = array.len() as i64;
                        // Negative indices count from the end of the array
                        let index = if index < 0 { ilen + index } else { index };
                        tail.apply_to_path(
                            &JSON::Bool(index >= 0 && index < ilen),
                            vars,
                            &input_path.append(json_index.clone()),
                            errors,
                        )
                    }
                    (json_key @ JSON::String(s), Some(index)) => {
                        let ilen = s.as_str().len() as i64;
                        // Negative indices count from the end of the array
                        let index = if index < 0 { ilen + index } else { index };
                        tail.apply_to_path(
                            &JSON::Bool(index >= 0 && index < ilen),
                            vars,
                            &input_path.append(json_key.clone()),
                            errors,
                        )
                    }
                    _ => tail.apply_to_path(
                        &JSON::Bool(false),
                        vars,
                        &input_path.append(json_index.clone()),
                        errors,
                    ),
                },
                Some(json_key @ JSON::String(s)) => match data {
                    JSON::Object(map) => tail.apply_to_path(
                        &JSON::Bool(map.contains_key(s.as_str())),
                        vars,
                        &input_path.append(json_key.clone()),
                        errors,
                    ),
                    _ => tail.apply_to_path(
                        &JSON::Bool(false),
                        vars,
                        &input_path.append(json_key.clone()),
                        errors,
                    ),
                },
                Some(value) => tail.apply_to_path(
                    &JSON::Bool(false),
                    vars,
                    &input_path.append(value.clone()),
                    errors,
                ),
                None => tail.apply_to_path(&JSON::Bool(false), vars, input_path, errors),
            },
            None => {
                errors.insert(ApplyToError::new(
                    format!("Method ->{} requires an argument", method_name),
                    input_path.to_vec(),
                ));
                None
            }
        }
    } else {
        errors.insert(ApplyToError::new(
            format!("Method ->{} requires an argument", method_name),
            input_path.to_vec(),
        ));
        None
    }
}

// Returns the array or string element at the given index, as Option<JSON>. If
// the index is out of bounds, returns None and reports an error.
fn get_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        if let Some(index_literal) = args.first() {
            match &index_literal.apply_to_path(data, vars, input_path, errors) {
                Some(JSON::Number(n)) => match (data, n.as_i64()) {
                    (JSON::Array(array), Some(i)) => {
                        // Negative indices count from the end of the array
                        if let Some(element) = array.get(if i < 0 {
                            (array.len() as i64 + i) as usize
                        } else {
                            i as usize
                        }) {
                            tail.apply_to_path(element, vars, input_path, errors)
                        } else {
                            errors.insert(ApplyToError::new(
                                format!(
                                    "Method ->{}({}) array index out of bounds",
                                    method_name, i,
                                ),
                                input_path.to_vec(),
                            ));
                            None
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
                                errors,
                            )
                        } else {
                            errors.insert(ApplyToError::new(
                                format!(
                                    "Method ->{}({}) string index out of bounds",
                                    method_name, i,
                                ),
                                input_path.to_vec(),
                            ));
                            None
                        }
                    }
                    (_, None) => {
                        errors.insert(ApplyToError::new(
                            format!("Method ->{} requires an integer index", method_name),
                            input_path.to_vec(),
                        ));
                        None
                    }
                    _ => {
                        errors.insert(ApplyToError::new(
                            format!(
                                "Method ->{} requires an array or string input, not {}",
                                method_name,
                                json_type_name(data),
                            ),
                            input_path.to_vec(),
                        ));
                        None
                    }
                },
                Some(key @ JSON::String(s)) => match data {
                    JSON::Object(map) => {
                        if let Some(value) = map.get(s.as_str()) {
                            tail.apply_to_path(value, vars, input_path, errors)
                        } else {
                            errors.insert(ApplyToError::new(
                                format!("Method ->{}({}) object key not found", method_name, key),
                                input_path.to_vec(),
                            ));
                            None
                        }
                    }
                    _ => {
                        errors.insert(ApplyToError::new(
                            format!("Method ->{}({}) requires an object input", method_name, key),
                            input_path.to_vec(),
                        ));
                        None
                    }
                },
                Some(value) => {
                    errors.insert(ApplyToError::new(
                        format!(
                            "Method ->{}({}) requires an integer or string argument",
                            method_name, value,
                        ),
                        input_path.to_vec(),
                    ));
                    None
                }
                None => {
                    errors.insert(ApplyToError::new(
                        format!("Method ->{} received undefined argument", method_name),
                        input_path.to_vec(),
                    ));
                    None
                }
            }
        } else {
            errors.insert(ApplyToError::new(
                format!("Method ->{} requires an argument", method_name),
                input_path.to_vec(),
            ));
            None
        }
    } else {
        errors.insert(ApplyToError::new(
            format!("Method ->{} requires an argument", method_name),
            input_path.to_vec(),
        ));
        None
    }
}

fn slice_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    let length = if let JSON::Array(array) = data {
        array.len() as i64
    } else if let JSON::String(s) = data {
        s.as_str().len() as i64
    } else {
        errors.insert(ApplyToError::new(
            format!("Method ->{} requires an array or string input", method_name),
            input_path.to_vec(),
        ));
        return None;
    };

    if let Some(MethodArgs(args)) = method_args {
        let start = args
            .first()
            .and_then(|arg| arg.apply_to_path(data, vars, input_path, errors))
            .and_then(|n| n.as_i64())
            .unwrap_or(0)
            .max(0)
            .min(length) as usize;
        let end = args
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

fn size_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(_)) = method_args {
        errors.insert(ApplyToError::new(
            format!("Method ->{} does not take any arguments", method_name),
            input_path.to_vec(),
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
                    method_name,
                    json_type_name(data),
                ),
                input_path.to_vec(),
            ));
            None
        }
    }
}

fn keys_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(_)) = method_args {
        errors.insert(ApplyToError::new(
            format!("Method ->{} does not take any arguments", method_name),
            input_path.to_vec(),
        ));
        return None;
    }

    match data {
        JSON::Object(map) => {
            let keys = map.keys().map(|key| JSON::String(key.clone())).collect();
            tail.apply_to_path(&JSON::Array(keys), vars, input_path, errors)
        }
        _ => {
            errors.insert(ApplyToError::new(
                format!(
                    "Method ->{} requires an object input, not {}",
                    method_name,
                    json_type_name(data),
                ),
                input_path.to_vec(),
            ));
            None
        }
    }
}

fn values_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(_)) = method_args {
        errors.insert(ApplyToError::new(
            format!("Method ->{} does not take any arguments", method_name),
            input_path.to_vec(),
        ));
        return None;
    }

    match data {
        JSON::Object(map) => {
            let values = map.values().cloned().collect();
            tail.apply_to_path(&JSON::Array(values), vars, input_path, errors)
        }
        _ => {
            errors.insert(ApplyToError::new(
                format!(
                    "Method ->{} requires an object input, not {}",
                    method_name,
                    json_type_name(data),
                ),
                input_path.to_vec(),
            ));
            None
        }
    }
}

// Returns a list of [{ key, value }, ...] objects for each key-value pair in
// the object. Returning a list of [[ key, value ], ...] pairs might also seem
// like an option, but GraphQL doesn't handle heterogeneous lists (or tuples) as
// well as it handles objects with named properties like { key, value }.
fn entries_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(_)) = method_args {
        errors.insert(ApplyToError::new(
            format!("Method ->{} does not take any arguments", method_name),
            input_path.to_vec(),
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
                    method_name,
                    json_type_name(data),
                ),
                input_path.to_vec(),
            ));
            None
        }
    }
}

fn not_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if method_args.is_some() {
        errors.insert(ApplyToError::new(
            format!("Method ->{} does not take any arguments", method_name),
            input_path.to_vec(),
        ));
        None
    } else {
        tail.apply_to_path(&JSON::Bool(!is_truthy(data)), vars, input_path, errors)
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

fn or_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        let mut result = is_truthy(data);
        for arg in args {
            if result {
                break;
            }
            result = arg
                .apply_to_path(data, vars, input_path, errors)
                .map(|value| is_truthy(&value))
                .unwrap_or(false);
        }
        tail.apply_to_path(&JSON::Bool(result), vars, input_path, errors)
    } else {
        errors.insert(ApplyToError::new(
            format!("Method ->{} requires arguments", method_name),
            input_path.to_vec(),
        ));
        None
    }
}

fn and_method(
    method_name: &str,
    method_args: &Option<MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &PathList,
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON> {
    if let Some(MethodArgs(args)) = method_args {
        let mut result = is_truthy(data);
        for arg in args {
            if !result {
                break;
            }
            result = arg
                .apply_to_path(data, vars, input_path, errors)
                .map(|value| is_truthy(&value))
                .unwrap_or(false);
        }
        tail.apply_to_path(&JSON::Bool(result), vars, input_path, errors)
    } else {
        errors.insert(ApplyToError::new(
            format!("Method ->{} requires arguments", method_name),
            input_path.to_vec(),
        ));
        None
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::selection;

    #[test]
    fn test_echo_method() {
        assert_eq!(
            selection!("$->echo('oyez')").apply_to(&json!(null)),
            (Some(json!("oyez")), vec![]),
        );

        assert_eq!(
            selection!("$->echo('oyez')").apply_to(&json!([1, 2, 3])),
            (Some(json!("oyez")), vec![]),
        );

        assert_eq!(
            selection!("$->echo([1, 2, 3]) { id: $ }").apply_to(&json!(null)),
            (Some(json!([{ "id": 1 }, { "id": 2 }, { "id": 3 }])), vec![]),
        );

        assert_eq!(
            selection!("$->echo([1, 2, 3])->last { id: $ }").apply_to(&json!(null)),
            (Some(json!({ "id": 3 })), vec![]),
        );

        assert_eq!(
            selection!("$->echo([1.1, 0.2, -3.3]) { id: $ }").apply_to(&json!(null)),
            (
                Some(json!([{ "id": 1.1 }, { "id": 0.2 }, { "id": -3.3 }])),
                vec![]
            ),
        );

        assert_eq!(
            selection!("$.nested.value->echo(['before', @, 'after'])").apply_to(&json!({
                "nested": {
                    "value": 123,
                },
            })),
            (Some(json!(["before", 123, "after"])), vec![]),
        );

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

        assert_eq!(
            selection!("data->echo(@.results->last)").apply_to(&json!({
                "data": {
                    "results": [1, 2, 3],
                },
            })),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("results->echo(@->first)").apply_to(&json!({
                "results": [
                    [1, 2, 3],
                    "ignored",
                ],
            })),
            (Some(json!([1, 2, 3])), vec![]),
        );

        assert_eq!(
            selection!("results->echo(@->first)->last").apply_to(&json!({
                "results": [
                    [1, 2, 3],
                    "ignored",
                ],
            })),
            (Some(json!(3)), vec![]),
        );

        {
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

    #[test]
    fn test_typeof_method() {
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

    #[test]
    fn test_map_method() {
        assert_eq!(
            selection!("$->map(@->add(10))").apply_to(&json!([1, 2, 3])),
            (Some(json!(vec![11, 12, 13])), vec![]),
        );

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

        assert_eq!(
            selection!("messages->map(@.roles)").apply_to(&json!({
                "messages": [
                    { "roles": ["admin"] },
                    { "roles": ["user", "guest"] },
                ],
            })),
            (Some(json!([["admin"], ["user", "guest"]])), vec![]),
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
            (Some(json!(1230)), vec![]),
        );
    }

    #[test]
    fn test_missing_method() {
        assert_eq!(
            selection!("nested.path->bogus").apply_to(&json!({
                "nested": {
                    "path": 123,
                },
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->bogus not found",
                    "path": ["nested", "path"],
                }))],
            ),
        );
    }

    #[test]
    fn test_match_methods() {
        assert_eq!(
            selection!(
                r#"
                name
                __typename: kind->match(
                    ['dog', 'Canine'],
                    ['cat', 'Feline']
                )
                "#
            )
            .apply_to(&json!({
                "kind": "cat",
                "name": "Whiskers",
            })),
            (
                Some(json!({
                    "__typename": "Feline",
                    "name": "Whiskers",
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                name
                __typename: kind->match(
                    ['dog', 'Canine'],
                    ['cat', 'Feline'],
                    [@, 'Exotic']
                )
                "#
            )
            .apply_to(&json!({
                "kind": "axlotl",
                "name": "Gulpy",
            })),
            (
                Some(json!({
                    "__typename": "Exotic",
                    "name": "Gulpy",
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                name
                __typename: kind->match(
                    ['dog', 'Canine'],
                    ['cat', 'Feline'],
                    ['Exotic']
                )
                "#
            )
            .apply_to(&json!({
                "kind": "axlotl",
                "name": "Gulpy",
            })),
            (
                Some(json!({
                    "__typename": "Exotic",
                    "name": "Gulpy",
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                name
                __typename: kind->match(
                    ['dog', 'Canine'],
                    ['cat', 'Feline'],
                    ['Exotic']
                )
                "#
            )
            .apply_to(&json!({
                "kind": "dog",
                "name": "Laika",
            })),
            (
                Some(json!({
                    "__typename": "Canine",
                    "name": "Laika",
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                num: value->matchIf(
                    [@->typeof->eq('number'), @],
                    [true, 'not a number']
                )
                "#
            )
            .apply_to(&json!({ "value": 123 })),
            (
                Some(json!({
                    "num": 123,
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                num: value->matchIf(
                    [@->typeof->eq('number'), @],
                    [true, 'not a number']
                )
                "#
            )
            .apply_to(&json!({ "value": true })),
            (
                Some(json!({
                    "num": "not a number",
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                result->matchIf(
                    [@->typeof->eq('boolean'), @],
                    [true, 'not boolean']
                )
                "#
            )
            .apply_to(&json!({
                "result": true,
            })),
            (Some(json!(true)), vec![]),
        );

        assert_eq!(
            selection!(
                r#"
                result->match_if(
                    [@->typeof->eq('boolean'), @],
                    [true, 'not boolean']
                )
                "#
            )
            .apply_to(&json!({
                "result": 321,
            })),
            (Some(json!("not boolean")), vec![]),
        );
    }

    fn test_arithmetic_methods() {
        assert_eq!(
            selection!("$->add(1)").apply_to(&json!(2)),
            (Some(json!(3)), vec![]),
        );
        assert_eq!(
            selection!("$->add(1.5)").apply_to(&json!(2)),
            (Some(json!(3.5)), vec![]),
        );
        assert_eq!(
            selection!("$->add(1)").apply_to(&json!(2.5)),
            (Some(json!(3.5)), vec![]),
        );
        assert_eq!(
            selection!("$->add(1, 2, 3, 5, 8)").apply_to(&json!(1)),
            (Some(json!(20)), vec![]),
        );

        assert_eq!(
            selection!("$->sub(1)").apply_to(&json!(2)),
            (Some(json!(1)), vec![]),
        );
        assert_eq!(
            selection!("$->sub(1.5)").apply_to(&json!(2)),
            (Some(json!(0.5)), vec![]),
        );
        assert_eq!(
            selection!("$->sub(10)").apply_to(&json!(2.5)),
            (Some(json!(-7.5)), vec![]),
        );
        assert_eq!(
            selection!("$->sub(10, 2.5)").apply_to(&json!(2.5)),
            (Some(json!(-10.0)), vec![]),
        );

        assert_eq!(
            selection!("$->mul(2)").apply_to(&json!(3)),
            (Some(json!(6)), vec![]),
        );
        assert_eq!(
            selection!("$->mul(2.5)").apply_to(&json!(3)),
            (Some(json!(7.5)), vec![]),
        );
        assert_eq!(
            selection!("$->mul(2)").apply_to(&json!(3.5)),
            (Some(json!(7.0)), vec![]),
        );
        assert_eq!(
            selection!("$->mul(-2.5)").apply_to(&json!(3.5)),
            (Some(json!(-8.75)), vec![]),
        );
        assert_eq!(
            selection!("$->mul(2, 3, 5, 7)").apply_to(&json!(10)),
            (Some(json!(2100)), vec![]),
        );

        assert_eq!(
            selection!("$->div(2)").apply_to(&json!(6)),
            (Some(json!(3)), vec![]),
        );
        assert_eq!(
            selection!("$->div(2.5)").apply_to(&json!(7.5)),
            (Some(json!(3.0)), vec![]),
        );
        assert_eq!(
            selection!("$->div(2)").apply_to(&json!(7)),
            (Some(json!(3)), vec![]),
        );
        assert_eq!(
            selection!("$->div(2.5)").apply_to(&json!(7)),
            (Some(json!(2.8)), vec![]),
        );
        assert_eq!(
            selection!("$->div(2, 3, 5, 7)").apply_to(&json!(2100)),
            (Some(json!(10)), vec![]),
        );

        assert_eq!(
            selection!("$->mod(2)").apply_to(&json!(6)),
            (Some(json!(0)), vec![]),
        );
        assert_eq!(
            selection!("$->mod(2.5)").apply_to(&json!(7.5)),
            (Some(json!(0.0)), vec![]),
        );
        assert_eq!(
            selection!("$->mod(2)").apply_to(&json!(7)),
            (Some(json!(1)), vec![]),
        );
        assert_eq!(
            selection!("$->mod(4)").apply_to(&json!(7)),
            (Some(json!(3)), vec![]),
        );
        assert_eq!(
            selection!("$->mod(2.5)").apply_to(&json!(7)),
            (Some(json!(2.0)), vec![]),
        );
        assert_eq!(
            selection!("$->mod(2, 3, 5, 7)").apply_to(&json!(2100)),
            (Some(json!(0)), vec![]),
        );
    }

    #[test]
    fn test_array_methods() {
        assert_eq!(
            selection!("$->first").apply_to(&json!([1, 2, 3])),
            (Some(json!(1)), vec![]),
        );

        assert_eq!(selection!("$->first").apply_to(&json!([])), (None, vec![]),);

        assert_eq!(
            selection!("$->last").apply_to(&json!([1, 2, 3])),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(selection!("$->last").apply_to(&json!([])), (None, vec![]),);

        assert_eq!(
            selection!("$->get(1)").apply_to(&json!([1, 2, 3])),
            (Some(json!(2)), vec![]),
        );

        assert_eq!(
            selection!("$->get(-1)").apply_to(&json!([1, 2, 3])),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("numbers->map(@->get(-2))").apply_to(&json!({
                "numbers": [
                    [1, 2, 3],
                    [5, 6],
                ],
            })),
            (Some(json!([2, 5])), vec![]),
        );

        assert_eq!(
            selection!("$->get(3)").apply_to(&json!([1, 2, 3])),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get(3) array index out of bounds",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->get(-4)").apply_to(&json!([1, 2, 3])),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get(-4) array index out of bounds",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->get").apply_to(&json!([1, 2, 3])),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get requires an argument",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->get('bogus')").apply_to(&json!([1, 2, 3])),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get(\"bogus\") requires an object input",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->has(1)").apply_to(&json!([1, 2, 3])),
            (Some(json!(true)), vec![]),
        );

        assert_eq!(
            selection!("$->has(5)").apply_to(&json!([1, 2, 3])),
            (Some(json!(false)), vec![]),
        );

        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!([1, 2, 3, 4, 5])),
            (Some(json!([2, 3])), vec![]),
        );

        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!([1, 2])),
            (Some(json!([2])), vec![]),
        );

        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!([1])),
            (Some(json!([])), vec![]),
        );

        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!([])),
            (Some(json!([])), vec![]),
        );

        assert_eq!(
            selection!("$->size").apply_to(&json!([])),
            (Some(json!(0)), vec![]),
        );

        assert_eq!(
            selection!("$->size").apply_to(&json!([1, 2, 3])),
            (Some(json!(3)), vec![]),
        );
    }

    #[test]
    fn test_size_method_errors() {
        assert_eq!(
            selection!("$->size").apply_to(&json!(null)),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->size requires an array, string, or object input, not null",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->size").apply_to(&json!(true)),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->size requires an array, string, or object input, not boolean",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("count->size").apply_to(&json!({
                "count": 123,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->size requires an array, string, or object input, not number",
                    "path": ["count"],
                }))]
            ),
        );
    }

    #[test]
    fn test_string_methods() {
        assert_eq!(
            selection!("$->has(2)").apply_to(&json!("oyez")),
            (Some(json!(true)), vec![]),
        );

        assert_eq!(
            selection!("$->has(-2)").apply_to(&json!("oyez")),
            (Some(json!(true)), vec![]),
        );

        assert_eq!(
            selection!("$->has(10)").apply_to(&json!("oyez")),
            (Some(json!(false)), vec![]),
        );

        assert_eq!(
            selection!("$->has(-10)").apply_to(&json!("oyez")),
            (Some(json!(false)), vec![]),
        );

        assert_eq!(
            selection!("$->first").apply_to(&json!("hello")),
            (Some(json!("h")), vec![]),
        );

        assert_eq!(
            selection!("$->last").apply_to(&json!("hello")),
            (Some(json!("o")), vec![]),
        );

        assert_eq!(
            selection!("$->get(2)").apply_to(&json!("oyez")),
            (Some(json!("e")), vec![]),
        );

        assert_eq!(
            selection!("$->get(-1)").apply_to(&json!("oyez")),
            (Some(json!("z")), vec![]),
        );

        assert_eq!(
            selection!("$->get(3)").apply_to(&json!("oyez")),
            (Some(json!("z")), vec![]),
        );

        assert_eq!(
            selection!("$->get(4)").apply_to(&json!("oyez")),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get(4) string index out of bounds",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->get($->echo(-5)->mul(2))").apply_to(&json!("oyez")),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get(-10) string index out of bounds",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->get(true)").apply_to(&json!("input")),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get(true) requires an integer or string argument",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!("")),
            (Some(json!("")), vec![]),
        );

        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!("hello")),
            (Some(json!("el")), vec![]),
        );

        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!("he")),
            (Some(json!("e")), vec![]),
        );

        assert_eq!(
            selection!("$->slice(1, 3)").apply_to(&json!("h")),
            (Some(json!("")), vec![]),
        );

        assert_eq!(
            selection!("$->size").apply_to(&json!("hello")),
            (Some(json!(5)), vec![]),
        );

        assert_eq!(
            selection!("$->size").apply_to(&json!("")),
            (Some(json!(0)), vec![]),
        );
    }

    #[test]
    fn test_object_methods() {
        assert_eq!(
            selection!("object->has('a')").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(true)), vec![]),
        );

        assert_eq!(
            selection!("object->has('c')").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(false)), vec![]),
        );

        assert_eq!(
            selection!("object->has(true)").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(false)), vec![]),
        );

        assert_eq!(
            selection!("object->has(null)").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(false)), vec![]),
        );

        assert_eq!(
            selection!("object->has('a')->and(object->has('b'))").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(true)), vec![]),
        );

        assert_eq!(
            selection!("object->has('b')->and(object->has('c'))").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!(false)), vec![]),
        );

        assert_eq!(
            selection!("object->has('xxx')->typeof").apply_to(&json!({
                "object": {
                    "a": 123,
                    "b": 456,
                },
            })),
            (Some(json!("boolean")), vec![]),
        );

        assert_eq!(
            selection!("$->size").apply_to(&json!({ "a": 1, "b": 2, "c": 3 })),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("$->size").apply_to(&json!({})),
            (Some(json!(0)), vec![]),
        );

        assert_eq!(
            selection!("$->get('a')").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(1)), vec![]),
        );

        assert_eq!(
            selection!("$->get('b')").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(2)), vec![]),
        );

        assert_eq!(
            selection!("$->get('c')").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("$->get('d')").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get(\"d\") object key not found",
                    "path": [],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->get('a')->add(10)").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(11)), vec![]),
        );

        assert_eq!(
            selection!("$->get('b')->add(10)").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(12)), vec![]),
        );

        assert_eq!(
            selection!("$->keys").apply_to(&json!({
                "a": 1,
                "b": 2,
                "c": 3,
            })),
            (Some(json!(["a", "b", "c"])), vec![]),
        );

        assert_eq!(
            selection!("$->keys").apply_to(&json!({})),
            (Some(json!([])), vec![]),
        );

        assert_eq!(
            selection!("notAnObject->keys").apply_to(&json!({
                "notAnObject": 123,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->keys requires an object input, not number",
                    "path": ["notAnObject"],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->values").apply_to(&json!({
                "a": 1,
                "b": "two",
                "c": false,
            })),
            (Some(json!([1, "two", false])), vec![]),
        );

        assert_eq!(
            selection!("$->values").apply_to(&json!({})),
            (Some(json!([])), vec![]),
        );

        assert_eq!(
            selection!("notAnObject->values").apply_to(&json!({
                "notAnObject": null,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->values requires an object input, not null",
                    "path": ["notAnObject"],
                }))]
            ),
        );

        assert_eq!(
            selection!("$->entries").apply_to(&json!({
                "a": 1,
                "b": "two",
                "c": false,
            })),
            (
                Some(json!([
                    { "key": "a", "value": 1 },
                    { "key": "b", "value": "two" },
                    { "key": "c", "value": false },
                ])),
                vec![],
            ),
        );

        assert_eq!(
            // This is just like $->keys, given the automatic array mapping of
            // .key, though you probably want to use ->keys directly because it
            // avoids cloning all the values unnecessarily.
            selection!("$->entries.key").apply_to(&json!({
                "one": 1,
                "two": 2,
                "three": 3,
            })),
            (Some(json!(["one", "two", "three"])), vec![]),
        );

        assert_eq!(
            // This is just like $->values, given the automatic array mapping of
            // .value, though you probably want to use ->values directly because
            // it avoids cloning all the keys unnecessarily.
            selection!("$->entries.value").apply_to(&json!({
                "one": 1,
                "two": 2,
                "three": 3,
            })),
            (Some(json!([1, 2, 3])), vec![]),
        );

        assert_eq!(
            selection!("$->entries").apply_to(&json!({})),
            (Some(json!([])), vec![]),
        );

        assert_eq!(
            selection!("notAnObject->entries").apply_to(&json!({
                "notAnObject": true,
            })),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->entries requires an object input, not boolean",
                    "path": ["notAnObject"],
                }))]
            ),
        );
    }

    #[test]
    fn test_logical_methods() {
        assert_eq!(
            selection!("$->map(@->not)").apply_to(&json!([
                true,
                false,
                0,
                1,
                -123,
                null,
                "hello",
                {},
                [],
            ])),
            (
                Some(json!([
                    false, true, true, false, false, true, false, false, false,
                ])),
                vec![],
            ),
        );

        assert_eq!(
            selection!("$->map(@->not->not)").apply_to(&json!([
                true,
                false,
                0,
                1,
                -123,
                null,
                "hello",
                {},
                [],
            ])),
            (
                Some(json!([
                    true, false, false, true, true, false, true, true, true,
                ])),
                vec![],
            ),
        );

        assert_eq!(
            selection!("$.a->and($.b, $.c)").apply_to(&json!({
                "a": true,
                "b": null,
                "c": true,
            })),
            (Some(json!(false)), vec![]),
        );
        assert_eq!(
            selection!("$.b->and($.c, $.a)").apply_to(&json!({
                "a": "hello",
                "b": true,
                "c": 123,
            })),
            (Some(json!(true)), vec![]),
        );
        assert_eq!(
            selection!("$.both->and($.and)").apply_to(&json!({
                "both": true,
                "and": true,
            })),
            (Some(json!(true)), vec![]),
        );
        assert_eq!(
            selection!("data.x->and($.data.y)").apply_to(&json!({
                "data": {
                    "x": true,
                    "y": false,
                },
            })),
            (Some(json!(false)), vec![]),
        );

        assert_eq!(
            selection!("$.a->or($.b, $.c)").apply_to(&json!({
                "a": true,
                "b": null,
                "c": true,
            })),
            (Some(json!(true)), vec![]),
        );
        assert_eq!(
            selection!("$.b->or($.a, $.c)").apply_to(&json!({
                "a": false,
                "b": null,
                "c": 0,
            })),
            (Some(json!(false)), vec![]),
        );
        assert_eq!(
            selection!("$.both->or($.and)").apply_to(&json!({
                "both": true,
                "and": false,
            })),
            (Some(json!(true)), vec![]),
        );
        assert_eq!(
            selection!("data.x->or($.data.y)").apply_to(&json!({
                "data": {
                    "x": false,
                    "y": false,
                },
            })),
            (Some(json!(false)), vec![]),
        );
    }
}
