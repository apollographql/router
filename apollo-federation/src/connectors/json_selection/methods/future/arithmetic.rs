use serde_json::Number;
use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

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
    let mut check_result = check_numeric_shape(&input_shape);

    if matches!(check_result, CheckNumericResult::Neither) {
        return Shape::error(
            format!(
                "Method ->{} received non-numeric input",
                method_name.as_ref()
            ),
            input_shape.locations.iter().cloned(),
        );
    }

    if method_name.as_ref() == "div" {
        // The ->div method stays safe by always returning Float, so
        // check_result starts off false in that case.
        check_result = CheckNumericResult::FloatPossible;
    }

    for (i, arg) in method_args
        .iter()
        .flat_map(|args| args.args.iter())
        .enumerate()
    {
        let arg_shape = arg.compute_output_shape(
            context,
            input_shape.clone(),
            dollar_shape.clone(),
        );

        match check_numeric_shape(&arg_shape) {
            CheckNumericResult::IntForSure => {}
            CheckNumericResult::FloatPossible => {
                check_result = CheckNumericResult::FloatPossible;
            }
            CheckNumericResult::Neither => {
                return Shape::error(
                    format!(
                        "Method ->{} received non-numeric argument {}",
                        method_name.as_ref(),
                        i
                    ),
                    arg_shape.locations.iter().cloned(),
                );
            }
        }
    }

    match check_result {
        CheckNumericResult::IntForSure => {
            // TODO If we wanted to climb Mount Cleverest, we could perform
            // static integer math when all the inputs are statically known, and
            // return a specific Shape::int_value when that math succeeds. That
            // might require using different shape functions for each of the
            // math operations, rather than a single math_shape function.
            Shape::int(method_name.shape_location(context.source_id()))
        }
        CheckNumericResult::FloatPossible => Shape::float(method_name.shape_location(context.source_id())),
        CheckNumericResult::Neither => unreachable!("handled above"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CheckNumericResult {
    IntForSure,
    FloatPossible,
    Neither,
}

fn check_numeric_shape(arg_shape: &Shape) -> CheckNumericResult {
    match arg_shape.case() {
        // This includes both the general Int case and any specific
        // ShapeCase::Int(Some(value)) integer shapes.
        ShapeCase::Int(_) => CheckNumericResult::IntForSure,

        // Beside the obvious ShapeCase::Float variant, Name and Unknown shapes
        // could potentially turn out to be numeric (i.e. Float but not
        // necessarily Int), so they do not warrant an error (yet).
        ShapeCase::Name(_, _) | ShapeCase::Unknown | ShapeCase::Float => {
            CheckNumericResult::FloatPossible
        }

        ShapeCase::One(one) => {
            let mut result = CheckNumericResult::IntForSure;

            for shape in one.iter() {
                match check_numeric_shape(shape) {
                    CheckNumericResult::IntForSure => {
                        // Leave result == IntForSure if not already
                        // FloatPossible. This means all the member shapes have
                        // to be Int in order the final result to be IntForSure.
                    }
                    CheckNumericResult::FloatPossible => {
                        result = CheckNumericResult::FloatPossible;
                    }
                    CheckNumericResult::Neither => {
                        // If any of the member shapes is not numeric, there's a
                        // chance this math method will fail at runtime.
                        return CheckNumericResult::Neither;
                    }
                };
            }

            result
        }

        ShapeCase::All(all) => {
            let mut saw_int = false;
            let mut saw_float = false;

            for shape in all.iter() {
                match check_numeric_shape(shape) {
                    CheckNumericResult::IntForSure => {
                        saw_int = true;
                    }
                    CheckNumericResult::FloatPossible => {
                        saw_float = true;
                    }
                    CheckNumericResult::Neither => {}
                };
            }

            // Because the ShapeCase::All intersection claims to be all the
            // member shapes simultaneously, the answer is IntForSure if any
            // member is an Int, even if some members are FloatPossible or even
            // Neither. If no member is an Int, but some are FloatPossible,
            // that's the answer. Otherwise, the answer is Neither.
            if saw_int {
                CheckNumericResult::IntForSure
            } else if saw_float {
                CheckNumericResult::FloatPossible
            } else {
                CheckNumericResult::Neither
            }
        }

        // Math methods refuse to operate on definitely non-numeric values.
        ShapeCase::Bool(_)
        | ShapeCase::String(_)
        | ShapeCase::Null
        | ShapeCase::None
        | ShapeCase::Object { .. }
        | ShapeCase::Array { .. } => CheckNumericResult::Neither,

        // An Error with a partial shape delegates to the partial shape.
        ShapeCase::Error(shape::Error { partial, .. }) => {
            if let Some(partial) = partial {
                check_numeric_shape(partial)
            } else {
                CheckNumericResult::Neither
            }
        }
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
    use shape::Shape;
    use shape::location::SourceId;

    use crate::connectors::json_selection::ShapeContext;
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

    #[test]
    fn add_shape_can_return_int() {
        assert_eq!(selection!("$(1->add(2, 3))").shape().pretty_print(), "Int");

        assert_eq!(
            selection!("$(-1->add(2, -3))").shape().pretty_print(),
            "Int",
        );

        assert_eq!(
            selection!("$(1->add(2.1, -3))").shape().pretty_print(),
            "Float",
        );

        assert_eq!(
            selection!("$(-1.1->add(2, 3))").shape().pretty_print(),
            "Float",
        );
    }

    #[test]
    fn sub_shape_can_return_int() {
        assert_eq!(selection!("$(1->sub(2, 3))").shape().pretty_print(), "Int");

        assert_eq!(
            selection!("$(-1->sub(2, -3))").shape().pretty_print(),
            "Int",
        );

        assert_eq!(
            selection!("$(1->sub(2.1, -3))").shape().pretty_print(),
            "Float",
        );

        assert_eq!(
            selection!("$(-1.1->sub(2, 3))").shape().pretty_print(),
            "Float",
        );
    }

    #[test]
    fn mul_shape_can_return_int() {
        assert_eq!(selection!("$(1->mul(2, 3))").shape().pretty_print(), "Int");

        assert_eq!(
            selection!("$(-1->mul(2, -3))").shape().pretty_print(),
            "Int",
        );

        assert_eq!(
            selection!("$(1->mul(2.1, -3))").shape().pretty_print(),
            "Float",
        );

        assert_eq!(
            selection!("$(-1.1->mul(2, 3))").shape().pretty_print(),
            "Float",
        );
    }

    #[test]
    fn div_shape_cannot_return_int() {
        assert_eq!(
            selection!("$(1->div(2, 3))").shape().pretty_print(),
            "Float"
        );

        assert_eq!(
            selection!("$(-1->div(2, -3))").shape().pretty_print(),
            "Float",
        );

        assert_eq!(
            selection!("$(1->div(2.1, -3))").shape().pretty_print(),
            "Float",
        );

        assert_eq!(
            selection!("$(-1.1->div(2, 3))").shape().pretty_print(),
            "Float",
        );
    }

    #[test]
    fn mod_shape_can_return_int() {
        assert_eq!(selection!("$(1->mod(2, 3))").shape().pretty_print(), "Int");

        assert_eq!(
            selection!("$(-1->mod(2, -3))").shape().pretty_print(),
            "Int",
        );

        assert_eq!(
            selection!("$(1->mod(2.1, -3))").shape().pretty_print(),
            "Float"
        );
    }

    #[test]
    fn check_errors_for_non_numeric_arguments() {
        assert_eq!(
            selection!("$->add(1, 'foo')").shape().pretty_print(),
            "Error<\"Method ->add received non-numeric argument 1\">",
        );

        assert_eq!(
            selection!("$->add(1, 2, true)").shape().pretty_print(),
            "Error<\"Method ->add received non-numeric argument 2\">",
        );

        let add_one_selection = selection!("$->add(1)");
        let add_one_shape = add_one_selection.compute_output_shape(
            &ShapeContext::new(SourceId::Other("JSONSelection".into())),
            Shape::string([]),
        );

        assert_eq!(
            add_one_shape.pretty_print(),
            "Error<\"Method ->add received non-numeric input\">",
        );
    }

    #[test]
    fn test_union_argument_shapes() {
        let all_numeric_union =
            selection!("$->add(@->eq(0)->match([true, 1], [false, 2], [@, 3]))");
        let all_numeric_union_shape = all_numeric_union.compute_output_shape(
            &ShapeContext::new(SourceId::Other("JSONSelection".into())),
            Shape::int([]),
        );
        assert_eq!(all_numeric_union_shape.pretty_print(), "Int");

        let missing_catchall_case = selection!("$->add(@->eq(0)->match([true, 1], [false, 2]))");
        let missing_catchall_case_shape = missing_catchall_case.compute_output_shape(
            &ShapeContext::new(SourceId::Other("JSONSelection".into())),
            Shape::int([]),
        );
        assert_eq!(
            missing_catchall_case_shape.pretty_print(),
            "Error<\"Method ->add received non-numeric argument 0\">"
        );

        let mixed_float_union =
            selection!("$->add(@->eq(0)->match([true, 1], [false, 2.5], [@, 3]))");
        let mixed_float_union_shape = mixed_float_union.compute_output_shape(
            &ShapeContext::new(SourceId::Other("JSONSelection".into())),
            Shape::int([]),
        );
        assert_eq!(mixed_float_union_shape.pretty_print(), "Float");

        let no_number_union =
            selection!("$->add(@->eq(0)->match([true, 'a'], [false, 'b'], [@, null]))");
        let no_number_union_shape = no_number_union.compute_output_shape(
            &ShapeContext::new(SourceId::Other("JSONSelection".into())),
            Shape::int([]),
        );
        assert_eq!(
            no_number_union_shape.pretty_print(),
            "Error<\"Method ->add received non-numeric argument 0\">"
        );
    }
}
