use std::iter::empty;

use apollo_compiler::collections::IndexMap;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;
use shape::location::SourceId;
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
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn echo_shape(
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
                // Return a singleton array wrapping the value of applying the
                // ->map method the non-array input data.
                return first_arg
                    .apply_to_path(data, vars, input_path)
                    .and_then_collecting_errors(|value| {
                        tail.apply_to_path(&JSON::Array(vec![value.clone()]), vars, input_path)
                    });
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
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn map_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    let Some(first_arg) = method_args.and_then(|args| args.args.first()) else {
        return Shape::error(
            format!("Method ->{} requires one argument", method_name.as_ref()),
            method_name.shape_location(source_id),
        );
    };
    match input_shape.case() {
        ShapeCase::Array { prefix, tail } => {
            let new_prefix = prefix
                .iter()
                .map(|shape| {
                    first_arg.compute_output_shape(
                        shape.clone(),
                        dollar_shape.clone(),
                        named_var_shapes,
                        source_id,
                    )
                })
                .collect::<Vec<_>>();
            let new_tail = first_arg.compute_output_shape(
                tail.clone(),
                dollar_shape.clone(),
                named_var_shapes,
                source_id,
            );
            Shape::array(new_prefix, new_tail, input_shape.locations)
        }
        _ => Shape::list(
            first_arg.compute_output_shape(
                input_shape.any_item([]),
                dollar_shape.clone(),
                named_var_shapes,
                source_id,
            ),
            input_shape.locations,
        ),
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
#[allow(dead_code)] // method type-checking disabled until we add name resolution
pub(super) fn match_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
    named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
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
                        source_id,
                    );
                    result_union.push(value_shape);
                }
            }
        }

        if !has_infallible_case {
            result_union.push(Shape::none());
        }

        if result_union.is_empty() {
            Shape::error(
                format!(
                    "Method ->{} requires at least one [candidate, value] pair",
                    method_name.as_ref(),
                ),
                merge_ranges(
                    method_name.range(),
                    method_args.and_then(|args| args.range()),
                )
                .map(|range| source_id.location(range)),
            )
        } else {
            Shape::one(result_union, method_name.shape_location(source_id))
        }
    } else {
        Shape::error(
            format!(
                "Method ->{} requires at least one [candidate, value] pair",
                method_name.as_ref(),
            ),
            method_name.shape_location(source_id),
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
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn first_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    let location = method_name.shape_location(source_id);
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
    let locations = input_shape.locations.iter().cloned().chain(location);

    match input_shape.case() {
        ShapeCase::String(Some(value)) => Shape::string_value(&value[0..1], locations),
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
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn last_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if method_args.is_some() {
        return Shape::error(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.shape_location(source_id),
        );
    }

    match input_shape.case() {
        ShapeCase::String(Some(value)) => {
            if let Some(last_char) = value.chars().last() {
                Shape::string_value(
                    last_char.to_string().as_str(),
                    method_name.shape_location(source_id),
                )
            } else {
                Shape::none()
            }
        }
        ShapeCase::String(None) => Shape::one(
            [
                Shape::string(method_name.shape_location(source_id)),
                Shape::none(),
            ],
            method_name.shape_location(source_id),
        ),
        ShapeCase::Array { prefix, tail } => {
            if tail.is_none() {
                if let Some(last) = prefix.last() {
                    last.clone()
                } else {
                    Shape::none()
                }
            } else if let Some(last) = prefix.last() {
                Shape::one(
                    [last.clone(), tail.clone(), Shape::none()],
                    method_name.shape_location(source_id),
                )
            } else {
                Shape::one(
                    [tail.clone(), Shape::none()],
                    method_name.shape_location(source_id),
                )
            }
        }
        ShapeCase::Name(_, _) => input_shape.any_item(method_name.shape_location(source_id)),
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
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn slice_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    mut input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
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
            Shape::array([], Shape::one(one_shapes, empty()), input_shape.locations)
        }
        ShapeCase::String(_) => Shape::string(input_shape.locations),
        ShapeCase::Name(_, _) => input_shape, // TODO: add a way to validate inputs after name resolution
        _ => Shape::error(
            format!(
                "Method ->{} requires an array or string input",
                method_name.as_ref()
            ),
            {
                input_shape
                    .locations
                    .extend(method_name.shape_location(source_id));
                input_shape.locations
            },
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
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn size_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    mut input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if method_args.is_some() {
        return Shape::error(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.shape_location(source_id),
        );
    }

    match input_shape.case() {
        ShapeCase::String(Some(value)) => {
            Shape::int_value(value.len() as i64, method_name.shape_location(source_id))
        }
        ShapeCase::String(None) => Shape::int(method_name.shape_location(source_id)),
        ShapeCase::Name(_, _) => Shape::int(method_name.shape_location(source_id)), // TODO: catch errors after name resolution
        ShapeCase::Array { prefix, tail } => {
            if tail.is_none() {
                Shape::int_value(prefix.len() as i64, method_name.shape_location(source_id))
            } else {
                Shape::int(method_name.shape_location(source_id))
            }
        }
        ShapeCase::Object { fields, rest, .. } => {
            if rest.is_none() {
                Shape::int_value(fields.len() as i64, method_name.shape_location(source_id))
            } else {
                Shape::int(method_name.shape_location(source_id))
            }
        }
        _ => Shape::error(
            format!(
                "Method ->{} requires an array, string, or object input",
                method_name.as_ref()
            ),
            {
                input_shape
                    .locations
                    .extend(method_name.shape_location(source_id));
                input_shape.locations
            },
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
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn entries_shape(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    mut input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    if method_args.is_some() {
        return Shape::error(
            format!(
                "Method ->{} does not take any arguments",
                method_name.as_ref()
            ),
            method_name.shape_location(source_id),
        );
    }

    match input_shape.case() {
        ShapeCase::Object { fields, rest, .. } => {
            let entry_shapes = fields
                .iter()
                .map(|(key, value)| {
                    let mut key_value_pair = Shape::empty_map();
                    key_value_pair.insert(
                        "key".to_string(),
                        Shape::string_value(key.as_str(), Vec::new()),
                    );
                    key_value_pair.insert("value".to_string(), value.clone());
                    Shape::object(
                        key_value_pair,
                        Shape::none(),
                        method_name.shape_location(source_id),
                    )
                })
                .collect::<Vec<_>>();

            if rest.is_none() {
                Shape::array(
                    entry_shapes,
                    rest.clone(),
                    method_name.shape_location(source_id),
                )
            } else {
                let mut tail_key_value_pair = Shape::empty_map();
                tail_key_value_pair.insert("key".to_string(), Shape::string(Vec::new()));
                tail_key_value_pair.insert("value".to_string(), rest.clone());
                Shape::array(
                    entry_shapes,
                    Shape::object(
                        tail_key_value_pair,
                        Shape::none(),
                        method_name.shape_location(source_id),
                    ),
                    method_name.shape_location(source_id),
                )
            }
        }
        ShapeCase::Name(_, _) => {
            let mut entries = Shape::empty_map();
            entries.insert("key".to_string(), Shape::string(Vec::new()));
            entries.insert("value".to_string(), input_shape.any_field(Vec::new()));
            Shape::list(
                Shape::object(
                    entries,
                    Shape::none(),
                    method_name.shape_location(source_id),
                ),
                method_name.shape_location(source_id),
            )
        }
        _ => Shape::error(
            format!("Method ->{} requires an object input", method_name.as_ref()),
            {
                input_shape
                    .locations
                    .extend(method_name.shape_location(source_id));
                input_shape.locations
            },
        ),
    }
}

impl_arrow_method!(
    JsonStringifyMethod,
    json_stringify_method,
    json_stringify_shape
);
fn json_stringify_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    _vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    _tail: &WithRange<PathList>,
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

    match serde_json::to_string(data) {
        Ok(val) => (Some(JSON::String(val.into())), vec![]),
        Err(err) => (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} failed to serialize JSON: {}",
                    method_name.as_ref(),
                    err
                ),
                input_path.to_vec(),
                method_name.range(),
            )],
        ),
    }
}
#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn json_stringify_shape(
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
    _named_var_shapes: &IndexMap<&str, Shape>,
    source_id: &SourceId,
) -> Shape {
    Shape::string(method_name.shape_location(source_id))
}
