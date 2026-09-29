use serde_json::Number;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::ApplyToError;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::helpers::missing_element_as_null;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;

/// Returns true if `shape1` and `shape2` could both be numbers, or could both
/// be strings, so that methods like `->gt` could compare them.
pub(crate) fn is_comparable_shape_combination(shape1: &Shape, shape2: &Shape) -> bool {
    [Shape::float([]), Shape::string([])]
        .iter()
        .any(|kind| could_satisfy(kind, shape1) && could_satisfy(kind, shape2))
}

/// Returns true if some value of `shape` that is not `None` could satisfy
/// `contract`, judging only by the parts of `shape` that are known when method
/// shapes are computed.
///
/// Method shapes are computed before variables like `$args` are resolved, so a
/// shape can contain unbound named shapes anywhere inside it (for example
/// `List<$args.ids.*>` after `->map(@)`). `Shape::validate` rejects those, as
/// well as `Unknown`, even though they could turn out to be fine. It also
/// requires every member of a union to satisfy the contract, while a method
/// call can still succeed at runtime if any one of them does.
///
/// Array elements that may have no value are checked as `null`, since that is
/// what they are at runtime (see `missing_element_as_null`).
///
/// Shape functions use this to avoid denying a call that some combination of
/// argument values could make succeed at runtime.
pub(crate) fn could_satisfy(contract: &Shape, shape: &Shape) -> bool {
    if contract.validate(shape).is_none() {
        return true;
    }

    match shape.case() {
        ShapeCase::Name(name, weak) => {
            return weak
                .upgrade(name)
                .is_none_or(|named| could_satisfy(contract, &named));
        }
        ShapeCase::Unknown => return true,
        ShapeCase::None => return false,
        ShapeCase::One(members) => {
            return members
                .iter()
                .any(|member| !member.is_none() && could_satisfy(contract, member));
        }
        // A value of an intersection has the shape of every member, so it
        // satisfies the contract if any member does.
        ShapeCase::All(members) => {
            return members.iter().any(|member| could_satisfy(contract, member));
        }
        ShapeCase::Error(shape::Error {
            partial: Some(partial),
            ..
        }) => return could_satisfy(contract, partial),
        _ => {}
    }

    match (contract.case(), shape.case()) {
        (ShapeCase::Name(name, weak), _) => weak
            .upgrade(name)
            .is_none_or(|named| could_satisfy(&named, shape)),
        (ShapeCase::One(members), _) => members.iter().any(|member| could_satisfy(member, shape)),
        (ShapeCase::All(members), _) => members.iter().all(|member| could_satisfy(member, shape)),
        (
            ShapeCase::Array {
                prefix: contract_prefix,
                tail: contract_tail,
            },
            ShapeCase::Array { prefix, tail },
        ) => {
            let items_could = (0..contract_prefix.len().max(prefix.len())).all(|i| {
                let expected = match contract_prefix.get(i) {
                    Some(expected) => expected,
                    // The contract has no expectations past its prefix.
                    None if contract_tail.is_none() => return true,
                    None => contract_tail,
                };
                let received = match prefix.get(i) {
                    // At runtime, an array element with no value is `null`.
                    Some(item) => missing_element_as_null(item),
                    // A `None` tail means there are no more elements, so there
                    // is no element here to satisfy the contract.
                    None if tail.is_none() => tail.clone(),
                    // Otherwise any element here has the tail's shape.
                    None => missing_element_as_null(tail),
                };
                could_satisfy(expected, &received)
            });
            items_could
                && (contract_tail.is_none()
                    || tail.is_none()
                    || could_satisfy(contract_tail, &missing_element_as_null(tail)))
        }
        (
            ShapeCase::Object {
                fields: contract_fields,
                rest: contract_rest,
            },
            ShapeCase::Object { fields, rest },
        ) => {
            let fields_could = contract_fields.iter().all(|(name, expected)| {
                fields
                    .get(name)
                    .is_none_or(|received| could_satisfy(expected, received))
            });
            fields_could
                && (contract_rest.is_none() || rest.is_none() || could_satisfy(contract_rest, rest))
        }
        // `shape` is known here, and `validate` has already rejected it.
        _ => false,
    }
}

