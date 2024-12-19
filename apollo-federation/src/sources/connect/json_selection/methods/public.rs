use apollo_compiler::collections::IndexMap;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::impl_arrow_method;
use crate::sources::connect::json_selection::apply_to::ApplyToResultMethods;
use crate::sources::connect::json_selection::helpers::json_type_name;
use crate::sources::connect::json_selection::helpers::vec_push;
use crate::sources::connect::json_selection::immutable::InputPath;
use crate::sources::connect::json_selection::known_var::KnownVariable;
use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::location::merge_ranges;
use crate::sources::connect::json_selection::location::Ranged;
use crate::sources::connect::json_selection::location::WithRange;
use crate::sources::connect::json_selection::ApplyToError;
use crate::sources::connect::json_selection::ApplyToInternal;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::PathList;
use crate::sources::connect::json_selection::VarsWithPathsMap;

impl_arrow_method!(EchoMethod, echo_method, echo_shape);
fn echo_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        if let Some(arg) = args.first() {
            return arg
                .apply_to_path(data, vars, input_path)
                .and_then_collecting_errors(|value| tail.apply_to_path(value, vars, input_path));
        }
    }
    (
        None,
        vec![ApplyToError::new(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            input_path.to_vec(),
            method_name.range(),
        )],
    )
}
fn echo_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        return first_arg.compute_output_shape(input_shape, dollar_shape, named_var_shapes);
    }
    Shape::error_with_range(
        format!("Method ->{} requires one argument", method_name.as_ref()),
        method_name.range(),
    )
}

impl_arrow_method!(MapMethod, map_method, map_shape);
fn map_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(args) = method_args {
        if let Some(first_arg) = args.args.first() {
            if let JSON::Array(array) = data {
                let mut output = Vec::with_capacity(array.len());
                let mut errors = Vec::new();

                for (i, element) in array.iter().enumerate() {
                    let input_path = input_path.append(JSON::Number(i.into()));
                    let (applied_opt, arg_errors) =
                        first_arg.apply_to_path(element, vars, &input_path);
                    errors.extend(arg_errors);
                    if let Some(applied) = applied_opt {
                        let (value_opt, apply_errors) =
                            tail.apply_to_path(&applied, vars, &input_path);
                        errors.extend(apply_errors);
                        if let Some(value) = value_opt {
                            output.push(value);
                            continue;
                        }
                    }
                    output.push(JSON::Null);
                }

                return (Some(JSON::Array(output)), errors);
            } else {
                return first_arg.apply_to_path(data, vars, input_path);
            }
        } else {
            return (
                None,
                vec![ApplyToError::new(
                    format!("Method ->{} requires one argument", method_name.as_ref()),
                    input_path.to_vec(),
                    method_name.range(),
                )],
            );
        }
    }
    (
        None,
        vec![ApplyToError::new(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            input_path.to_vec(),
            method_name.range(),
        )],
    )
}
fn map_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if let Some(first_arg) = method_args.and_then(|args| args.args.first()) {
        match input_shape.case() {
            ShapeCase::Array { prefix, tail } => {
                let new_prefix = prefix
                    .iter()
                    .map(|shape| {
                        first_arg.compute_output_shape(
                            shape.clone(),
                            dollar_shape.clone(),
                            named_var_shapes,
                        )
                    })
                    .collect::<Vec<_>>();
                let new_tail = first_arg.compute_output_shape(
                    tail.clone(),
                    dollar_shape.clone(),
                    named_var_shapes,
                );
                Shape::array(new_prefix, new_tail)
            }
            ShapeCase::Name(_name, _subpath) => {
                // Since we do not know if a named shape is an array or a
                // non-array, we hedge the input shape using a .* subpath
                // wildcard, which denotes the union of all array element shapes
                // for arrays, or the shape itself (no union) for non-arrays.
                let any_subshape = input_shape.any_item();
                first_arg.compute_output_shape(
                    // When we ->map, the @ variable gets rebound to each
                    // element visited, but the $ variable stays the same.
                    any_subshape.clone(),
                    dollar_shape.clone(),
                    named_var_shapes,
                )
            }
            _ => first_arg.compute_output_shape(
                input_shape.clone(),
                dollar_shape.clone(),
                named_var_shapes,
            ),
        }
    } else {
        Shape::error_with_range(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.range(),
        )
    }
}

