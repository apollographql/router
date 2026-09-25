use serde_json::Number;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::ApplyToError;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;

pub(crate) fn is_comparable_shape_combination(shape1: &Shape, shape2: &Shape) -> bool {
    if Shape::float([]).accepts(shape1) {
        Shape::float([]).accepts(shape2) || shape2.accepts(&Shape::unknown([]))
    } else if Shape::string([]).accepts(shape1) {
        Shape::string([]).accepts(shape2) || shape2.accepts(&Shape::unknown([]))
    } else if shape1.accepts(&Shape::unknown([])) {
        Shape::float([]).accepts(shape2)
            || Shape::string([]).accepts(shape2)
            || shape2.accepts(&Shape::unknown([]))
    } else {
        false
    }
}

/// Returns true if `shape` cannot satisfy `contract`, judging only by the parts
/// of `shape` that are known when method shapes are computed.
///
/// Method shapes are computed before variables like `$args` are resolved, so an
/// input shape can contain unbound named shapes anywhere inside it (for
/// example `List<$args.ids.*>` after `->map(@)`). `Shape::validate` rejects
/// those, as well as `Unknown`, even though they could turn out to be fine.
/// This treats them as satisfying any contract, and reports a mismatch only
/// when a part of `shape` that is actually known cannot satisfy the
/// corresponding part of `contract`.
pub(crate) fn definitely_mismatches(contract: &Shape, shape: &Shape) -> bool {
    if contract.validate(shape).is_none() {
        return false;
    }

    match shape.case() {
        ShapeCase::Name(name, weak) => {
            return weak
                .upgrade(name)
                .is_some_and(|named| definitely_mismatches(contract, &named));
        }
        ShapeCase::Unknown => return false,
        // Every member of a union must be able to satisfy the contract.
        ShapeCase::One(members) => {
            return members
                .iter()
                .any(|member| definitely_mismatches(contract, member));
        }
        // An intersection satisfies the contract if any member does.
        ShapeCase::All(members) => {
            return members
                .iter()
                .all(|member| definitely_mismatches(contract, member));
        }
        ShapeCase::Error(shape::Error {
            partial: Some(partial),
            ..
        }) => return definitely_mismatches(contract, partial),
        _ => {}
    }

    match (contract.case(), shape.case()) {
        (ShapeCase::Name(name, weak), _) => weak
            .upgrade(name)
            .is_some_and(|named| definitely_mismatches(&named, shape)),
        // A union contract is satisfied if any of its members could be.
        (ShapeCase::One(members), _) => members
            .iter()
            .all(|member| definitely_mismatches(member, shape)),
        (ShapeCase::All(members), _) => members
            .iter()
            .any(|member| definitely_mismatches(member, shape)),
        (
            ShapeCase::Array {
                prefix: contract_prefix,
                tail: contract_tail,
            },
            ShapeCase::Array { prefix, tail },
        ) => {
            let items_mismatch = (0..contract_prefix.len().max(prefix.len())).any(|i| {
                let expected = match contract_prefix.get(i) {
                    Some(expected) => expected,
                    // The contract has no expectations past its prefix.
                    None if contract_tail.is_none() => return false,
                    None => contract_tail,
                };
                let received = prefix.get(i).unwrap_or(tail);
                definitely_mismatches(expected, received)
            });
            items_mismatch
                || (!contract_tail.is_none()
                    && !tail.is_none()
                    && definitely_mismatches(contract_tail, tail))
        }
        (
            ShapeCase::Object {
                fields: contract_fields,
                rest: contract_rest,
            },
            ShapeCase::Object { fields, rest },
        ) => {
            let fields_mismatch = contract_fields.iter().any(|(name, expected)| {
                fields
                    .get(name)
                    .is_some_and(|received| definitely_mismatches(expected, received))
            });
            fields_mismatch
                || (!contract_rest.is_none()
                    && !rest.is_none()
                    && definitely_mismatches(contract_rest, rest))
        }
        // `shape` is known here, and `validate` has already rejected it.
        _ => true,
    }
}

/// Returns the part of `shape` that is not `None`, or `None` if `shape` is
/// always `None` (missing).
///
/// At runtime, most `->` methods stop and produce no value, without an error,
/// when one of their arguments produces no value. Shape logic mirrors that by
/// checking only the present part of an argument shape, and adding `None` to
/// the result shape (see [`or_missing`]) when the argument may be missing.
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
pub(crate) fn or_missing(result: Shape, maybe_missing: bool) -> Shape {
    if maybe_missing {
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
pub(crate) fn is_same_type_comparison(a: &Shape, b: &Shape) -> bool {
    let (a, b) = (widen_literals(a), widen_literals(b));
    a.accepts(&b) || b.accepts(&a)
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
    fn test_definitely_mismatches_negative_cases(#[case] contract: Shape, #[case] shape: Shape) {
        assert!(!definitely_mismatches(&contract, &shape));
    }

    #[rstest::rstest]
    #[case::wrong_scalar(Shape::string([]), Shape::int([]))]
    #[case::none(Shape::string([]), Shape::none())]
    #[case::maybe_missing(
        Shape::string([]),
        Shape::one([Shape::string([]), Shape::none()], [])
    )]
    #[case::list_for_string(
        Shape::string([]),
        Shape::list(Shape::name("$args.ids.*", []), [])
    )]
    #[case::object_for_string(Shape::string([]), Shape::dict(Shape::name("$args.v", []), []))]
    #[case::named_or_object(
        Shape::string([]),
        Shape::one(
            [Shape::name("$args.s", []), Shape::dict(Shape::string([]), [])],
            []
        )
    )]
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
    fn test_definitely_mismatches_positive_cases(#[case] contract: Shape, #[case] shape: Shape) {
        assert!(definitely_mismatches(&contract, &shape));
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
}
