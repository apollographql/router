use shape::ShapeCase;

pub(crate) fn is_comparable_shape_combination(shape1: &ShapeCase, shape2: &ShapeCase) -> bool {
    matches!(
        (shape1, shape2),
        // Unknown and Name are compatible with comparable types
        (ShapeCase::Unknown | ShapeCase::Name(_, _), ShapeCase::Int(_) | ShapeCase::Float | ShapeCase::String(_) | ShapeCase::Unknown | ShapeCase::Name(_, _)) |
          (ShapeCase::Int(_) | ShapeCase::Float | ShapeCase::String(_), ShapeCase::Unknown | ShapeCase::Name(_, _)) |
          // Numeric types are cross-compatible
          (ShapeCase::Int(_), ShapeCase::Int(_) | ShapeCase::Float) |
          (ShapeCase::Float, ShapeCase::Int(_) | ShapeCase::Float) |
          // String types
          (ShapeCase::String(_), ShapeCase::String(_))
    )
}

#[cfg(test)]
mod tests {
    use shape::ShapeCase;
    use shape::location::Located;

    use super::*;

    #[rstest::rstest]
    #[case(&ShapeCase::Unknown, &ShapeCase::Unknown, true)]
    #[case(&ShapeCase::Int(Some(1)), &ShapeCase::Int(Some(1)), true)]
    #[case(&ShapeCase::Int(Some(1)), &ShapeCase::Unknown, true)]
    #[case(&ShapeCase::Unknown, &ShapeCase::Int(Some(1)), true)]
    #[case(&ShapeCase::Float, &ShapeCase::Float, true)]
    #[case(&ShapeCase::Float, &ShapeCase::Unknown, true)]
    #[case(&ShapeCase::Unknown, &ShapeCase::Float, true)]
    #[case(&ShapeCase::Int(Some(1)), &ShapeCase::Float, true)]
    #[case(&ShapeCase::Float, &ShapeCase::Int(Some(1)), true)]
    #[case(&ShapeCase::String(Some("a".to_string())), &ShapeCase::String(Some("a".to_string())), true)]
    #[case(&ShapeCase::String(Some("a".to_string())), &ShapeCase::Unknown, true)]
    #[case(&ShapeCase::Unknown, &ShapeCase::String(Some("a".to_string())), true)]
    #[case(&ShapeCase::Name(Located::new("name".to_string(), []), vec![]), &ShapeCase::Name(Located::new("name".to_string(), []), vec![]), true)]
    #[case(&ShapeCase::Name(Located::new("name".to_string(), []), vec![]), &ShapeCase::Unknown, true)]
    #[case(&ShapeCase::Unknown, &ShapeCase::Name(Located::new("name".to_string(), []), vec![]), true)]
    #[case(&ShapeCase::Name(Located::new("name".to_string(), []), vec![]), &ShapeCase::Int(Some(1)), true)]
    #[case(&ShapeCase::Int(Some(1)), &ShapeCase::Name(Located::new("name".to_string(), []), vec![]), true)]
    #[case(&ShapeCase::Name(Located::new("name".to_string(), []), vec![]), &ShapeCase::Float, true)]
    #[case(&ShapeCase::Float, &ShapeCase::Name(Located::new("name".to_string(), []), vec![]), true)]
    #[case(&ShapeCase::Name(Located::new("name".to_string(), []), vec![]), &ShapeCase::String(Some("a".to_string())), true)]
    #[case(&ShapeCase::String(Some("a".to_string())), &ShapeCase::Name(Located::new("name".to_string(), []), vec![]), true)]
    fn test_is_comparable_shape_combination_positive_cases(
        #[case] shape1: &ShapeCase,
        #[case] shape2: &ShapeCase,
        #[case] expected: bool,
    ) {
        let actual = is_comparable_shape_combination(shape1, shape2);
        assert_eq!(expected, actual);
    }

    #[rstest::rstest]
    #[case(&ShapeCase::Int(Some(1)), &ShapeCase::String(Some("a".to_string())), false)]
    #[case(&ShapeCase::String(Some("a".to_string())), &ShapeCase::Int(Some(1)), false)]
    #[case(&ShapeCase::Int(Some(1)), &ShapeCase::Bool(Some(true)), false)]
    #[case(&ShapeCase::Bool(Some(true)), &ShapeCase::Int(Some(1)), false)]
    #[case(&ShapeCase::Float, &ShapeCase::String(Some("a".to_string())), false)]
    #[case(&ShapeCase::String(Some("a".to_string())), &ShapeCase::Float, false)]
    #[case(&ShapeCase::Float, &ShapeCase::Bool(Some(true)), false)]
    #[case(&ShapeCase::Bool(Some(true)), &ShapeCase::Float, false)]
    #[case(&ShapeCase::String(Some("a".to_string())), &ShapeCase::Bool(Some(true)), false)]
    #[case(&ShapeCase::Bool(Some(true)), &ShapeCase::String(Some("a".to_string())), false)]
    #[case(&ShapeCase::Int(Some(1)), &ShapeCase::Null, false)]
    #[case(&ShapeCase::Null, &ShapeCase::Int(Some(1)), false)]
    #[case(&ShapeCase::Float, &ShapeCase::Null, false)]
    #[case(&ShapeCase::Null, &ShapeCase::Float, false)]
    #[case(&ShapeCase::String(Some("a".to_string())), &ShapeCase::Null, false)]
    #[case(&ShapeCase::Null, &ShapeCase::String(Some("a".to_string())), false)]
    #[case(&ShapeCase::Bool(Some(true)), &ShapeCase::Null, false)]
    #[case(&ShapeCase::Name(Located::new("name".to_string(), []), vec![]), &ShapeCase::Bool(Some(true)), false)]
    #[case(&ShapeCase::Bool(Some(true)), &ShapeCase::Name(Located::new("name".to_string(), []), vec![]), false)]
    #[case(&ShapeCase::Name(Located::new("name".to_string(), []), vec![]), &ShapeCase::Null, false)]
    #[case(&ShapeCase::Null, &ShapeCase::Name(Located::new("name".to_string(), []), vec![]), false)]
    #[case(&ShapeCase::Null, &ShapeCase::Bool(Some(true)), false)]
    fn test_is_comparable_shape_combination_negative_cases(
        #[case] shape1: &ShapeCase,
        #[case] shape2: &ShapeCase,
        #[case] expected: bool,
    ) {
        let actual = is_comparable_shape_combination(shape1, shape2);
        assert_eq!(expected, actual);
    }
}
