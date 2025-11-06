use serde_json::Number;
use serde_json_bytes::Value as JSON;
use shape::Shape;

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
}
