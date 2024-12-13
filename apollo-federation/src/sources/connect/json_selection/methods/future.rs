// The future.rs module contains methods that are not yet exposed for use in
// JSONSelection strings in connector schemas, but have proposed implementations
// and tests. After careful review, they may one day move to public.rs.

use apollo_compiler::collections::IndexMap;
use serde_json::Number;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::impl_arrow_method;
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

impl_arrow_method!(TypeOfMethod, typeof_method, typeof_shape);
fn typeof_method(
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
fn typeof_shape(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    // TODO Compute this union type once and clone it here.
    Shape::one(&[
        Shape::string_value("null"),
        Shape::string_value("boolean"),
        Shape::string_value("number"),
        Shape::string_value("string"),
        Shape::string_value("array"),
        Shape::string_value("object"),
    ])
}

impl_arrow_method!(EqMethod, eq_method, eq_shape);
fn eq_method(
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
fn eq_shape(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    Shape::bool()
}

impl_arrow_method!(ThenMethod, then_method, then_shape);
fn then_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        if args.is_empty() || args.len() > 2 {
            (
                None,
                vec![ApplyToError::new(
                    format!(
                        "Method ->{} requires one or two arguments",
                        method_name.as_ref()
                    ),
                    input_path.to_vec(),
                    method_name.range(),
                )],
            )
        } else if is_truthy(data) {
            args[0]
                .apply_to_path(data, vars, input_path)
                .and_then_collecting_errors(|value| tail.apply_to_path(value, vars, input_path))
        } else if args.len() > 1 {
            args[1]
                .apply_to_path(data, vars, input_path)
                .and_then_collecting_errors(|value| tail.apply_to_path(value, vars, input_path))
        } else {
            // Allows ... $(false)->then(expression) to have no output keys.
            (None, Vec::new())
        }
    } else {
        (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires one or two arguments",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        )
    }
}
fn then_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if let Some(MethodArgs { args, .. }) = method_args {
        match args.len() {
            1 => Shape::one(&[
                args[0].compute_output_shape(
                    input_shape.clone(),
                    dollar_shape.clone(),
                    named_var_shapes,
                ),
                Shape::none(),
            ]),
            2 => Shape::one(&[
                args[0].compute_output_shape(
                    input_shape.clone(),
                    dollar_shape.clone(),
                    named_var_shapes,
                ),
                args[1].compute_output_shape(
                    input_shape.clone(),
                    dollar_shape.clone(),
                    named_var_shapes,
                ),
            ]),
            _ => Shape::error_with_range(
                "Method ->then requires one or two arguments",
                method_name.range(),
            ),
        }
    } else {
        Shape::error_with_range(
            "Method ->then requires one or two arguments",
            method_name.range(),
        )
    }
}