impl_arrow_method!(MatchMethod, match_method, match_shape);
fn match_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    // Takes any number of pairs [key, value], and returns value for the first
    // key that equals the data. If none of the pairs match, returns None.
    // Typically, the final pair will use @ as its key to ensure some default
    // value is returned.
    let mut errors = Vec::new();

    if let Some(MethodArgs { args, .. }) = method_args {
        for pair in args {
            if let LitExpr::Array(pair) = pair.as_ref() {
                if pair.len() == 2 {
                    let (candidate_opt, candidate_errors) =
                        pair[0].apply_to_path(data, vars, input_path);
                    errors.extend(candidate_errors);

                    if let Some(candidate) = candidate_opt {
                        if candidate == *data {
                            return pair[1]
                                .apply_to_path(data, vars, input_path)
                                .and_then_collecting_errors(|value| {
                                    tail.apply_to_path(value, vars, input_path)
                                })
                                .prepend_errors(errors);
                        }
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
                    "Method ->{} did not match any [candidate, value] pair",
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
pub(super) fn match_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if let Some(MethodArgs { args, .. }) = method_args {
        let mut result_union = Vec::new();
        let mut has_infallible_case = false;

        for pair in args {
            if let LitExpr::Array(pair) = pair.as_ref() {
                if pair.len() == 2 {
                    if let LitExpr::Path(path) = pair[0].as_ref() {
                        if let PathList::Var(known_var, _tail) = path.path.as_ref() {
                            if known_var.as_ref() == &KnownVariable::AtSign {
                                has_infallible_case = true;
                            }
                        }
                    };

                    let value_shape = pair[1].compute_output_shape(
                        input_shape.clone(),
                        dollar_shape.clone(),
                        named_var_shapes,
                    );
                    result_union.push(value_shape);
                }
            }
        }

        if !has_infallible_case {
            result_union.push(Shape::none());
        }

        if result_union.is_empty() {
            Shape::error_with_range(
                format!(
                    "Method ->{} requires at least one [candidate, value] pair",
                    method_name.as_ref(),
                ),
                merge_ranges(
                    method_name.range(),
                    method_args.and_then(|args| args.range()),
                ),
            )
        } else {
            Shape::one(result_union)
        }
    } else {
        Shape::error_with_range(
            format!(
                "Method ->{} requires at least one [candidate, value] pair",
                method_name.as_ref(),
            ),
            method_name.range(),
        )
    }
}

impl_arrow_method!(FirstMethod, first_method, first_shape);
fn first_method(
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
        JSON::Array(array) => {
            if let Some(first) = array.first() {
                tail.apply_to_path(first, vars, input_path)
            } else {
                (None, vec![])
            }
        }

        JSON::String(s) => {
            if let Some(first) = s.as_str().chars().next() {
                tail.apply_to_path(&JSON::String(first.to_string().into()), vars, input_path)
            } else {
                (None, vec![])
            }
        }

        _ => tail.apply_to_path(data, vars, input_path),
    }
}
fn first_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if method_args.is_some() {
        return Shape::error_with_range(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.range(),
        );
    }

    match input_shape.case() {
        ShapeCase::String(Some(value)) => Shape::string_value(&value[0..1]),
        ShapeCase::String(None) => Shape::string(),
        ShapeCase::Array { prefix, tail } => {
            if let Some(first) = prefix.first() {
                first.clone()
            } else if tail.is_none() {
                Shape::none()
            } else {
                Shape::one([tail.clone(), Shape::none()])
            }
        }
        ShapeCase::One(shapes) => Shape::one(shapes.iter().map(|shape| {
            first_shape(
                method_name,
                method_args,
                shape.clone(),
                Shape::none(), // $ is not used in `->first`
                _named_var_shapes,
            )
        })),
        // When there is no obvious first element, ->first gives us the input
        // value itself, which has input_shape.
        _ => input_shape.clone(),
    }
}

impl_arrow_method!(LastMethod, last_method, last_shape);
fn last_method(
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
        JSON::Array(array) => {
            if let Some(last) = array.last() {
                tail.apply_to_path(last, vars, input_path)
            } else {
                (None, vec![])
            }
        }

        JSON::String(s) => {
            if let Some(last) = s.as_str().chars().last() {
                tail.apply_to_path(&JSON::String(last.to_string().into()), vars, input_path)
            } else {
                (None, vec![])
            }
        }

        _ => tail.apply_to_path(data, vars, input_path),
    }
}
fn last_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if method_args.is_some() {
        return Shape::error_with_range(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.range(),
        );
    }

    match input_shape.case() {
        ShapeCase::String(Some(value)) => {
            if let Some(last_char) = value.chars().last() {
                Shape::string_value(last_char.to_string().as_str())
            } else {
                Shape::none()
            }
        }
        ShapeCase::String(None) => Shape::one([Shape::string(), Shape::none()]),
        ShapeCase::Array { prefix, tail } => {
            if tail.is_none() {
                if let Some(last) = prefix.last() {
                    last.clone()
                } else {
                    Shape::none()
                }
            } else if let Some(last) = prefix.last() {
                Shape::one([last.clone(), tail.clone(), Shape::none()])
            } else {
                Shape::one([tail.clone(), Shape::none()])
            }
        }
        // When there is no obvious last element, ->last gives us the input
        // value itself, which has input_shape.
        _ => input_shape.clone(),
    }
}