/// Returns the part of `shape` that is not `None`, or `None` if `shape` is
/// always `None` (missing).
///
/// At runtime, most `->` methods stop and produce no value, without an error,
/// when one of their arguments produces no value. Shape logic mirrors that by
/// checking only the present part of an argument shape, and adding `None` to
/// the result shape (see [`or_missing`]) when the argument may be missing.
///
/// Related helpers: [`present_arg`] is this for an argument, keeping result
/// shapes unchanged before `connect/v0.5`, and [`compared_element`] is this for
/// an array element compared by `->in` or `->contains`, also skipping `null`.
pub(crate) fn present_part(shape: &Shape) -> Option<Shape> {
    match shape.case() {
        ShapeCase::None => None,
        ShapeCase::One(members) if members.iter().any(Shape::is_none) => Some(Shape::one(
            members.iter().filter(|member| !member.is_none()).cloned(),
            shape.locations().cloned(),
        )),
        _ => Some(shape.clone()),
    }
}

/// Returns the part of an array element shape that `->in` and `->contains`
/// should check against the value they look for, or `None` to skip it.
///
/// At runtime these methods compare with `==` and never report an error for
/// an element of another type, so the same-type check only catches likely
/// mistakes. An element with no value is skipped, and a `null` element simply
/// doesn't match. From `connect/v0.5`, an array literal element with no value
/// has the shape `null` (see `missing_as_null`), so skipping `null` also keeps
/// those elements accepted there.
pub(crate) fn compared_element(shape: &Shape) -> Option<Shape> {
    present_part(shape).filter(|shape| !shape.is_null())
}

/// Like [`present_part`], for the argument of a method that produces no value
/// when that argument has none.
///
/// From `connect/v0.5`, an argument that is always missing gives `None`, and
/// the method should return `Shape::none()`. Before that, the method's result
/// shape must stay what it was, so an always-missing argument is treated as
/// `Unknown` instead. That way the call is accepted without changing its
/// result shape.
pub(crate) fn present_arg(context: &ShapeContext, shape: &Shape) -> Option<Shape> {
    present_part(shape).or_else(|| {
        (context.spec() < ConnectSpec::V0_5).then(|| Shape::unknown(shape.locations().cloned()))
    })
}

/// Carries the errors anywhere in `arg_shapes` over to `result`, for the
/// arguments of a `->` method call (see `ShapeContext::compute_method_shape`).
///
/// [`could_satisfy`] looks through an error to its `partial` shape, so an
/// argument like `$(1)->gt("x")` (an error with a `Bool` partial) passes a
/// method's argument check, and a method that builds its result without the
/// argument's shape would otherwise drop the error. Errors are collected from
/// error chains, union and intersection members, array elements and object
/// fields, like `$(true)->match([true, $(1)->gt("x")])`, which has the shape
/// `One<Error<Bool>, None>`, or `[$(1)->gt("x")]`.
///
/// Each error becomes a layer around `result`, first error outermost, and
/// errors `result` already contains (from methods like `->echo`, whose result
/// includes the argument's shape) are skipped.
pub(crate) fn with_arg_errors<'a>(
    arg_shapes: impl IntoIterator<Item = &'a Shape>,
    result: Shape,
) -> Shape {
    let mut errors = Vec::new();
    for arg_shape in arg_shapes {
        collect_errors(arg_shape, &mut errors);
    }
    if errors.is_empty() {
        return result;
    }

    let mut existing = Vec::new();
    collect_errors(&result, &mut existing);
    errors
        .into_iter()
        .rev()
        .filter(|error| !existing.contains(error))
        .fold(result, |result, error| match error.case() {
            ShapeCase::Error(shape::Error { message, .. }) => {
                Shape::error_with_partial(message.clone(), result, error.locations().cloned())
            }
            _ => result,
        })
}

