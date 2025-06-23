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
    #[case(Shape::float([]), Shape::float([]), true)]
    #[case(Shape::float([]), Shape::unknown([]), true)]
    #[case(Shape::float([]), Shape::name("test", []), true)]
    #[case(Shape::float([]), Shape::int([]), true)]
    #[case(Shape::string([]), Shape::string([]), true)]
    #[case(Shape::string([]), Shape::unknown([]), true)]
    #[case(Shape::string([]), Shape::name("test", []), true)]
    #[case(Shape::unknown([]), Shape::float([]), true)]
    #[case(Shape::unknown([]), Shape::int([]), true)]
    #[case(Shape::unknown([]), Shape::string([]), true)]
    #[case(Shape::unknown([]), Shape::unknown([]), true)]
    #[case(Shape::unknown([]), Shape::name("test", []), true)]
    #[case(Shape::name("test", []), Shape::float([]), true)]
    #[case(Shape::name("test", []), Shape::string([]), true)]
    #[case(Shape::name("test", []), Shape::unknown([]), true)]
    #[case(Shape::name("test", []), Shape::int([]), true)]
    #[case(Shape::name("test", []), Shape::name("test", []), true)]
    #[case(Shape::int([]), Shape::float([]), true)]
    #[case(Shape::int([]), Shape::int([]), true)]
    #[case(Shape::int([]), Shape::name("test", []), true)]
    #[case(Shape::int([]), Shape::unknown([]), true)]
    fn test_is_comparable_shape_combination_2_positive_cases(
        #[case] shape1: Shape,
        #[case] shape2: Shape,
        #[case] expected: bool,
    ) {
        let actual = is_comparable_shape_combination(&shape1, &shape2);
        assert_eq!(expected, actual);
    }

    #[rstest::rstest]
    #[case(Shape::string([]), Shape::int([]), false)]
    #[case(Shape::string([]), Shape::bool([]), false)]
    #[case(Shape::string([]), Shape::null([]), false)]
    #[case(Shape::string([]), Shape::float([]), false)]
    #[case(Shape::float([]), Shape::bool([]), false)]
    #[case(Shape::float([]), Shape::null([]), false)]
    #[case(Shape::float([]), Shape::string([]), false)]
    #[case(Shape::int([]), Shape::string([]), false)]
    #[case(Shape::int([]), Shape::bool([]), false)]
    #[case(Shape::int([]), Shape::null([]), false)]
    #[case(Shape::null([]), Shape::float([]), false)]
    #[case(Shape::null([]), Shape::int([]), false)]
    #[case(Shape::null([]), Shape::null([]), false)]
    #[case(Shape::null([]), Shape::string([]), false)]
    #[case(Shape::null([]), Shape::unknown([]), false)]
    #[case(Shape::null([]), Shape::name("test", []), false)]
    #[case(Shape::name("test", []), Shape::bool([]), false)]
    #[case(Shape::name("test", []), Shape::null([]), false)]
    #[case(Shape::unknown([]), Shape::bool([]), false)]
    #[case(Shape::unknown([]), Shape::null([]), false)]
    #[case(Shape::bool([]), Shape::float([]), false)]
    #[case(Shape::bool([]), Shape::string([]), false)]
    #[case(Shape::bool([]), Shape::int([]), false)]
    #[case(Shape::bool([]), Shape::unknown([]), false)]
    #[case(Shape::bool([]), Shape::name("test", []), false)]
    fn test_is_comparable_shape_combination_2_negative_cases(
        #[case] shape1: Shape,
        #[case] shape2: Shape,
        #[case] expected: bool,
    ) {
        let actual = is_comparable_shape_combination(&shape1, &shape2);
        assert_eq!(expected, actual);
    }
}
