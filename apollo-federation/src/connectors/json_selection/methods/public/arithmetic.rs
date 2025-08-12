use apollo_compiler::collections::IndexSet;
use serde_json::Number;
use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::helpers::vec_push;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

/// This module exports a series of math functions (add, sub, mul, div, mod) which accept a number, and are applied
/// against a number, and returns the result of the operation.
///
/// Examples:
/// $->echo(5)->add(5)      results in 10
/// $->echo(5)->sub(1)      results in 4
/// $->echo(5)->mul(5)      results in 25
/// $->echo(5)->div(5)      results in 1.0 (division always returns a float)
/// $->echo(5)->mod(2)      results in 1
///
/// You can also chain multiple of the same operation together by providing a comma separated list of numbers:
/// $->echo(1)->add(2,3,4,5)      results in 15
pub(super) fn arithmetic_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    op: impl Fn(&Number, &Number) -> Option<Number>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if let Some(MethodArgs { args, .. }) = method_args {
        if let JSON::Number(result) = data {
            let mut result = result.clone();
            let mut errors = Vec::new();
            for arg in args {
                let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path, spec);
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
                                    spec,
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
                                spec,
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
                    spec,
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
                spec,
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
                infix_math_op!(@int_case a_i64, b_i64, $op)
            } else {
                None
            }
        }
    };
    // This branching is because when we are dividing, two whole numbers could result in a non-whole result (E.g. 7/2 = 3.5)
    // It is much more predictable and intuitive for this to return "3.5" instead of "3" (if we did int math) but we need
    // the return type to always be the same for type checking to work. So, for division specifically, we will always return
    // a float.
    (@int_case $a:ident, $b:ident, /) => {{
        Number::from_f64($a as f64 / $b as f64)
    }};
    (@int_case $a:ident, $b:ident, $op:tt) => {{
        Some(Number::from($a $op $b))
    }};
}
infix_math_op!(add_op, +);
infix_math_op!(sub_op, -);
infix_math_op!(mul_op, *);
infix_math_op!(div_op, /);
infix_math_op!(rem_op, %);