/// Adds each distinct error anywhere in `shape` to `errors`, as an error shape
/// with no partial. Errors are compared by message, since shapes compare
/// equal regardless of their locations. `Name` references are not followed,
/// so an error only reachable through one is not collected.
fn collect_errors(shape: &Shape, errors: &mut Vec<Shape>) {
    match shape.case() {
        ShapeCase::Error(shape::Error { message, partial }) => {
            let error = Shape::error(message.clone(), shape.locations().cloned());
            if !errors.contains(&error) {
                errors.push(error);
            }
            if let Some(partial) = partial {
                collect_errors(partial, errors);
            }
        }
        ShapeCase::One(members) => members
            .iter()
            .for_each(|member| collect_errors(member, errors)),
        ShapeCase::All(members) => members
            .iter()
            .for_each(|member| collect_errors(member, errors)),
        ShapeCase::Array { prefix, tail } => {
            prefix.iter().for_each(|item| collect_errors(item, errors));
            collect_errors(tail, errors);
        }
        ShapeCase::Object { fields, rest } => {
            fields
                .values()
                .for_each(|field| collect_errors(field, errors));
            collect_errors(rest, errors);
        }
        _ => {}
    }
}

/// Returns true if `shape` is `None` or a union that includes `None`.
pub(crate) fn may_be_missing(shape: &Shape) -> bool {
    match shape.case() {
        ShapeCase::None => true,
        ShapeCase::One(members) => members.iter().any(Shape::is_none),
        _ => false,
    }
}

/// Adds `None` to `result` if `maybe_missing`, for methods that produce no
/// value when an argument may be missing.
///
/// This changes the result shape of expressions that were already valid, which
/// can make them fail where an exact shape is expected (like a `Bool` for
/// `isSuccess`), so it only applies from `connect/v0.5`.
pub(crate) fn or_missing(context: &ShapeContext, result: Shape, maybe_missing: bool) -> Shape {
    if maybe_missing && context.spec() >= ConnectSpec::V0_5 {
        Shape::one([result, Shape::none()], [])
    } else {
        result
    }
}

/// Returns true if values of shapes `a` and `b` can be meaningfully compared
/// with methods like `->eq`, meaning they have the same type, where an int can
/// be compared with a float and unknown or named shapes with anything.
///
/// Literal values are compared by their type, not their value: comparing
/// `"a"` with `"b"` is a valid comparison that happens to return false.
///
/// For unions, it is enough that some member of `a` could be compared with some
/// member of `b`, since those values could meet at runtime.
pub(crate) fn is_same_type_comparison(a: &Shape, b: &Shape) -> bool {
    fn present_members(shape: &Shape) -> Vec<Shape> {
        match shape.case() {
            ShapeCase::One(members) => members
                .iter()
                .filter(|member| !member.is_none())
                .map(widen_literals)
                .collect(),
            _ => vec![widen_literals(shape)],
        }
    }

    let b_members = present_members(b);
    present_members(a)
        .iter()
        .any(|a| b_members.iter().any(|b| a.accepts(b) || b.accepts(a)))
}

/// Replaces literal string, int, and bool shapes (like `"a"`, `1`, or `true`)
/// anywhere in `shape` with the general shape of their type.
fn widen_literals(shape: &Shape) -> Shape {
    let locations = shape.locations().cloned();
    match shape.case() {
        ShapeCase::String(Some(_)) => Shape::string(locations),
        ShapeCase::Int(Some(_)) => Shape::int(locations),
        ShapeCase::Bool(Some(_)) => Shape::bool(locations),
        ShapeCase::One(members) => Shape::one(members.iter().map(widen_literals), locations),
        ShapeCase::All(members) => Shape::all(members.iter().map(widen_literals), locations),
        ShapeCase::Array { prefix, tail } => Shape::array(
            prefix.iter().map(widen_literals),
            widen_literals(tail),
            locations,
        ),
        ShapeCase::Object { fields, rest } => Shape::object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), widen_literals(field)))
                .collect(),
            widen_literals(rest),
            locations,
        ),
        _ => shape.clone(),
    }
}