// Like ->match, but expects the first element of each pair to evaluate to a
// boolean, returning the second element of the first pair whose first element
// is true. This makes providing a final catch-all case easy, since the last
// pair can be [true, <default>].
impl_arrow_method!(MatchIfMethod, match_if_method, match_if_shape);
fn match_if_method(
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
fn match_if_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    use super::super::methods::public::match_shape;
    // Since match_shape does not inspect the candidate expressions, we can
    // reuse it for ->matchIf, where the only functional difference is that the
    // candidate expressions are expected to be boolean.
    match_shape(
        method_name,
        method_args,
        input_shape,
        dollar_shape,
        named_var_shapes,
    )
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

fn math_shape(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    Shape::error("TODO: math_shape")
}

macro_rules! infix_math_method {
    ($struct_name:ident, $fn_name:ident, $op:ident) => {
        impl_arrow_method!($struct_name, $fn_name, math_shape);
        fn $fn_name(
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
infix_math_method!(AddMethod, add_method, add_op);
infix_math_method!(SubMethod, sub_method, sub_op);
infix_math_method!(MulMethod, mul_method, mul_op);
infix_math_method!(DivMethod, div_method, div_op);
infix_math_method!(ModMethod, mod_method, rem_op);

impl_arrow_method!(HasMethod, has_method, has_shape);
fn has_method(
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
fn has_shape(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    // TODO We could be more clever here (sometimes) based on the input_shape
    // and argument shapes.
    Shape::bool()
}

impl_arrow_method!(GetMethod, get_method, get_shape);
fn get_method(
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
fn get_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if let Some(MethodArgs { args, .. }) = method_args {
        if let Some(index_literal) = args.first() {
            let index_shape = index_literal.compute_output_shape(
                input_shape.clone(),
                dollar_shape.clone(),
                named_var_shapes,
            );
            return match index_shape.case() {
                ShapeCase::String(value_opt) => match input_shape.case() {
                    ShapeCase::Object { fields, rest } => {
                        if let Some(literal_name) = value_opt {
                            if let Some(shape) = fields.get(literal_name.as_str()) {
                                return shape.clone();
                            }
                        }
                        let mut value_shapes = fields.values().cloned().collect::<Vec<_>>();
                        if !rest.is_none() {
                            value_shapes.push(rest.clone());
                        }
                        value_shapes.push(Shape::none());
                        Shape::one(&value_shapes)
                    }
                    ShapeCase::Array { .. } => Shape::error_with_range(
                        format!(
                            "Method ->{} applied to array requires integer index, not string",
                            method_name.as_ref()
                        )
                        .as_str(),
                        index_literal.range(),
                    ),
                    ShapeCase::String(_) => Shape::error_with_range(
                        format!(
                            "Method ->{} applied to string requires integer index, not string",
                            method_name.as_ref()
                        )
                        .as_str(),
                        index_literal.range(),
                    ),
                    _ => Shape::error("Method ->get requires an object, array, or string input"),
                },

                ShapeCase::Int(value_opt) => {
                    match input_shape.case() {
                        ShapeCase::Array { prefix, tail } => {
                            if let Some(index) = value_opt {
                                if let Some(item) = prefix.get(*index as usize) {
                                    return item.clone();
                                }
                            }
                            // If tail.is_none(), this will simplify to Shape::none().
                            Shape::one(&[tail.clone(), Shape::none()])
                        }

                        ShapeCase::String(Some(s)) => {
                            if let Some(index) = value_opt {
                                let index = *index as usize;
                                if index < s.len() {
                                    Shape::string_value(&s[index..index + 1])
                                } else {
                                    Shape::none()
                                }
                            } else {
                                Shape::one(&[Shape::string(), Shape::none()])
                            }
                        }
                        ShapeCase::String(None) => Shape::one(&[Shape::string(), Shape::none()]),

                        ShapeCase::Object { .. } => Shape::error_with_range(
                            format!(
                                "Method ->{} applied to object requires string index, not integer",
                                method_name.as_ref()
                            )
                            .as_str(),
                            index_literal.range(),
                        ),

                        _ => {
                            Shape::error("Method ->get requires an object, array, or string input")
                        }
                    }
                }

                _ => Shape::error_with_range(
                    format!(
                        "Method ->{} requires an integer or string argument",
                        method_name.as_ref()
                    )
                    .as_str(),
                    index_literal.range(),
                ),
            };
        }
    }

    Shape::error_with_range(
        format!("Method ->{} requires an argument", method_name.as_ref()).as_str(),
        method_name.range(),
    )
}

impl_arrow_method!(KeysMethod, keys_method, keys_shape);
fn keys_method(
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
fn keys_shape(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Object { fields, rest, .. } => {
            // Any statically known field names become string literal shapes in
            // the resulting keys array.
            let keys_vec = fields
                .keys()
                .map(|key| Shape::string_value(key.as_str()))
                .collect::<Vec<_>>();

            Shape::array(
                &keys_vec,
                // Since we're collecting key shapes, we want String for the
                // rest shape when it's not None.
                if rest.is_none() {
                    Shape::none()
                } else {
                    Shape::string()
                },
            )
        }
        _ => Shape::error("Method ->keys requires an object input"),
    }
}

impl_arrow_method!(ValuesMethod, values_method, values_shape);
fn values_method(
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
fn values_shape(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Object { fields, rest, .. } => {
            let values_vec = fields.values().cloned().collect::<Vec<_>>();
            Shape::array(&values_vec, rest.clone())
        }
        _ => Shape::error("Method ->values requires an object input"),
    }
}

impl_arrow_method!(NotMethod, not_method, not_shape);
fn not_method(
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
fn not_shape(
    _method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Bool(Some(value)) => Shape::bool_value(!*value),
        ShapeCase::Int(Some(value)) => Shape::bool_value(*value == 0),
        ShapeCase::String(Some(value)) => Shape::bool_value(value.is_empty()),
        ShapeCase::Null => Shape::bool_value(true),
        ShapeCase::Array { .. } | ShapeCase::Object { .. } => Shape::bool_value(false),
        _ => Shape::bool(),
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

impl_arrow_method!(OrMethod, or_method, or_shape);
fn or_method(
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
fn or_shape(
    _method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Bool(Some(true)) => {
            return Shape::bool_value(true);
        }
        ShapeCase::Int(Some(value)) if *value != 0 => {
            return Shape::bool_value(true);
        }
        ShapeCase::String(Some(value)) if !value.is_empty() => {
            return Shape::bool_value(true);
        }
        ShapeCase::Array { .. } | ShapeCase::Object { .. } => {
            return Shape::bool_value(true);
        }
        _ => {}
    };

    if let Some(MethodArgs { args, .. }) = method_args {
        for arg in args {
            let arg_shape = arg.compute_output_shape(
                input_shape.clone(),
                dollar_shape.clone(),
                named_var_shapes,
            );
            match arg_shape.case() {
                ShapeCase::Bool(Some(true)) => {
                    return Shape::bool_value(true);
                }
                ShapeCase::Int(Some(value)) if *value != 0 => {
                    return Shape::bool_value(true);
                }
                ShapeCase::String(Some(value)) if !value.is_empty() => {
                    return Shape::bool_value(true);
                }
                ShapeCase::Array { .. } | ShapeCase::Object { .. } => {
                    return Shape::bool_value(true);
                }
                _ => {}
            }
        }
    }

    Shape::bool()
}

impl_arrow_method!(AndMethod, and_method, and_shape);
fn and_method(
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
fn and_shape(
    _method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    match input_shape.case() {
        ShapeCase::Bool(Some(false)) => {
            return Shape::bool_value(false);
        }
        ShapeCase::Int(Some(value)) if *value == 0 => {
            return Shape::bool_value(false);
        }
        ShapeCase::String(Some(value)) if value.is_empty() => {
            return Shape::bool_value(false);
        }
        ShapeCase::Null => {
            return Shape::bool_value(false);
        }
        _ => {}
    };

    if let Some(MethodArgs { args, .. }) = method_args {
        for arg in args {
            let arg_shape = arg.compute_output_shape(
                input_shape.clone(),
                dollar_shape.clone(),
                named_var_shapes,
            );
            match arg_shape.case() {
                ShapeCase::Bool(Some(false)) => {
                    return Shape::bool_value(false);
                }
                ShapeCase::Int(Some(value)) if *value == 0 => {
                    return Shape::bool_value(false);
                }
                ShapeCase::String(Some(value)) if value.is_empty() => {
                    return Shape::bool_value(false);
                }
                ShapeCase::Null => {
                    return Shape::bool_value(false);
                }
                _ => {}
            }
        }
    }

    Shape::bool()
}