impl_arrow_method!(SliceMethod, slice_method, slice_shape);
fn slice_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    tail: &WithRange<PathList>,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let length = if let JSON::Array(array) = data {
        array.len() as i64
    } else if let JSON::String(s) = data {
        s.as_str().len() as i64
    } else {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an array or string input",
                    method_name.as_ref()
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        );
    };

    if let Some(MethodArgs { args, .. }) = method_args {
        let mut errors = Vec::new();

        let start = args
            .first()
            .and_then(|arg| {
                let (value_opt, apply_errors) = arg.apply_to_path(data, vars, input_path);
                errors.extend(apply_errors);
                value_opt
            })
            .and_then(|n| n.as_i64())
            .unwrap_or(0)
            .max(0)
            .min(length) as usize;

        let end = args
            .get(1)
            .and_then(|arg| {
                let (value_opt, apply_errors) = arg.apply_to_path(data, vars, input_path);
                errors.extend(apply_errors);
                value_opt
            })
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

        tail.apply_to_path(&array, vars, input_path)
            .prepend_errors(errors)
    } else {
        // TODO Should calling ->slice or ->slice() without arguments be an
        // error? In JavaScript, array->slice() copies the array, but that's not
        // so useful in an immutable value-typed language like JSONSelection.
        (Some(data.clone()), vec![])
    }
}
fn slice_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    // There are more clever shapes we could compute here (when start and end
    // are statically known integers and input_shape is an array or string with
    // statically known prefix elements, for example) but for now we play it
    // safe (and honest) by returning a new variable-length array whose element
    // shape is a union of the original element (prefix and tail) shapes.
    match input_shape.case() {
        ShapeCase::Array { prefix, tail } => {
            let mut one_shapes = prefix.clone();
            if !tail.is_none() {
                one_shapes.push(tail.clone());
            }
            Shape::array([], Shape::one(one_shapes))
        }
        ShapeCase::String(_) => Shape::string(),
        _ => Shape::error_with_range(
            format!(
                "Method ->{} requires an array or string input",
                method_name.as_ref()
            ),
            method_name.range(),
        ),
    }
}

impl_arrow_method!(SizeMethod, size_method, size_shape);
fn size_method(
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
        JSON::Array(array) => {
            let size = array.len() as i64;
            tail.apply_to_path(&JSON::Number(size.into()), vars, input_path)
        }
        JSON::String(s) => {
            let size = s.as_str().len() as i64;
            tail.apply_to_path(&JSON::Number(size.into()), vars, input_path)
        }
        // Though we can't ask for ->first or ->last or ->at(n) on an object, we
        // can safely return how many properties the object has for ->size.
        JSON::Object(map) => {
            let size = map.len() as i64;
            tail.apply_to_path(&JSON::Number(size.into()), vars, input_path)
        }
        _ => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires an array, string, or object input, not {}",
                    method_name.as_ref(),
                    json_type_name(data),
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        ),
    }
}
fn size_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if method_args.is_some() {
        return Shape::error_with_range(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.range(),
        );
    }

    match input_shape.case() {
        ShapeCase::String(Some(value)) => Shape::int_value(value.len() as i64),
        ShapeCase::String(None) => Shape::int(),
        ShapeCase::Array { prefix, tail } => {
            if tail.is_none() {
                Shape::int_value(prefix.len() as i64)
            } else {
                Shape::int()
            }
        }
        ShapeCase::Object { fields, rest, .. } => {
            if rest.is_none() {
                Shape::int_value(fields.len() as i64)
            } else {
                Shape::int()
            }
        }
        _ => Shape::error_with_range(
            format!(
                "Method ->{} requires an array, string, or object input",
                method_name.as_ref()
            ),
            method_name.range(),
        ),
    }
}

impl_arrow_method!(EntriesMethod, entries_method, entries_shape);
fn entries_method(
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
            let entries = map
                .iter()
                .map(|(key, value)| {
                    let mut key_value_pair = JSONMap::new();
                    key_value_pair.insert(ByteString::from("key"), JSON::String(key.clone()));
                    key_value_pair.insert(ByteString::from("value"), value.clone());
                    JSON::Object(key_value_pair)
                })
                .collect();
            tail.apply_to_path(&JSON::Array(entries), vars, input_path)
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
fn entries_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
) -> Shape {
    if method_args.is_some() {
        return Shape::error_with_range(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.range(),
        );
    }

    match input_shape.case() {
        ShapeCase::Object { fields, rest, .. } => {
            let entry_shapes = fields
                .iter()
                .map(|(key, value)| {
                    let mut key_value_pair = Shape::empty_map();
                    key_value_pair.insert("key".to_string(), Shape::string_value(key.as_str()));
                    key_value_pair.insert("value".to_string(), value.clone());
                    Shape::object(key_value_pair, Shape::none())
                })
                .collect::<Vec<_>>();

            if rest.is_none() {
                Shape::array(entry_shapes, rest.clone())
            } else {
                let mut tail_key_value_pair = Shape::empty_map();
                tail_key_value_pair.insert("key".to_string(), Shape::string());
                tail_key_value_pair.insert("value".to_string(), rest.clone());
                Shape::array(
                    entry_shapes,
                    Shape::object(tail_key_value_pair, Shape::none()),
                )
            }
        }
        _ => Shape::error_with_range(
            format!("Method ->{} requires an object input", method_name.as_ref()),
            method_name.range(),
        ),
    }
}