pub(crate) fn number_value_as_float(
    number: &Number,
    method_name: &WithRange<String>,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> Result<f64, ApplyToError> {
    match number.as_f64() {
        Some(val) => Ok(val),
        None => {
            // Note that we don't have tests for these `None` cases because I can't actually find a case where this ever actually fails
            // It seems that the current implementation in serde_json always returns a value
            Err(ApplyToError::new(
                format!(
                    "Method ->{} fail to convert applied to value to float.",
                    method_name.as_ref(),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case(Shape::float([]), Shape::float([]))]
    #[case(Shape::float([]), Shape::unknown([]))]
    #[case(Shape::float([]), Shape::name("test", []))]
    #[case(Shape::float([]), Shape::int([]))]
    #[case(Shape::string([]), Shape::string([]))]
    #[case(Shape::string([]), Shape::unknown([]))]
    #[case(Shape::string([]), Shape::name("test", []))]
    #[case(Shape::unknown([]), Shape::float([]))]
    #[case(Shape::unknown([]), Shape::int([]))]
    #[case(Shape::unknown([]), Shape::string([]))]
    #[case(Shape::unknown([]), Shape::unknown([]))]
    #[case(Shape::unknown([]), Shape::name("test", []))]
    #[case(Shape::name("test", []), Shape::float([]))]
    #[case(Shape::name("test", []), Shape::string([]))]
    #[case(Shape::name("test", []), Shape::unknown([]))]
    #[case(Shape::name("test", []), Shape::int([]))]
    #[case(Shape::name("test", []), Shape::name("test", []))]
    #[case(Shape::int([]), Shape::float([]))]
    #[case(Shape::int([]), Shape::int([]))]
    #[case(Shape::int([]), Shape::name("test", []))]
    #[case(Shape::int([]), Shape::unknown([]))]
    #[case(Shape::one([Shape::string([])], []), Shape::one([Shape::string([])], []))]
    fn test_is_comparable_shape_combination_positive_cases(
        #[case] shape1: Shape,
        #[case] shape2: Shape,
    ) {
        assert!(is_comparable_shape_combination(&shape1, &shape2));
    }

    #[rstest::rstest]
    #[case(Shape::string([]), Shape::int([]))]
    #[case(Shape::string([]), Shape::bool([]))]
    #[case(Shape::string([]), Shape::null([]))]
    #[case(Shape::string([]), Shape::float([]))]
    #[case(Shape::string([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::string([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::float([]), Shape::bool([]))]
    #[case(Shape::float([]), Shape::null([]))]
    #[case(Shape::float([]), Shape::string([]))]
    #[case(Shape::float([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::float([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::int([]), Shape::string([]))]
    #[case(Shape::int([]), Shape::bool([]))]
    #[case(Shape::int([]), Shape::null([]))]
    #[case(Shape::int([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::int([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::null([]), Shape::float([]))]
    #[case(Shape::null([]), Shape::int([]))]
    #[case(Shape::null([]), Shape::null([]))]
    #[case(Shape::null([]), Shape::string([]))]
    #[case(Shape::null([]), Shape::unknown([]))]
    #[case(Shape::null([]), Shape::name("test", []))]
    #[case(Shape::null([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::null([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::name("test", []), Shape::bool([]))]
    #[case(Shape::name("test", []), Shape::null([]))]
    #[case(Shape::name("test", []), Shape::list(Shape::string([]), []))]
    #[case(Shape::name("test", []), Shape::dict(Shape::string([]), []))]
    #[case(Shape::unknown([]), Shape::bool([]))]
    #[case(Shape::unknown([]), Shape::null([]))]
    #[case(Shape::unknown([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::unknown([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::list(Shape::string([]), []), Shape::string([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::float([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::int([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::bool([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::null([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::unknown([]))]
    #[case(Shape::list(Shape::string([]), []), Shape::name("test", []))]
    #[case(Shape::list(Shape::string([]), []), Shape::dict(Shape::string([]), []))]
    #[case(Shape::dict(Shape::string([]), []), Shape::string([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::float([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::int([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::bool([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::null([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::unknown([]))]
    #[case(Shape::dict(Shape::string([]), []), Shape::name("test", []))]
    #[case(Shape::dict(Shape::string([]), []), Shape::list(Shape::string([]), []))]
    #[case(Shape::bool([]), Shape::float([]))]
    #[case(Shape::bool([]), Shape::string([]))]
    #[case(Shape::bool([]), Shape::int([]))]
    #[case(Shape::bool([]), Shape::unknown([]))]
    #[case(Shape::bool([]), Shape::name("test", []))]
    #[case(Shape::bool([]), Shape::list(Shape::string([]), []))]
    #[case(Shape::bool([]), Shape::dict(Shape::string([]), []))]
    #[case(Shape::one([Shape::string([])], []), Shape::one([Shape::int([])], []))]
    fn test_is_comparable_shape_combination_negative_cases(
        #[case] shape1: Shape,
        #[case] shape2: Shape,
    ) {
        assert!(!is_comparable_shape_combination(&shape1, &shape2));
    }

    fn scalar_list() -> Shape {
        Shape::list(
            Shape::one([Shape::string([]), Shape::int([]), Shape::null([])], []),
            [],
        )
    }

    fn record(field: &str, shape: Shape) -> Shape {
        Shape::record([(field.to_string(), shape)].into_iter().collect(), [])
    }

    #[rstest::rstest]
    #[case::exact(Shape::string([]), Shape::string([]))]
    #[case::named(Shape::string([]), Shape::name("$args.s", []))]
    #[case::unknown(Shape::string([]), Shape::unknown([]))]
    #[case::named_or_string(
        Shape::string([]),
        Shape::one([Shape::name("$args.s", []), Shape::string([])], [])
    )]
    #[case::list_of_named(scalar_list(), Shape::list(Shape::name("$args.ids.*", []), []))]
    #[case::list_of_unknown(scalar_list(), Shape::list(Shape::unknown([]), []))]
    #[case::tuple_with_named(
        scalar_list(),
        Shape::tuple([Shape::string([]), Shape::name("$args.id", [])], [])
    )]
    #[case::union_contract(
        Shape::one([Shape::string([]), scalar_list()], []),
        Shape::list(Shape::name("$args.ids.*", []), [])
    )]
    #[case::union_on_both_sides(
        Shape::one([Shape::string([]), scalar_list()], []),
        Shape::one(
            [Shape::string([]), Shape::list(Shape::name("$args.ids.*", []), [])],
            []
        )
    )]
    #[case::record_with_named_field(
        record("a", Shape::string([])),
        record("a", Shape::name("$args.a", []))
    )]
    #[case::maybe_missing(
        Shape::string([]),
        Shape::one([Shape::string([]), Shape::none()], [])
    )]
    #[case::named_or_object(
        Shape::string([]),
        Shape::one(
            [Shape::name("$args.s", []), Shape::dict(Shape::string([]), [])],
            []
        )
    )]
    #[case::some_union_member(
        Shape::string([]),
        Shape::one([Shape::int([]), Shape::string([])], [])
    )]
    #[case::list_of_string_or_object(
        scalar_list(),
        Shape::list(
            Shape::one([Shape::string([]), Shape::empty_object([])], []),
            []
        )
    )]
    // An array element with no value is `null` at runtime.
    #[case::tuple_with_missing_element(
        scalar_list(),
        Shape::tuple([Shape::none(), Shape::string([])], [])
    )]
    #[case::list_of_maybe_missing(
        scalar_list(),
        Shape::list(Shape::one([Shape::string([]), Shape::none()], []), [])
    )]
    fn test_could_satisfy_positive_cases(#[case] contract: Shape, #[case] shape: Shape) {
        assert!(could_satisfy(&contract, &shape));
    }

    #[rstest::rstest]
    #[case::wrong_scalar(Shape::string([]), Shape::int([]))]
    #[case::none(Shape::string([]), Shape::none())]
    #[case::no_union_member(
        Shape::string([]),
        Shape::one([Shape::int([]), Shape::bool([]), Shape::none()], [])
    )]
    #[case::list_for_string(
        Shape::string([]),
        Shape::list(Shape::name("$args.ids.*", []), [])
    )]
    #[case::object_for_string(Shape::string([]), Shape::dict(Shape::name("$args.v", []), []))]
    #[case::list_of_objects(scalar_list(), Shape::list(Shape::dict(Shape::string([]), []), []))]
    #[case::list_of_lists(
        scalar_list(),
        Shape::list(Shape::list(Shape::name("$args.ids.*", []), []), [])
    )]
    #[case::tuple_with_object(
        scalar_list(),
        Shape::tuple([Shape::name("$args.id", []), Shape::empty_object([])], [])
    )]
    #[case::union_contract(
        Shape::one([Shape::string([]), scalar_list()], []),
        Shape::list(Shape::empty_object([]), [])
    )]
    #[case::record_with_wrong_field(
        record("a", Shape::string([])),
        record("a", Shape::bool([]))
    )]
    fn test_could_satisfy_negative_cases(#[case] contract: Shape, #[case] shape: Shape) {
        assert!(!could_satisfy(&contract, &shape));
    }

    #[rstest::rstest]
    #[case::same_string_literals(Shape::string_value("a", []), Shape::string_value("a", []))]
    #[case::different_string_literals(Shape::string_value("a", []), Shape::string_value("b", []))]
    #[case::string_literal_and_string(Shape::string_value("a", []), Shape::string([]))]
    #[case::different_int_literals(Shape::int_value(1, []), Shape::int_value(2, []))]
    #[case::int_literal_and_float(Shape::int_value(1, []), Shape::float([]))]
    #[case::different_bool_literals(Shape::bool_value(true, []), Shape::bool_value(false, []))]
    #[case::literal_union(
        Shape::one([Shape::string_value("x", []), Shape::string_value("y", [])], []),
        Shape::string_value("z", [])
    )]
    #[case::nested_literals(
        Shape::tuple([Shape::string_value("a", [])], []),
        Shape::tuple([Shape::string_value("b", [])], [])
    )]
    #[case::named(Shape::string_value("a", []), Shape::name("$args.s", []))]
    fn test_is_same_type_comparison_positive_cases(#[case] a: Shape, #[case] b: Shape) {
        assert!(is_same_type_comparison(&a, &b));
        assert!(is_same_type_comparison(&b, &a));
    }

    #[rstest::rstest]
    #[case::string_and_int(Shape::string_value("a", []), Shape::int_value(1, []))]
    #[case::bool_and_string(Shape::bool_value(true, []), Shape::string([]))]
    #[case::string_and_null(Shape::string_value("a", []), Shape::null([]))]
    #[case::nested_mismatch(
        Shape::tuple([Shape::string_value("a", [])], []),
        Shape::tuple([Shape::int_value(1, [])], [])
    )]
    fn test_is_same_type_comparison_negative_cases(#[case] a: Shape, #[case] b: Shape) {
        assert!(!is_same_type_comparison(&a, &b));
        assert!(!is_same_type_comparison(&b, &a));
    }

    #[rstest::rstest]
    #[case::missing(Shape::none())]
    #[case::null(Shape::null([]))]
    #[case::missing_or_null(Shape::one([Shape::none(), Shape::null([])], []))]
    fn test_compared_element_skips(#[case] element: Shape) {
        assert_eq!(compared_element(&element), None);
    }

    #[rstest::rstest]
    #[case::string(Shape::string([]), Shape::string([]))]
    #[case::maybe_missing(
        Shape::one([Shape::string([]), Shape::none()], []),
        Shape::string([])
    )]
    #[case::maybe_null(
        Shape::one([Shape::string([]), Shape::null([])], []),
        Shape::one([Shape::string([]), Shape::null([])], [])
    )]
    fn test_compared_element_keeps(#[case] element: Shape, #[case] expected: Shape) {
        assert_eq!(compared_element(&element), Some(expected));
    }

    fn arg_error(message: &str) -> Shape {
        Shape::error_with_partial(message, Shape::bool([]), [])
    }

    #[rstest::rstest]
    #[case::no_error(Shape::bool([]), vec![])]
    #[case::top_level(arg_error("a"), vec!["a"])]
    #[case::union_member(Shape::one([arg_error("a"), Shape::none()], []), vec!["a"])]
    #[case::intersection_member(
        Shape::all([Shape::bool([]), arg_error("a")], []),
        vec!["a"]
    )]
    #[case::several_members(
        Shape::one([arg_error("a"), Shape::bool([]), arg_error("b")], []),
        vec!["a", "b"]
    )]
    #[case::nested(
        Shape::error_with_partial("a", Shape::one([arg_error("b"), Shape::none()], []), []),
        vec!["a", "b"]
    )]
    #[case::array_element(Shape::tuple([Shape::bool([]), arg_error("a")], []), vec!["a"])]
    #[case::array_tail(Shape::list(arg_error("a"), []), vec!["a"])]
    #[case::object_field(
        Shape::record([("f".to_string(), arg_error("a"))].into_iter().collect(), []),
        vec!["a"]
    )]
    #[case::repeated(Shape::tuple([arg_error("a"), arg_error("a")], []), vec!["a"])]
    fn test_with_arg_errors(#[case] arg_shape: Shape, #[case] expected: Vec<&str>) {
        let (messages, result) = unwrap_errors(with_arg_errors([&arg_shape], Shape::string([])));
        assert_eq!(messages, expected);
        assert_eq!(
            result,
            Shape::string([]),
            "the result is the innermost partial"
        );
    }

    #[test]
    fn test_with_arg_errors_several_args() {
        let args = [arg_error("a"), Shape::bool([]), arg_error("b")];
        let (messages, _) = unwrap_errors(with_arg_errors(&args, Shape::string([])));
        assert_eq!(messages, vec!["a", "b"]);
    }

    // Errors the result already contains, like the argument shape `->echo`
    // returns, are not added again.
    #[rstest::rstest]
    #[case::same_shape(arg_error("a"), arg_error("a"))]
    #[case::inside_result(
        arg_error("a"),
        Shape::one([Shape::string([]), arg_error("a")], [])
    )]
    fn test_with_arg_errors_skips_existing(#[case] arg_shape: Shape, #[case] result: Shape) {
        assert_eq!(with_arg_errors([&arg_shape], result.clone()), result);
    }

    // Argument errors are added around a method's result only when the result
    // doesn't already contain them, and only once. `chain` is the number of
    // errors around the innermost result, which for `->echo` is the echoed
    // argument's own error.
    #[rstest::rstest]
    #[case::echo(r#"$->echo($(1)->gt("x"))"#, 1)]
    #[case::echo_array(r#"$->echo([$(1)->gt("x")])"#, 0)]
    #[case::match_value(r#"$(1)->match([1, $(1)->gt("x")], [@, true])"#, 0)]
    #[case::map_body(r#"$([1])->map($(1)->gt("x"))"#, 0)]
    #[case::nested_echo(r#"$->echo($->echo($(1)->gt("x")))"#, 1)]
    #[case::and(r#"$(true)->and($(1)->gt("x"))"#, 1)]
    #[case::and_nested(r#"$(true)->and($(true)->and($(1)->gt("x")))"#, 1)]
    #[case::and_repeated_arg(r#"$(true)->and($(1)->gt("x"), $(1)->gt("x"))"#, 1)]
    fn test_argument_errors_are_not_duplicated(#[case] selection: &str, #[case] chain: usize) {
        for spec in [ConnectSpec::V0_3, ConnectSpec::V0_4, ConnectSpec::V0_5] {
            let shape = crate::selection!(selection, spec).shape();
            let (messages, inner) = unwrap_errors(shape.clone());
            let mut inner_errors = vec![];
            collect_errors(&inner, &mut inner_errors);
            let inner_messages: Vec<_> = inner_errors
                .iter()
                .filter_map(|error| match error.case() {
                    ShapeCase::Error(shape::Error { message, .. }) => Some(message.clone()),
                    _ => None,
                })
                .collect();

            assert_eq!(messages.len(), chain, "{spec:?}: {}", shape.pretty_print());
            assert!(
                messages
                    .iter()
                    .all(|message| !inner_messages.contains(message)),
                "{spec:?}: an error around the result is also inside it: {}",
                shape.pretty_print()
            );
        }
    }

    /// Unwraps the chain of errors around a shape, returning their messages,
    /// outermost first, and the innermost partial.
    fn unwrap_errors(mut shape: Shape) -> (Vec<String>, Shape) {
        let mut messages = vec![];
        while let ShapeCase::Error(shape::Error { message, partial }) = shape.case() {
            messages.push(message.clone());
            shape = partial
                .clone()
                .expect("errors keep the result as a partial");
        }
        (messages, shape)
    }
}
