use serde_json_bytes::Value as JSON;
use serde_json_bytes::json;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyContext;
use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::PathList;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::known_var::KnownVariable;
use crate::connectors::json_selection::lit_expr::LitExpr;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(ReduceMethod, reduce_method, reduce_shape);
/// The `array->reduce($acc, <seed>, <update>)` method folds an array into a
/// single value, left to right.
///
/// The accumulator starts as `<seed>` (evaluated once, in the caller's scope),
/// and `<update>` is evaluated once per element with `@` bound to that element
/// and `$acc` bound to the accumulator so far. Each result becomes the next
/// accumulator, and the last one is the method's output:
///
/// ```graphql
/// $([1, 2, 3, 4])->reduce($acc, 0, @->add($acc))   // 10
/// orders->reduce($acc, 0, @.total->add($acc))      // sum one field of each element
/// ```
///
/// The seed does three jobs: it is the accumulator's starting value, it is the
/// result for an empty array (so the fold is *total* — there is always an
/// answer), and it pins the accumulator's type, which is what lets a fold
/// return something other than the element type. That last job is latent for
/// now: folding an array into an object needs a method that builds one key at a
/// time, which the language does not yet have.
///
/// # `$acc` is lexical, `@` is a cursor — mind the argument order
///
/// `@->add($acc)` is correct. The mirror image, `$acc->add(@)`, is **not**, and
/// it fails silently: leading a path with `$acc` retargets the cursor onto the
/// accumulator (see the `PathList::Var` arm of `apply_to_path`), so the inner
/// `@` means the accumulator rather than the element. Every step computes
/// `acc + acc`, the array is never read, and with a seed of `0` the answer is
/// `0` — no error, no type mismatch.
///
/// This is worth knowing precisely because `acc.combine(element)` is how folds
/// read in most languages, so it is the shape people reach for first. Naming
/// the element with a fourth argument was considered and rejected in favor of
/// keeping the signature tight and warning about the trap.
///
/// # Recursion is impossible, so the language stays total
///
/// The trip count is fixed by the input array's length before the loop starts,
/// and the accumulator is never the iteration source, so `->reduce` is bounded
/// iteration over finite data rather than a general loop. Folding is safe in a
/// way unfolding would not be.
fn reduce_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    context: &ApplyContext,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let spec = context.spec();
    let (acc_name_opt, mut errors) = check_method_args(method_name, method_args, input_path, spec);

    // Without a validated accumulator name there is nothing to bind the update
    // expression against, so the fold cannot run at all. `check_method_args`
    // has already explained why.
    let (Some(acc_name), Some(args)) = (acc_name_opt, method_args) else {
        return (None, errors);
    };
    let (Some(seed_arg), Some(update_arg)) = (args.args.get(1), args.args.get(2)) else {
        return (None, errors);
    };

    // The seed is evaluated once, in the caller's scope, before any binding
    // exists — so `$acc` is deliberately not in scope here.
    let (seed_opt, seed_errors) = seed_arg.apply_to_path(data, vars, input_path, context);
    errors.extend(seed_errors);
    let Some(mut acc) = seed_opt else {
        return (None, errors);
    };

    // A non-array input folds as a one-element array, matching how ->map and
    // ->filter treat scalars.
    let singleton;
    let elements: &[JSON] = match data {
        JSON::Array(array) => array.as_slice(),
        other => {
            singleton = [other.clone()];
            &singleton
        }
    };

    let acc_path = InputPath::empty().append(json!(acc_name));

    for (index, element) in elements.iter().enumerate() {
        let element_path = input_path.append(JSON::Number(index.into()));

        // Rebind `$acc` for this step only. The frame is scoped to the
        // iteration so the borrow of `acc` ends before it is reassigned, and
        // so the binding never escapes to the method's tail — unlike `->as`,
        // `->reduce` does not leak its variable to what follows.
        let (updated_opt, update_errors) = {
            let mut frame = vars.clone();
            frame.insert(
                KnownVariable::Local(acc_name.clone()),
                (&acc, acc_path.clone()),
            );
            update_arg.apply_to_path(element, &frame, &element_path, context)
        };
        errors.extend(update_errors);

        match updated_opt {
            Some(updated) => acc = updated,
            None => {
                // Carrying the previous accumulator forward would hand back a
                // plausible-looking number that quietly ignored an element, so
                // a failed step fails the whole fold. The errors collected
                // above say which element and why.
                return (None, errors);
            }
        }
    }

    (Some(acc), errors)
}

