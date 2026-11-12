use serde_json_bytes::Value as JSON;
use shape::Shape;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::json_selection::location::merge_ranges;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(IfThenMethod, if_then_method, if_then_shape);

/// The ifThen method evaluates a condition and returns one of two expressions based on the result.
///
/// Syntax: `condition->ifThen(then_expr[, else_expr])`
///
/// If the condition (the input data) evaluates to true, returns then_expr.
/// Otherwise, returns else_expr if provided, or None if omitted.
///
/// Example: `age->gte(18)->ifThen('adult', 'minor')`
fn if_then_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let Some(MethodArgs { args, .. }) = method_args else {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires 1-2 arguments: then_expr[, else_expr]",
                    method_name.as_ref(),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    if args.is_empty() || args.len() > 2 {
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{} requires 1-2 arguments, got {}",
                    method_name.as_ref(),
                    args.len(),
                ),
                input_path.to_vec(),
                merge_ranges(
                    method_name.range(),
                    method_args.and_then(|args| args.range()),
                ),
                spec,
            )],
        );
    }

    // There is no notion of truthiness in this mapping language: ->and/->or/->not
    // and ->filter/->find all require a strict boolean. ->ifThen keeps that
    // invariant — `true` is the only value that reaches the then branch, and
    // anything that is neither `true` nor `false` is reported. The error is
    // non-fatal, unlike those methods, because ->ifThen has a defined fallback
    // (the else branch, or nothing) and so can complain and still produce a
    // value rather than collapsing the whole selection.
    let mut errors = Vec::new();
    if data.as_bool().is_none() {
        errors.push(ApplyToError::new(
            format!(
                "Method ->{} can only be applied to boolean values.",
                method_name.as_ref()
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
    }

    // Check if condition is true
    let (value_opt, branch_errors) = if data == &JSON::Bool(true) {
        // Evaluate and return then_expr (first argument)
        if let Some(then_arg) = args.first() {
            then_arg.apply_to_path(data, vars, input_path, spec)
        } else {
            unreachable!("args validation ensures at least 1 argument")
        }
    } else {
        // Return else_expr if provided (second argument), otherwise None
        if let Some(else_arg) = args.get(1) {
            else_arg.apply_to_path(data, vars, input_path, spec)
        } else {
            (None, vec![])
        }
    };
    errors.extend(branch_errors);

    (value_opt, errors)
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
pub(crate) fn if_then_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let Some(MethodArgs { args, .. }) = method_args else {
        return Shape::error(
            format!(
                "Method ->{} requires 1-2 arguments: then_expr[, else_expr]",
                method_name.as_ref(),
            ),
            method_name.shape_location(context.source_id()),
        );
    };

    if args.is_empty() || args.len() > 2 {
        return Shape::error(
            format!(
                "Method ->{} requires 1-2 arguments, got {}",
                method_name.as_ref(),
                args.len(),
            ),
            merge_ranges(
                method_name.range(),
                method_args.and_then(|args| args.range()),
            )
            .map(|range| context.source_id().location(range)),
        );
    }

    // Compute shape for then branch
    let then_shape = if let Some(then_arg) = args.first() {
        then_arg.compute_output_shape(context, input_shape.clone(), dollar_shape.clone())
    } else {
        unreachable!("args validation ensures at least 1 argument")
    };

    // Compute shape for else branch (or None if omitted)
    let else_shape = if let Some(else_arg) = args.get(1) {
        else_arg.compute_output_shape(context, input_shape.clone(), dollar_shape)
    } else {
        Shape::none()
    };

    // Mirror the runtime's non-fatal complaint about non-boolean conditions.
    // Anything bool-like, or unknown/named (not yet resolved), is accepted. For
    // anything else the partial is the else branch alone rather than the union:
    // since `true` is the only value that reaches the then branch, a shape known
    // not to be boolean can never take it, so narrowing here is sound.
    if !(Shape::bool([]).accepts(&input_shape) || input_shape.accepts(&Shape::unknown([]))) {
        return Shape::error_with_partial(
            format!(
                "Method ->{} can only be applied to boolean values. Got {input_shape}.",
                method_name.as_ref()
            ),
            else_shape,
            method_name.shape_location(context.source_id()),
        );
    }

    // Return union of both branches
    Shape::one(
        vec![then_shape, else_shape],
        method_name.shape_location(context.source_id()),
    )
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::connectors::ConnectSpec;
    use crate::selection;

    #[test]
    fn if_then_should_return_then_expr_when_condition_is_true() {
        assert_eq!(
            selection!(
                r#"
                result: condition->ifThen('yes', 'no')
                "#
            )
            .apply_to(&json!({
                "condition": true,
            })),
            (
                Some(json!({
                    "result": "yes",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn if_then_should_return_else_expr_when_condition_is_false() {
        assert_eq!(
            selection!(
                r#"
                result: condition->ifThen('yes', 'no')
                "#
            )
            .apply_to(&json!({
                "condition": false,
            })),
            (
                Some(json!({
                    "result": "no",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn if_then_should_return_none_when_condition_is_false_and_no_else() {
        assert_eq!(
            selection!(
                r#"
                result: condition->ifThen('yes')
                "#
            )
            .apply_to(&json!({
                "condition": false,
            })),
            (Some(json!({})), vec![]),
        );
    }

    #[test]
    fn if_then_should_work_with_complex_expressions() {
        assert_eq!(
            selection!(
                r#"
                status: age->gte(18)->ifThen('adult', 'minor')
                "#
            )
            .apply_to(&json!({
                "age": 25,
            })),
            (
                Some(json!({
                    "status": "adult",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn if_then_should_work_with_nested_data() {
        assert_eq!(
            selection!(
                r#"
                discount: isPremium->ifThen(20, 0)
                "#
            )
            .apply_to(&json!({
                "isPremium": true,
            })),
            (
                Some(json!({
                    "discount": 20,
                })),
                vec![],
            ),
        );
    }

    #[rstest::rstest]
    // No truthiness: `true` is the only value that reaches the then branch, so
    // every one of these takes the else branch. Notably `"false"` and `1` are
    // not special — a language with truthiness would disagree about both.
    #[case::null(json!(null))]
    #[case::zero(json!(0))]
    #[case::one(json!(1))]
    #[case::negative(json!(-1))]
    #[case::empty_string(json!(""))]
    #[case::non_empty_string(json!("not a boolean"))]
    #[case::true_string(json!("true"))]
    #[case::false_string(json!("false"))]
    #[case::empty_array(json!([]))]
    #[case::empty_object(json!({}))]
    fn if_then_should_take_else_branch_and_report_non_booleans(
        #[case] value: serde_json_bytes::Value,
    ) {
        let result = selection!(
            r#"
                result: value->ifThen('yes', 'no')
                "#
        )
        .apply_to(&json!({ "value": value }));

        // The selection still produces a value — the error is non-fatal...
        assert_eq!(result.0, Some(json!({ "result": "no" })));

        // ...but anything that is neither `true` nor `false` is reported.
        assert_eq!(result.1.len(), 1);
        assert!(
            result.1[0]
                .message()
                .contains("Method ->ifThen can only be applied to boolean values.")
        );
    }

    #[test]
    fn if_then_branch_args_are_applied_to_the_condition_not_the_receiver() {
        // The then/else expressions are applied to the *condition* value, so a
        // bare `@` inside them is the boolean, not the value being tested. This
        // is why ->as is needed to carry the receiver into a branch.
        assert_eq!(
            selection!("$.n->gt(1)->ifThen(@, 'no')", ConnectSpec::V0_5).apply_to(&json!({
                "n": 5,
            })),
            (Some(json!(true)), vec![]),
        );

        // Binding the receiver first makes it reachable from either branch.
        assert_eq!(
            selection!(
                "$.n->as($self)->gt(1)->ifThen($self, 'no')",
                ConnectSpec::V0_5
            )
            .apply_to(&json!({
                "n": 5,
            })),
            (Some(json!(5)), vec![]),
        );
    }

    #[rstest::rstest]
    // ->ifThen expresses min/max over the receiver and an argument, which
    // otherwise needs ->match's [true, x] pair plus an `@` catch-all. The
    // receiver is bound with ->as so both branches can name it, since a bare
    // `@` there would be the comparison's result. These bodies are the
    // ->ifThen form of the min/max defs.
    #[case::min_receiver_smaller(json!(3), json!(7), 3)]
    #[case::min_arg_smaller(json!(7), json!(3), 3)]
    #[case::min_equal(json!(4), json!(4), 4)]
    #[case::min_negative(json!(-2), json!(1), -2)]
    fn if_then_can_express_min(
        #[case] receiver: serde_json_bytes::Value,
        #[case] arg: serde_json_bytes::Value,
        #[case] expected: i64,
    ) {
        assert_eq!(
            selection!(
                "$.n->as($self)->lte($.arg)->ifThen($self, $.arg)",
                ConnectSpec::V0_5
            )
            .apply_to(&json!({
                "n": receiver,
                "arg": arg,
            })),
            (Some(json!(expected)), vec![]),
        );
    }

    #[rstest::rstest]
    #[case::max_receiver_larger(json!(7), json!(3), 7)]
    #[case::max_arg_larger(json!(3), json!(7), 7)]
    #[case::max_equal(json!(4), json!(4), 4)]
    #[case::max_negative(json!(-2), json!(1), 1)]
    fn if_then_can_express_max(
        #[case] receiver: serde_json_bytes::Value,
        #[case] arg: serde_json_bytes::Value,
        #[case] expected: i64,
    ) {
        assert_eq!(
            selection!(
                "$.n->as($self)->gte($.arg)->ifThen($self, $.arg)",
                ConnectSpec::V0_5
            )
            .apply_to(&json!({
                "n": receiver,
                "arg": arg,
            })),
            (Some(json!(expected)), vec![]),
        );
    }

    #[rstest::rstest]
    #[case::bool_true(json!(true), "yes")]
    #[case::bool_false(json!(false), "no")]
    fn if_then_should_not_warn_on_actual_booleans(
        #[case] value: serde_json_bytes::Value,
        #[case] expected: &str,
    ) {
        assert_eq!(
            selection!(
                r#"
                result: value->ifThen('yes', 'no')
                "#
            )
            .apply_to(&json!({ "value": value })),
            (Some(json!({ "result": expected })), vec![]),
        );
    }

    #[test]
    fn if_then_non_boolean_spread_reports_both_cause_and_consequence() {
        // A non-boolean condition takes the else branch, which is absent here,
        // so the spread contributes nothing and adds its usual note after the
        // root cause — the same pairing apply_to.rs asserts for any errored
        // inline path. Neither error is fatal: the selection still resolves.
        let result = selection!(
            "before ...value->ifThen({ optional: 'value' }) after",
            ConnectSpec::V0_5
        )
        .apply_to(&json!({
            "before": "before value",
            "after": "after value",
            "value": "not a boolean",
        }));

        assert_eq!(
            result.0,
            Some(json!({
                "before": "before value",
                "after": "after value",
            })),
        );
        assert_eq!(
            result.1.iter().map(|e| e.message()).collect::<Vec<_>>(),
            vec![
                "Method ->ifThen can only be applied to boolean values.",
                "Inlined path produced no value",
            ],
        );
    }

    #[rstest::rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    #[case::v0_4(ConnectSpec::V0_4)]
    #[case::v0_5(ConnectSpec::V0_5)]
    fn if_then_should_evaluate_lazily(#[case] spec: ConnectSpec) {
        // When condition is true, else_expr should not be evaluated
        assert_eq!(
            selection!("$.isValid->ifThen('then', $.missing)", spec).apply_to(&json!({
                "isValid": true,
            })),
            (Some(json!("then")), vec![]),
        );

        // When condition is false, then_expr should not be evaluated
        assert_eq!(
            selection!("$.isValid->ifThen($.missing, 'else')", spec).apply_to(&json!({
                "isValid": false,
            })),
            (Some(json!("else")), vec![]),
        );
    }

    #[rstest::rstest]
    #[case::v0_4(ConnectSpec::V0_4)]
    #[case::v0_5(ConnectSpec::V0_5)]
    fn if_then_spread_with_none_should_spread_nothing(#[case] spec: ConnectSpec) {
        let sel = selection!(
            "before ...condition->ifThen({ optional: 'value' }) after",
            spec
        );

        // When condition is true, spread should include the optional field
        assert_eq!(
            sel.apply_to(&json!({
                "before": "before value",
                "after": "after value",
                "condition": true,
            })),
            (
                Some(json!({
                    "before": "before value",
                    "after": "after value",
                    "optional": "value",
                })),
                vec![],
            ),
        );

        // When condition is false (no else clause), ifThen returns None
        // which should spread nothing (no error, no fields)
        assert_eq!(
            sel.apply_to(&json!({
                "before": "before value",
                "after": "after value",
                "condition": false,
            })),
            (
                Some(json!({
                    "before": "before value",
                    "after": "after value",
                })),
                vec![],
            ),
        );
    }
}
