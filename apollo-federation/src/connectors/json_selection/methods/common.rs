use shape::Shape;

#[allow(dead_code)]
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
    #[case(Shape::float([]), Shape::bool([]))]
    #[case(Shape::float([]), Shape::null([]))]
    #[case(Shape::float([]), Shape::string([]))]
    #[case(Shape::int([]), Shape::string([]))]
    #[case(Shape::int([]), Shape::bool([]))]
    #[case(Shape::int([]), Shape::null([]))]
    #[case(Shape::null([]), Shape::float([]))]
    #[case(Shape::null([]), Shape::int([]))]
    #[case(Shape::null([]), Shape::null([]))]
    #[case(Shape::null([]), Shape::string([]))]
    #[case(Shape::null([]), Shape::unknown([]))]
    #[case(Shape::null([]), Shape::name("test", []))]
    #[case(Shape::name("test", []), Shape::bool([]))]
    #[case(Shape::name("test", []), Shape::null([]))]
    #[case(Shape::unknown([]), Shape::bool([]))]
    #[case(Shape::unknown([]), Shape::null([]))]
    #[case(Shape::bool([]), Shape::float([]))]
    #[case(Shape::bool([]), Shape::string([]))]
    #[case(Shape::bool([]), Shape::int([]))]
    #[case(Shape::bool([]), Shape::unknown([]))]
    #[case(Shape::bool([]), Shape::name("test", []))]
    fn test_is_comparable_shape_combination_negative_cases(
        #[case] shape1: Shape,
        #[case] shape2: Shape,
    ) {
        assert!(!is_comparable_shape_combination(&shape1, &shape2));
    }
}