fn math_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let mut check_results = IndexSet::default();
    let input_result = check_numeric_shape(&input_shape);

    if let Some(result) = input_result {
        check_results.insert(result);
    } else {
        return Shape::error(
            format!(
                "Method ->{} received non-numeric input",
                method_name.as_ref()
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    if method_name.as_ref() == "div" {
        // The ->div method stays safe by always returning Float, so
        // check_result starts off false in that case.
        check_results.insert(CheckNumericResult::FloatPossible);
    }

    for (i, arg) in method_args
        .iter()
        .flat_map(|args| args.args.iter())
        .enumerate()
    {
        let arg_shape =
            arg.compute_output_shape(context, input_shape.clone(), dollar_shape.clone());

        if let Some(result) = check_numeric_shape(&arg_shape) {
            check_results.insert(result);
        } else {
            return Shape::error(
                format!(
                    "Method ->{} received non-numeric argument {}",
                    method_name.as_ref(),
                    i
                ),
                method_name.shape_location(context.source_id()),
            );
        }
    }

    if check_results
        .iter()
        .all(|result| matches!(result, CheckNumericResult::IntForSure))
    {
        // If all the shapes are definitely integers, math_shape can return Int.
        Shape::int(method_name.shape_location(context.source_id()))
    } else {
        // If any of the shapes are definitely floats, or could be floats,
        // we return a Float shape.
        Shape::float(method_name.shape_location(context.source_id()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CheckNumericResult {
    /// The shape is definitely an integer (general Int, specific 123 value, or
    /// union/intersection thereof).
    IntForSure,
    /// While we can't be sure the shape is an integer, it might still be a
    /// number. Note that Float contains all JSON number values, including all
    /// the integers. We report this result for Unknown and Name shapes as well,
    /// since they could resolve to a numeric value.
    FloatPossible,
}

fn check_numeric_shape(shape: &Shape) -> Option<CheckNumericResult> {
    // Using the `Shape::accepts` method automatically handles cases like shape
    // being a union or intersection.
    if Shape::int([]).accepts(shape) {
        Some(CheckNumericResult::IntForSure)
    } else if Shape::float([]).accepts(shape)
        // The only shapes that accept Unknown are Unknown and ShapeCase::Name
        // shapes, since their shape is logically unknown. It is otherwise
        // tricky to express a shape that accepts any ::Name shape, without
        // knowing the possible names in advance.
        || shape.accepts(&Shape::unknown([]))
    {
        // If shape meets the requirements of Float, or is an Unknown/Name shape
        // that might resolve to a numeric value, math_shape returns Float
        // (which is the same as saying "any numeric JSON value").
        Some(CheckNumericResult::FloatPossible)
    } else {
        // If there's no chance the shape could be a number (because we know
        // it's something else), math_shape will return an error.
        None
    }
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
            spec: ConnectSpec,
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            arithmetic_method(method_name, method_args, $op, data, vars, input_path, spec)
        }
    };
}
infix_math_method!(AddMethod, add_method, add_op);
infix_math_method!(SubMethod, sub_method, sub_op);
infix_math_method!(MulMethod, mul_method, mul_op);
infix_math_method!(DivMethod, div_method, div_op);
infix_math_method!(ModMethod, mod_method, rem_op);

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn add_should_return_sum_of_integers() {
        assert_eq!(
            selection!("$->add(3)").apply_to(&json!(2)),
            (Some(json!(5)), vec![])
        );
    }

    #[test]
    fn add_should_return_sum_of_integer_and_float() {
        assert_eq!(
            selection!("$->add(1.5)").apply_to(&json!(2)),
            (Some(json!(3.5)), vec![])
        );
    }

    #[test]
    fn add_should_return_sum_of_float_and_integer() {
        assert_eq!(
            selection!("$->add(1)").apply_to(&json!(2.5)),
            (Some(json!(3.5)), vec![])
        );
    }

    #[test]
    fn add_should_return_sum_of_floats() {
        assert_eq!(
            selection!("$->add(1.5)").apply_to(&json!(2.5)),
            (Some(json!(4.0)), vec![])
        );
    }

    #[test]
    fn add_should_return_sum_of_multiple_arguments() {
        assert_eq!(
            selection!("$->add(1, 2, 3)").apply_to(&json!(4)),
            (Some(json!(10)), vec![])
        );
    }

    #[test]
    fn add_should_return_sum_with_negative_numbers() {
        assert_eq!(
            selection!("$->add(-5)").apply_to(&json!(10)),
            (Some(json!(5)), vec![])
        );
    }

    #[test]
    fn add_should_return_error_when_applied_to_non_numeric_input() {
        let result = selection!("$->add(1)").apply_to(&json!("not a number"));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->add requires numeric arguments")
        );
    }

    #[test]
    fn add_should_return_error_when_given_non_numeric_argument() {
        let result = selection!("$->add('not a number')").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->add requires numeric arguments")
        );
    }

    #[test]
    fn add_should_return_error_when_no_arguments_provided() {
        let result = selection!("$->add").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->add requires at least one argument")
        );
    }

    #[test]
    fn sub_should_return_difference_of_integers() {
        assert_eq!(
            selection!("$->sub(3)").apply_to(&json!(8)),
            (Some(json!(5)), vec![])
        );
    }

    #[test]
    fn sub_should_return_difference_of_integer_and_float() {
        assert_eq!(
            selection!("$->sub(1.5)").apply_to(&json!(5)),
            (Some(json!(3.5)), vec![])
        );
    }

    #[test]
    fn sub_should_return_difference_of_float_and_integer() {
        assert_eq!(
            selection!("$->sub(2)").apply_to(&json!(5.5)),
            (Some(json!(3.5)), vec![])
        );
    }

    #[test]
    fn sub_should_return_difference_of_floats() {
        assert_eq!(
            selection!("$->sub(2.5)").apply_to(&json!(5.5)),
            (Some(json!(3.0)), vec![])
        );
    }

    #[test]
    fn sub_should_return_difference_with_multiple_arguments() {
        assert_eq!(
            selection!("$->sub(1, 2)").apply_to(&json!(10)),
            (Some(json!(7)), vec![])
        );
    }

    #[test]
    fn sub_should_return_negative_result() {
        assert_eq!(
            selection!("$->sub(10)").apply_to(&json!(5)),
            (Some(json!(-5)), vec![])
        );
    }

    #[test]
    fn sub_should_return_error_when_applied_to_non_numeric_input() {
        let result = selection!("$->sub(1)").apply_to(&json!("not a number"));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->sub requires numeric arguments")
        );
    }

    #[test]
    fn sub_should_return_error_when_given_non_numeric_argument() {
        let result = selection!("$->sub('not a number')").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->sub requires numeric arguments")
        );
    }

    #[test]
    fn sub_should_return_error_when_no_arguments_provided() {
        let result = selection!("$->sub").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->sub requires at least one argument")
        );
    }

    #[test]
    fn mul_should_return_product_of_integers() {
        assert_eq!(
            selection!("$->mul(3)").apply_to(&json!(4)),
            (Some(json!(12)), vec![])
        );
    }

    #[test]
    fn mul_should_return_product_of_integer_and_float() {
        assert_eq!(
            selection!("$->mul(2.5)").apply_to(&json!(4)),
            (Some(json!(10.0)), vec![])
        );
    }

    #[test]
    fn mul_should_return_product_of_float_and_integer() {
        assert_eq!(
            selection!("$->mul(3)").apply_to(&json!(2.5)),
            (Some(json!(7.5)), vec![])
        );
    }

    #[test]
    fn mul_should_return_product_of_floats() {
        assert_eq!(
            selection!("$->mul(2.5)").apply_to(&json!(1.5)),
            (Some(json!(3.75)), vec![])
        );
    }

    #[test]
    fn mul_should_return_product_with_multiple_arguments() {
        assert_eq!(
            selection!("$->mul(2, 3)").apply_to(&json!(4)),
            (Some(json!(24)), vec![])
        );
    }

    #[test]
    fn mul_should_return_product_with_negative_numbers() {
        assert_eq!(
            selection!("$->mul(-2)").apply_to(&json!(5)),
            (Some(json!(-10)), vec![])
        );
    }

    #[test]
    fn mul_should_return_zero_when_multiplied_by_zero() {
        assert_eq!(
            selection!("$->mul(0)").apply_to(&json!(5)),
            (Some(json!(0)), vec![])
        );
    }

    #[test]
    fn mul_should_return_error_when_applied_to_non_numeric_input() {
        let result = selection!("$->mul(2)").apply_to(&json!("not a number"));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->mul requires numeric arguments")
        );
    }

    #[test]
    fn mul_should_return_error_when_given_non_numeric_argument() {
        let result = selection!("$->mul('not a number')").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->mul requires numeric arguments")
        );
    }

    #[test]
    fn mul_should_return_error_when_no_arguments_provided() {
        let result = selection!("$->mul").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->mul requires at least one argument")
        );
    }

    #[test]
    fn div_should_return_quotient_of_integers_as_float() {
        assert_eq!(
            selection!("$->div(2)").apply_to(&json!(6)),
            (Some(json!(3.0)), vec![])
        );
    }

    #[test]
    fn div_should_return_quotient_with_decimal_result() {
        assert_eq!(
            selection!("$->div(2)").apply_to(&json!(5)),
            (Some(json!(2.5)), vec![])
        );
    }

    #[test]
    fn div_should_return_quotient_of_floats() {
        assert_eq!(
            selection!("$->div(2.5)").apply_to(&json!(7.5)),
            (Some(json!(3.0)), vec![])
        );
    }

    #[test]
    fn div_should_return_quotient_with_multiple_arguments() {
        assert_eq!(
            selection!("$->div(2, 3)").apply_to(&json!(12)),
            (Some(json!(2.0)), vec![])
        );
    }

    #[test]
    fn div_should_return_quotient_with_negative_numbers() {
        assert_eq!(
            selection!("$->div(-2)").apply_to(&json!(10)),
            (Some(json!(-5.0)), vec![])
        );
    }

    #[test]
    fn div_should_return_error_when_applied_to_non_numeric_input() {
        let result = selection!("$->div(2)").apply_to(&json!("not a number"));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->div requires numeric arguments")
        );
    }

    #[test]
    fn div_should_return_error_when_given_non_numeric_argument() {
        let result = selection!("$->div('not a number')").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->div requires numeric arguments")
        );
    }

    #[test]
    fn div_should_return_error_when_no_arguments_provided() {
        let result = selection!("$->div").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->div requires at least one argument")
        );
    }

    #[test]
    fn mod_should_return_remainder_of_integers() {
        assert_eq!(
            selection!("$->mod(3)").apply_to(&json!(7)),
            (Some(json!(1)), vec![])
        );
    }

    #[test]
    fn mod_should_return_remainder_of_integer_and_float() {
        assert_eq!(
            selection!("$->mod(2.5)").apply_to(&json!(7)),
            (Some(json!(2.0)), vec![])
        );
    }

    #[test]
    fn mod_should_return_remainder_of_float_and_integer() {
        assert_eq!(
            selection!("$->mod(3)").apply_to(&json!(7.5)),
            (Some(json!(1.5)), vec![])
        );
    }

    #[test]
    fn mod_should_return_remainder_of_floats() {
        assert_eq!(
            selection!("$->mod(2.5)").apply_to(&json!(7.5)),
            (Some(json!(0.0)), vec![])
        );
    }

    #[test]
    fn mod_should_return_remainder_with_multiple_arguments() {
        assert_eq!(
            selection!("$->mod(3, 2)").apply_to(&json!(10)),
            (Some(json!(1)), vec![])
        );
    }

    #[test]
    fn mod_should_return_zero_when_no_remainder() {
        assert_eq!(
            selection!("$->mod(5)").apply_to(&json!(10)),
            (Some(json!(0)), vec![])
        );
    }

    #[test]
    fn mod_should_return_error_when_applied_to_non_numeric_input() {
        let result = selection!("$->mod(2)").apply_to(&json!("not a number"));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->mod requires numeric arguments")
        );
    }

    #[test]
    fn mod_should_return_error_when_given_non_numeric_argument() {
        let result = selection!("$->mod('not a number')").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->mod requires numeric arguments")
        );
    }

    #[test]
    fn mod_should_return_error_when_no_arguments_provided() {
        let result = selection!("$->mod").apply_to(&json!(5));
        assert_eq!(result.0, None);
        assert!(!result.1.is_empty());
        assert!(
            result.1[0]
                .message()
                .contains("Method ->mod requires at least one argument")
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use serde_json::Number;
    use shape::Shape;
    use shape::location::Location;
    use shape::location::SourceId;

    use crate::connectors::json_selection::MethodArgs;
    use crate::connectors::json_selection::ShapeContext;
    use crate::connectors::json_selection::lit_expr::LitExpr;
    use crate::connectors::json_selection::location::WithRange;
    use crate::connectors::json_selection::methods::public::arithmetic::math_shape;

    fn get_location() -> Location {
        Location {
            source_id: SourceId::new("test".to_string()),
            span: 0..7,
        }
    }

    fn get_shape(method_name: &str, args: Vec<WithRange<LitExpr>>, input: Shape) -> Shape {
        let location = get_location();
        math_shape(
            &ShapeContext::new(location.source_id),
            &WithRange::new(method_name.to_string(), Some(location.span)),
            Some(&MethodArgs { args, range: None }),
            input,
            Shape::none(),
        )
    }

    #[test]
    fn add_shape_should_return_int_for_integer_arguments() {
        assert_eq!(
            get_shape(
                "add",
                vec![WithRange::new(LitExpr::Number(Number::from(2)), None)],
                Shape::int([])
            ),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn add_shape_should_return_float_for_float_arguments() {
        assert_eq!(
            get_shape(
                "add",
                vec![WithRange::new(
                    LitExpr::Number(Number::from_f64(2.5).unwrap()),
                    None
                )],
                Shape::int([])
            ),
            Shape::float([get_location()])
        );
    }

    #[test]
    fn add_shape_should_return_float_for_float_input() {
        assert_eq!(
            get_shape(
                "add",
                vec![WithRange::new(LitExpr::Number(Number::from(2)), None)],
                Shape::float([])
            ),
            Shape::float([get_location()])
        );
    }

    #[test]
    fn add_shape_should_return_error_for_non_numeric_input() {
        assert_eq!(
            get_shape(
                "add",
                vec![WithRange::new(LitExpr::Number(Number::from(1)), None)],
                Shape::string([]),
            ),
            Shape::error(
                "Method ->add received non-numeric input".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn add_shape_should_return_error_for_non_numeric_argument() {
        assert_eq!(
            get_shape(
                "add",
                vec![WithRange::new(
                    LitExpr::String("not a number".to_string()),
                    None,
                )],
                Shape::int([]),
            ),
            Shape::error(
                "Method ->add received non-numeric argument 0".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn sub_shape_should_return_int_for_integer_arguments() {
        assert_eq!(
            get_shape(
                "sub",
                vec![WithRange::new(LitExpr::Number(Number::from(2)), None)],
                Shape::int([])
            ),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn sub_shape_should_return_float_for_float_arguments() {
        assert_eq!(
            get_shape(
                "sub",
                vec![WithRange::new(
                    LitExpr::Number(Number::from_f64(2.5).unwrap()),
                    None
                )],
                Shape::int([])
            ),
            Shape::float([get_location()])
        );
    }

    #[test]
    fn mul_shape_should_return_int_for_integer_arguments() {
        assert_eq!(
            get_shape(
                "mul",
                vec![WithRange::new(LitExpr::Number(Number::from(4)), None)],
                Shape::int([])
            ),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn mul_shape_should_return_float_for_float_arguments() {
        assert_eq!(
            get_shape(
                "mul",
                vec![WithRange::new(
                    LitExpr::Number(Number::from_f64(4.5).unwrap()),
                    None
                )],
                Shape::int([])
            ),
            Shape::float([get_location()])
        );
    }

    #[test]
    fn div_shape_should_always_return_float() {
        assert_eq!(
            get_shape(
                "div",
                vec![WithRange::new(LitExpr::Number(Number::from(2)), None)],
                Shape::int([])
            ),
            Shape::float([get_location()])
        );
    }

    #[test]
    fn div_shape_should_return_float_for_float_arguments() {
        assert_eq!(
            get_shape(
                "div",
                vec![WithRange::new(
                    LitExpr::Number(Number::from_f64(2.5).unwrap()),
                    None
                )],
                Shape::int([])
            ),
            Shape::float([get_location()])
        );
    }

    #[test]
    fn mod_shape_should_return_int_for_integer_arguments() {
        assert_eq!(
            get_shape(
                "mod",
                vec![WithRange::new(LitExpr::Number(Number::from(3)), None)],
                Shape::int([])
            ),
            Shape::int([get_location()])
        );
    }

    #[test]
    fn mod_shape_should_return_float_for_float_arguments() {
        assert_eq!(
            get_shape(
                "mod",
                vec![WithRange::new(
                    LitExpr::Number(Number::from_f64(3.5).unwrap()),
                    None
                )],
                Shape::int([])
            ),
            Shape::float([get_location()])
        );
    }

    #[test]
    fn math_shape_should_return_error_for_non_numeric_input() {
        assert_eq!(
            get_shape(
                "mul",
                vec![WithRange::new(LitExpr::Number(Number::from(1)), None)],
                Shape::string([]),
            ),
            Shape::error(
                "Method ->mul received non-numeric input".to_string(),
                [get_location()]
            )
        );
    }

    #[test]
    fn math_shape_should_return_error_for_non_numeric_argument() {
        assert_eq!(
            get_shape(
                "div",
                vec![WithRange::new(LitExpr::String("invalid".to_string()), None)],
                Shape::int([]),
            ),
            Shape::error(
                "Method ->div received non-numeric argument 0".to_string(),
                [get_location()]
            )
        );
    }
}
