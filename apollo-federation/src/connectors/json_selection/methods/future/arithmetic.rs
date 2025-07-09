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

#[allow(dead_code)] // method type-checking disabled until we add name resolution
fn math_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    _method_args: Option<&MethodArgs>,
    _input_shape: Shape,
    _dollar_shape: Shape,
) -> Shape {
    Shape::error(
        "TODO: math_shape",
        method_name.shape_location(context.source_id()),
    )
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
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            arithmetic_method(method_name, method_args, $op, data, vars, input_path)
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
    fn add_should_add_whole_numbers() {
        assert_eq!(
            selection!("$->add(1)").apply_to(&json!(2)),
            (Some(json!(3)), vec![]),
        );
    }

    #[test]
    fn add_should_add_decimal_argument_to_whole_applied_to_number() {
        assert_eq!(
            selection!("$->add(1.5)").apply_to(&json!(2)),
            (Some(json!(3.5)), vec![]),
        );
    }

    #[test]
    fn add_should_add_whole_argument_to_decimal_applied_to_number() {
        assert_eq!(
            selection!("$->add(1)").apply_to(&json!(2.5)),
            (Some(json!(3.5)), vec![]),
        );
    }

    #[test]
    fn add_should_add_multiple_times_when_multiple_arguments_provided() {
        assert_eq!(
            selection!("$->add(1, 2, 3, 5, 8)").apply_to(&json!(1)),
            (Some(json!(20)), vec![]),
        );
    }

    #[test]
    fn sub_should_subtract_whole_numbers() {
        assert_eq!(
            selection!("$->sub(1)").apply_to(&json!(2)),
            (Some(json!(1)), vec![]),
        );
    }

    #[test]
    fn sub_should_subtract_decimal_argument_to_whole_applied_to_number() {
        assert_eq!(
            selection!("$->sub(1.5)").apply_to(&json!(2)),
            (Some(json!(0.5)), vec![]),
        );
    }

    #[test]
    fn sub_should_subtract_whole_argument_to_decimal_applied_to_number() {
        assert_eq!(
            selection!("$->sub(10)").apply_to(&json!(2.5)),
            (Some(json!(-7.5)), vec![]),
        );
    }

    #[test]
    fn sub_should_subtract_multiple_times_when_multiple_arguments_provided() {
        assert_eq!(
            selection!("$->sub(10, 2.5)").apply_to(&json!(2.5)),
            (Some(json!(-10.0)), vec![]),
        );
    }

    #[test]
    fn mul_should_multiply_whole_numbers() {
        assert_eq!(
            selection!("$->mul(2)").apply_to(&json!(3)),
            (Some(json!(6)), vec![]),
        );
    }

    #[test]
    fn mul_should_multiply_decimal_argument_to_whole_applied_to_number() {
        assert_eq!(
            selection!("$->mul(2.5)").apply_to(&json!(3)),
            (Some(json!(7.5)), vec![]),
        );
    }

    #[test]
    fn mul_should_multiply_whole_argument_to_decimal_applied_to_number() {
        assert_eq!(
            selection!("$->mul(2)").apply_to(&json!(3.5)),
            (Some(json!(7.0)), vec![]),
        );
    }

    #[test]
    fn mul_should_multiply_negative_numbers() {
        assert_eq!(
            selection!("$->mul(-2.5)").apply_to(&json!(3.5)),
            (Some(json!(-8.75)), vec![]),
        );
    }

    #[test]
    fn mul_should_multiply_multiple_times_when_multiple_arguments_provided() {
        assert_eq!(
            selection!("$->mul(2, 3, 5, 7)").apply_to(&json!(10)),
            (Some(json!(2100)), vec![]),
        );
    }

    #[test]
    fn div_should_divide_whole_numbers() {
        assert_eq!(
            selection!("$->div(2)").apply_to(&json!(6)),
            (Some(json!(3.0)), vec![]),
        );
    }

    #[test]
    fn div_should_divide_decimal_numbers() {
        assert_eq!(
            selection!("$->div(2.5)").apply_to(&json!(7.5)),
            (Some(json!(3.0)), vec![]),
        );
    }

    #[test]
    fn div_should_divide_whole_numbers_that_result_in_decimal() {
        assert_eq!(
            selection!("$->div(2)").apply_to(&json!(7)),
            (Some(json!(3.5)), vec![]),
        );
    }

    #[test]
    fn div_should_divide_whole_numbers_mixed_with_decimal_numbers() {
        assert_eq!(
            selection!("$->div(2.5)").apply_to(&json!(7)),
            (Some(json!(2.8)), vec![]),
        );
    }

    #[test]
    fn div_should_divide_multiple_times_when_multiple_arguments_provided() {
        assert_eq!(
            selection!("$->div(2, 3, 5, 7)").apply_to(&json!(2100)),
            (Some(json!(10.0)), vec![]),
        );
    }

    #[test]
    fn mod_should_return_remainder_of_whole_numbers() {
        assert_eq!(
            selection!("$->mod(2)").apply_to(&json!(7)),
            (Some(json!(1)), vec![]),
        );
    }

    #[test]
    fn mod_should_return_remainder_of_decimal_numbers() {
        assert_eq!(
            selection!("$->mod(2.5)").apply_to(&json!(7.5)),
            (Some(json!(0.0)), vec![]),
        );
    }

    #[test]
    fn mod_should_return_remainder_of_mix_of_whole_and_decimal_numbers() {
        assert_eq!(
            selection!("$->mod(2.5)").apply_to(&json!(7)),
            (Some(json!(2.0)), vec![]),
        );
    }

    #[test]
    fn mod_should_return_remainder_multiple_times_when_multiple_arguments_provided() {
        assert_eq!(
            selection!("$->mod(2, 3, 5, 7)").apply_to(&json!(2100)),
            (Some(json!(0)), vec![]),
        );
    }
}