/// Validate `->reduce`'s arguments, shared by [`reduce_method`] and
/// [`reduce_shape`] so the runtime and the shape pass agree about what is
/// well-formed. Returns the accumulator variable name (including its leading
/// `$`) when the first argument is a single bare `$variable`, as `->as` requires
/// of its own first argument.
fn check_method_args(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    // Not meaningful for reduce_shape.
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<String>, Vec<ApplyToError>) {
    let mut errors = vec![];

    let arg_count = method_args.map(|args| args.args.len()).unwrap_or_default();
    if arg_count != 3 {
        errors.push(ApplyToError::new(
            format!(
                "Method ->{} requires three arguments: a $variable for the accumulator, an initial value, and an update expression (got {arg_count})",
                method_name.as_ref(),
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
    }

    let Some(var_arg) = method_args.and_then(|args| args.args.first()) else {
        return (None, errors);
    };

    let mut not_a_variable = |suffix: &str| {
        errors.push(ApplyToError::new(
            format!(
                "First argument to ->{} must be a single $variable name{suffix}",
                method_name.as_ref(),
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
    };

    let LitExpr::Path(path_selection) = var_arg.as_ref() else {
        not_a_variable("");
        return (None, errors);
    };
    let PathList::Var(known_var, tail) = path_selection.path.as_ref() else {
        not_a_variable("");
        return (None, errors);
    };
    if !matches!(tail.as_ref(), PathList::Empty) {
        not_a_variable(" with no path suffix");
        return (None, errors);
    }

    match known_var.as_ref() {
        // The parser rewrites this argument to a Local when it sees a
        // `->reduce` call, exactly as it does for `->as`, so a Local here is
        // the well-formed case.
        KnownVariable::Local(var_name) => (Some(var_name.clone()), errors),

        // `@` and `$` are the language's own bindings, and rebinding either
        // per iteration would make the update expression unreadable.
        KnownVariable::Dollar | KnownVariable::AtSign => {
            errors.push(ApplyToError::new(
                format!(
                    "First argument to ->{} must be a named $variable, not {}",
                    method_name.as_ref(),
                    known_var.as_str(),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            ));
            (None, errors)
        }

        KnownVariable::External(var_name) => {
            errors.push(ApplyToError::new(
                format!(
                    "Argument {} to ->{} must not be an external variable",
                    var_name, // Includes the leading $
                    method_name.as_ref(),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            ));
            (None, errors)
        }
    }
}

/// The accumulator's static shape is a fixpoint: `$acc` starts as the seed, and
/// every step replaces it with the update's output computed against that same
/// `$acc`. Rather than iterate to convergence, take one widening step —
/// `seedShape ⊔ updateOutputShape` — and then *check* that it is the fixpoint by
/// applying the update once more against it.
///
/// The check is what makes the single step safe. It holds for every fold whose
/// accumulator type settles, which is nearly all of them and exactly what the
/// seed exists to pin. It fails for an update that grows the accumulator
/// structurally — `->reduce($acc, 0, [$acc])` wraps it one array deeper per
/// element — where one step would report `One<0, [0]>` while the runtime
/// produces arbitrarily deep nesting. That is not an imprecise shape but a wrong
/// one, so a failed check widens all the way to `Unknown` rather than pretending
/// to a bound the fold does not respect.
///
/// The seed joins the union only when the array might be empty, since the seed
/// is the result in exactly that case. When the input shape's array prefix
/// guarantees at least one element, the fold body always runs, and including the
/// seed would invent a case the caller then has to handle.
///
/// The element shape is the union of the input array's item shapes
/// (`any_item`), so a heterogeneous array widens the update's input rather than
/// analyzing each position separately — the fold's result comes from the last
/// element, but nothing here knows which shape that is. A non-array input yields
/// itself, mirroring the runtime's one-element fold.
fn reduce_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let (acc_name_opt, errors) = check_method_args(
        method_name,
        method_args,
        &InputPath::empty(),
        context.spec(),
    );

    if !errors.is_empty() {
        return Shape::error(
            errors
                .iter()
                .map(|e| e.message().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            method_name.shape_location(context.source_id()),
        );
    }

    let (Some(acc_name), Some(args)) = (acc_name_opt, method_args) else {
        return Shape::error(
            format!("Method ->{} could not be analyzed", method_name.as_ref()),
            method_name.shape_location(context.source_id()),
        );
    };
    let (Some(seed_arg), Some(update_arg)) = (args.args.get(1), args.args.get(2)) else {
        return Shape::error(
            format!("Method ->{} could not be analyzed", method_name.as_ref()),
            method_name.shape_location(context.source_id()),
        );
    };

    let seed_shape =
        seed_arg.compute_output_shape(context, input_shape.clone(), dollar_shape.clone());
    let element_shape = input_shape.any_item([]);
    let locations = || method_name.shape_location(context.source_id());

    // An array whose static prefix has at least one entry cannot be empty, so
    // the fold body is guaranteed to run and the seed cannot be the result.
    let may_be_empty = !matches!(
        input_shape.case(),
        ShapeCase::Array { prefix, .. } if !prefix.is_empty()
    );

    let update_once = |acc_shape: &Shape| {
        let update_context = context
            .clone()
            .with_named_shapes([(acc_name.clone(), acc_shape.clone())]);
        update_arg.compute_output_shape(
            &update_context,
            element_shape.clone(),
            dollar_shape.clone(),
        )
    };

    // One widening step, from the accumulator's initial shape.
    let first = update_once(&seed_shape);
    let candidate = if may_be_empty {
        Shape::one([seed_shape, first], locations())
    } else {
        first
    };

    // Then confirm it is the fixpoint. If feeding the candidate back through the
    // update yields something it does not already accept, the accumulator grows
    // without bound and no finite shape describes it.
    let second = update_once(&candidate);
    if candidate.accepts(&second) {
        candidate
    } else {
        Shape::unknown(locations())
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::connectors::ConnectSpec;
    use crate::selection;

    const SPEC: ConnectSpec = ConnectSpec::V0_5;

    #[test]
    fn sums_an_array_of_numbers() {
        assert_eq!(
            selection!("$->reduce($acc, 0, @->add($acc))", SPEC).apply_to(&json!([1, 2, 3, 4])),
            (Some(json!(10)), vec![]),
        );
    }

    #[test]
    fn returns_the_seed_for_an_empty_array() {
        // The seed is what makes the fold total: no elements, no problem.
        assert_eq!(
            selection!("$->reduce($acc, 0, @->add($acc))", SPEC).apply_to(&json!([])),
            (Some(json!(0)), vec![]),
        );
    }

    #[test]
    fn folds_over_a_selected_field() {
        assert_eq!(
            selection!("total: prices->reduce($acc, 0, @->add($acc))", SPEC).apply_to(&json!({
                "prices": [10, 20, 5],
            })),
            (Some(json!({ "total": 35 })), vec![]),
        );
    }

    #[test]
    fn accumulator_need_not_have_the_element_type() {
        // Strings in, a number out: the seed pins the accumulator's type
        // independently of the elements, which is what a seedless reduction
        // could not do.
        assert_eq!(
            selection!("$->reduce($acc, 0, @->size->add($acc))", SPEC)
                .apply_to(&json!(["a", "bb", "ccc"])),
            (Some(json!(6)), vec![]),
        );
    }

    #[test]
    fn accumulator_is_visible_to_the_update_only() {
        // `$acc` does not leak to the method's tail the way `->as` binds do.
        let (value, errors) =
            selection!("$->reduce($acc, 0, @->add($acc))->echo($acc)", SPEC).apply_to(&json!([1]));
        assert_eq!(value, None);
        assert_eq!(
            errors
                .iter()
                .map(|e| e.message().to_string())
                .collect::<Vec<_>>(),
            vec!["Variable $acc not found".to_string()],
        );
    }

    #[test]
    fn non_array_input_folds_as_a_single_element() {
        assert_eq!(
            selection!("$->reduce($acc, 100, @->add($acc))", SPEC).apply_to(&json!(5)),
            (Some(json!(105)), vec![]),
        );
    }

    #[test]
    fn leading_with_the_accumulator_is_the_documented_trap() {
        // `$acc->add(@)` retargets the cursor onto the accumulator, so `@` is
        // the accumulator too and the array is never read: 0+0, four times.
        // This test pins the behavior so a future change to it is deliberate.
        assert_eq!(
            selection!("$->reduce($acc, 0, $acc->add(@))", SPEC).apply_to(&json!([1, 2, 3, 4])),
            (Some(json!(0)), vec![]),
        );
    }

    #[test]
    fn rejects_a_non_variable_first_argument() {
        let (value, errors) = selection!("$->reduce(123, 0, @)", SPEC).apply_to(&json!([1, 2, 3]));
        assert_eq!(value, None);
        assert_eq!(
            errors
                .iter()
                .map(|e| e.message().to_string())
                .collect::<Vec<_>>(),
            vec!["First argument to ->reduce must be a single $variable name".to_string()],
        );
    }

    #[test]
    fn rejects_the_wrong_number_of_arguments() {
        let (value, errors) = selection!("$->reduce($acc, 0)", SPEC).apply_to(&json!([1, 2, 3]));
        assert_eq!(value, None);
        assert_eq!(
            errors
                .iter()
                .map(|e| e.message().to_string())
                .collect::<Vec<_>>(),
            vec![
                "Method ->reduce requires three arguments: a $variable for the accumulator, an initial value, and an update expression (got 2)"
                    .to_string()
            ],
        );
    }

    #[test]
    fn nested_folds_keep_their_own_accumulators() {
        assert_eq!(
            selection!(
                "$->reduce($outer, 0, @->reduce($inner, $outer, @->add($inner)))",
                SPEC
            )
            .apply_to(&json!([[1, 2], [3, 4]])),
            (Some(json!(10)), vec![]),
        );
    }

    #[test]
    fn sum_shape_is_the_widened_accumulator() {
        // `seedShape ⊔ updateOutputShape` = Int ⊔ Float. The union collapses to
        // Float because Int is subsumed by it — which is the widening step
        // doing its job, not a loss of precision from skipping the fixpoint.
        assert_eq!(
            selection!("$->reduce($acc, 0, @->add($acc))", SPEC)
                .shape()
                .pretty_print(),
            "Float",
        );
    }

    #[test]
    fn shape_omits_the_seed_when_the_array_cannot_be_empty() {
        // A statically non-empty array always runs the fold, so the seed is not
        // a possible result and must not appear in the output shape. (The
        // element shapes are all present because the analysis does not know
        // which element is last.)
        assert_eq!(
            selection!("$([1, 2, 3])->reduce($acc, null, @)", SPEC)
                .shape()
                .pretty_print(),
            "One<1, 2, 3>",
        );
    }

    #[test]
    fn shape_keeps_the_seed_when_the_array_might_be_empty() {
        // Nothing is known about `list`, so the empty case is live and the seed
        // is a genuine possibility.
        assert_eq!(
            selection!("list->reduce($acc, null, @)", SPEC)
                .shape()
                .pretty_print(),
            "One<null, $root.list.*>",
        );
    }

    #[test]
    fn shape_gives_up_on_an_accumulator_that_grows_each_step() {
        // `[$acc]` wraps the accumulator one array deeper per element, so no
        // finite shape describes the result. One widening step would claim
        // `One<0, [0]>`, which the runtime disproves at three elements
        // (`[[[0]]]`), so the stability check widens to Unknown instead.
        let sel = selection!("$->reduce($acc, 0, [$acc])", SPEC);
        assert_eq!(sel.shape().pretty_print(), "Unknown");
        assert_eq!(
            sel.apply_to(&json!([1, 2, 3])),
            (Some(json!([[[0]]])), vec![]),
        );
    }

    #[test]
    fn shape_reports_a_type_changing_accumulator_as_a_union() {
        // Seeded with a string, updated to an int: the one widening step keeps
        // both rather than silently claiming the seed's type.
        assert_eq!(
            selection!("$->reduce($acc, '', @->size)", SPEC)
                .shape()
                .pretty_print(),
            "One<\"\", Int>",
        );
    }
}
