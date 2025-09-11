use serde_json_bytes::Value as JSON;
use shape::Shape;

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

impl_arrow_method!(AsMethod, as_method, as_shape);
/// The `input->as($var[, expression])` method always returns `input` unmodified
/// (even when `input` is `None`, for then `->as` does not execute and the
/// result is `None`).
///
/// In addition to simply echoing the `input`, the `->as` method also assigns
/// the value of that input to a given variable (e.g. `$var`) that must be
/// provided as the first argument. Both static and runtime checks ensure this
/// argument is only ever a single variable name.
///
/// The `->as` method takes advantage of lazy method argument evaluation, so the
/// variable is not evaluated or looked up prior to being passed to the method
/// as an argument, but instead is passed as an expression that can be used to
/// declare/update the variable in question.
///
/// Because `->as` echoes its input, you can insert it relatively easily into
/// existing selection code without affecting the flow or overall results. In
/// some cases, the desired variable value will not be exactly the same as the
/// input, but may be derived from the input, so you can optionally pass an
/// expression as a second argument, using `@` to refer to the original input:
///
/// ```graphql
/// // These path selections produce equivalent output:
/// person->echo([@, @.id, @.name])
/// person->as($name, @.name)->as($id, @.id)->echo([@, $id, $name])
/// ```
///
/// As this example illustrates, when you provide an expression, the `->as`
/// method continues to return the original input value (`person` in this
/// example), not the value of the expression. However, the implementation below
/// actually returns a JSON object whose keys are variable names to be updated,
/// and whose values are the values of those named variables, so that the
/// calling code can be responsible for actually updating the variables and
/// processing the rest of the path. This outside/special handling for the
/// `->as` method saves us from adding a general ability to update variables to
/// all methods.
fn as_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let is_external_var =
        |var_name: &str| vars.contains_key(&KnownVariable::External(var_name.to_string()));

    let (var_name_opt, mut errors) =
        check_method_args(method_name, method_args, is_external_var, input_path, spec);

    // If there's a second argument, evaluate it and return that value. From the
    // developer's perspective, input->as($var, @.a.b) always returns input, but
    // since we handle the variable update elsewhere, it's more convenient to
    // return the variable's value (@.a.b) here.
    let result_opt =
        if let Some(expr_arg) = method_args.and_then(|MethodArgs { args, .. }| args.get(1)) {
            let (result_opt, mut expr_errors) =
            // Note that vars does not (and cannot) contain the new variable.
            expr_arg.apply_to_path(data, vars, input_path, spec);
            errors.append(&mut expr_errors);
            result_opt
        } else {
            Some(data.clone())
        };

    let mut map = serde_json_bytes::Map::new();
    if let (Some(var_name), Some(result)) = (var_name_opt, result_opt) {
        map.insert(var_name, result);
    }
    (Some(JSON::Object(map)), errors)
}

// Logic shared between as_method and as_shape to validate method_args. If the
// $var in input->as($var) was successfully parsed, this will return
// (Some("$var"), vec![...]). Errors may also be returned in the vector.
fn check_method_args(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    is_external_var: impl Fn(&str) -> bool,
    // Not meaningful for as_shape.
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<String>, Vec<ApplyToError>) {
    // These will become the (var_name_opt, errors) result tuple.
    let mut var_name_opt = None;
    let mut errors = vec![];

    // Check method_args to ensure they have the appropriate format (one
    // required $variable name followed by an optional expression).
    if let Some(MethodArgs { args, .. }) = method_args {
        if args.is_empty() || args.len() > 2 {
            errors.push(ApplyToError::new(
                format!(
                    "Method ->{} requires one or two arguments (got {})",
                    method_name.as_ref(),
                    args.len()
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            ));
        }

        if let Some(var_arg) = args.first() {
            match var_arg.as_ref() {
                LitExpr::Path(path) => match path.path.as_ref() {
                    PathList::Var(known_var, tail) => {
                        if !matches!(tail.as_ref(), PathList::Empty) {
                            errors.push(ApplyToError::new(
                                format!(
                                    "First argument to ->{} must be a single $variable name with no path suffix",
                                    method_name.as_ref()
                                ),
                                input_path.to_vec(),
                                method_name.range(),
                                spec,
                            ));
                        }

                        match known_var.as_ref() {
                            KnownVariable::Local(var_name) => {
                                if is_external_var(var_name) {
                                    errors.push(ApplyToError::new(
                                        format!(
                                            "Argument {} to ->{} must not shadow an external variable",
                                            var_name, // Includes the leading $
                                            method_name.as_ref()
                                        ),
                                        input_path.to_vec(),
                                        method_name.range(),
                                        spec,
                                    ));
                                } else {
                                    // Setting this option to Some(_) is
                                    // required for any variable to be updated.
                                    var_name_opt = Some(known_var.to_string());
                                }
                            }

                            // Forbid modifying the internal $ or @ variables.
                            KnownVariable::Dollar | KnownVariable::AtSign => {
                                errors.push(ApplyToError::new(
                                    format!(
                                        "First argument to ->{} must be a named $variable, not {}",
                                        method_name.as_ref(),
                                        known_var.as_str()
                                    ),
                                    input_path.to_vec(),
                                    method_name.range(),
                                    spec,
                                ));
                            }

                            KnownVariable::External(var_name) => {
                                errors.push(ApplyToError::new(
                                    format!(
                                        "Argument {} to ->{} must not be an external variable",
                                        var_name, // Includes the leading $
                                        method_name.as_ref()
                                    ),
                                    input_path.to_vec(),
                                    method_name.range(),
                                    spec,
                                ));
                            }
                        }
                    }
                    _ => {
                        errors.push(ApplyToError::new(
                            format!(
                                "First argument to ->{} must be a single $variable name",
                                method_name.as_ref()
                            ),
                            input_path.to_vec(),
                            method_name.range(),
                            spec,
                        ));
                    }
                },
                _ => {
                    // TODO Deduplicate this error with the identical one above?
                    errors.push(ApplyToError::new(
                        format!(
                            "First argument to ->{} must be a single $variable name",
                            method_name.as_ref()
                        ),
                        input_path.to_vec(),
                        method_name.range(),
                        spec,
                    ));
                }
            }
        }

        // The second argument to ->as($var, @.a.b) can be any expression, so we
        // have nothing in particular to validate here that isn't already
        // enforced by the type system (e.g. that it's a LitExpr).
    } else {
        errors.push(ApplyToError::new(
            format!(
                "Method ->{} requires one or two arguments (got 0)",
                method_name.as_ref()
            ),
            input_path.to_vec(),
            method_name.range(),
            spec,
        ));
    }

    (var_name_opt, errors)
}

/// Since `->as` always returns its input, the output shape is always the same
/// as the `input_shape`, but we can also perform static validations of the
/// variable argument.
///
/// Outwardly, `input->as($var)` always returns `input`, but as_shape (like
/// as_method) returns an object shape with keys corresponding to the variable
/// name (including the `$`) and value shapes for those named variables.
fn as_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let (var_name_opt, method_args_errors) = check_method_args(
        method_name,
        method_args,
        |var_name: &str| context.is_external_shape(var_name),
        &InputPath::empty(),
        context.spec(),
    );

    let result_shape = if let Some(expr_arg) = method_args.and_then(|args| args.args.get(1)) {
        // We can compute the shape of the expression argument, even though
        // it's not the value returned by the `->as` method.
        expr_arg.compute_output_shape(context, input_shape, dollar_shape)
    } else {
        // No second argument, so the output shape is the same as the input
        // shape.
        input_shape
    };

    let bound_vars_shape = if let Some(var_name) = var_name_opt.as_ref() {
        // Return an object shape with a key for the variable name and the
        // computed result shape as the value shape. Although there can be at
        // most one variable in this object for now, we might let ->as define
        // multiple variables in the future.
        Shape::record(
            [(var_name.clone(), result_shape)].into(),
            // No need for locations, as this is a utility/internal object.
            [],
        )
    } else {
        // Since we don't know the variable name, we can't create a
        // Shape::record with a static key, but we can record the
        // result_shape as the value of a dynamic dictionary.
        Shape::dict(result_shape, [])
    };

    if method_args_errors.is_empty() {
        bound_vars_shape
    } else {
        Shape::error_with_partial(
            method_args_errors
                .iter()
                .map(|e| e.message().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            bound_vars_shape,
            method_name.shape_location(context.source_id()),
        )
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;
    use serde_json_bytes::json;
    use shape::Shape;
    use shape::location::SourceId;

    use crate::connectors::ApplyToError;
    use crate::connectors::ConnectSpec;
    use crate::connectors::json_selection::ShapeContext;
    use crate::selection;

    #[test]
    fn test_too_few_as_args() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("person->as", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (
                Some(json!({ "id": 1, "name": "Alice" })),
                vec![ApplyToError::new(
                    "Method ->as requires one or two arguments (got 0)".to_string(),
                    vec![json!("person"), json!("->as")],
                    Some(8..10),
                    spec,
                )],
            ),
        );

        assert_eq!(
            selection!("person->as()", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (
                Some(json!({ "id": 1, "name": "Alice" })),
                vec![ApplyToError::new(
                    "Method ->as requires one or two arguments (got 0)".to_string(),
                    vec![json!("person"), json!("->as")],
                    Some(8..10),
                    spec,
                )],
            ),
        );
    }

    #[test]
    fn test_too_few_as_args_shape() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("person->as", spec).shape().pretty_print(),
            "Error<\"Method ->as requires one or two arguments (got 0)\", $root.person>",
        );

        assert_eq!(
            selection!("person->as()", spec).shape().pretty_print(),
            "Error<\"Method ->as requires one or two arguments (got 0)\", $root.person>",
        );
    }

    #[test]
    fn test_too_many_as_args() {
        let spec = ConnectSpec::V0_3;

        // Too many arguments
        assert_eq!(
            selection!("person->as($x, @.id, @.name)", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (
                Some(json!({ "id": 1, "name": "Alice" })),
                vec![ApplyToError::new(
                    "Method ->as requires one or two arguments (got 3)".to_string(),
                    vec![json!("person"), json!("->as")],
                    Some(8..10),
                    spec,
                )],
            ),
        );
    }

    #[test]
    fn test_too_many_as_args_shape() {
        let spec = ConnectSpec::V0_3;

        // Too many arguments
        assert_eq!(
            selection!("person->as($x, @.id, @.name)", spec)
                .shape()
                .pretty_print(),
            "Error<\"Method ->as requires one or two arguments (got 3)\", $root.person>",
        );
    }

    #[test]
    fn test_invalid_as_args() {
        let spec = ConnectSpec::V0_3;

        // First argument is not a variable
        assert_eq!(
            selection!("person->as(123)", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (
                Some(json!({ "id": 1, "name": "Alice" })),
                vec![ApplyToError::new(
                    "First argument to ->as must be a single $variable name".to_string(),
                    vec![json!("person"), json!("->as")],
                    Some(8..10),
                    spec,
                )],
            ),
        );

        // First argument is not a simple variable
        assert_eq!(
            selection!("person->as($x.id)", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (
                Some(json!({ "id": 1, "name": "Alice" })),
                vec![ApplyToError::new(
                    "First argument to ->as must be a single $variable name with no path suffix"
                        .to_string(),
                    vec![json!("person"), json!("->as")],
                    Some(8..10),
                    spec,
                )],
            ),
        );

        assert_eq!(
            selection!("person->as(p)", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (
                Some(json!({ "id": 1, "name": "Alice" })),
                vec![ApplyToError::new(
                    "First argument to ->as must be a single $variable name".to_string(),
                    vec![json!("person"), json!("->as")],
                    Some(8..10),
                    spec,
                )],
            )
        );
    }

    #[test]
    fn test_invalid_as_args_shape() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("person->as(123)", spec).shape().pretty_print(),
            "Error<\"First argument to ->as must be a single $variable name\", $root.person>",
        );

        assert_eq!(
            selection!("person->as($x.id)", spec).shape().pretty_print(),
            "Error<\"First argument to ->as must be a single $variable name with no path suffix\", $root.person>",
        );

        assert_eq!(
            selection!("person->as(p)", spec).shape().pretty_print(),
            "Error<\"First argument to ->as must be a single $variable name\", $root.person>",
        );
    }

    #[test]
    fn test_basic_as_with_echo() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("person->as($p)->echo($p)", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (Some(json!({ "id": 1, "name": "Alice" })), vec![]),
        );

        assert_eq!(
            selection!("person->as($name, @.name)->echo([@, $name])", spec)
                .apply_to(&json!({ "person": { "id": 1, "name": "Alice" }})),
            (Some(json!([{ "id": 1, "name": "Alice" }, "Alice"])), vec![]),
        );
    }

    #[test]
    fn test_basic_as_with_echo_shape() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("person->as($p)->echo($p)", spec)
                .shape()
                .pretty_print(),
            "$root.person",
        );

        assert_eq!(
            selection!("person->as($name, @.name)->echo([@, $name])", spec)
                .shape()
                .pretty_print(),
            "[$root.person, $root.person.name]",
        );
    }

    #[test]
    fn test_invalid_external_var_shadowing() {
        let spec = ConnectSpec::V0_3;

        let mut vars = IndexMap::default();
        vars.insert("$yikes".to_string(), json!("external"));

        assert_eq!(
            selection!("person->as($yikes).name", spec).apply_with_vars(
                &json!({ "person": {"id": 1, "name": "Alice" }, "ext": "external" }),
                &vars,
            ),
            (
                Some(json!("Alice")),
                vec![ApplyToError::new(
                    "Argument $yikes to ->as must not shadow an external variable".to_string(),
                    vec![json!("person"), json!("->as")],
                    Some(8..10),
                    spec,
                )],
            ),
        );
    }

    #[test]
    fn test_invalid_external_var_shadowing_shape() {
        let spec = ConnectSpec::V0_3;

        let mut vars = IndexMap::default();
        vars.insert("$yikes".to_string(), Shape::dict(Shape::int([]), []));

        assert_eq!(
            selection!("person->as($yikes) { id name }", spec)
                .compute_output_shape(
                    &ShapeContext::new(SourceId::Other("JSONSelection".into()))
                        .with_spec(spec)
                        .with_named_shapes(vars),
                    Shape::record(
                        {
                            let mut map = Shape::empty_map();
                            map.insert(
                                "person".to_string(),
                                Shape::record(
                                    {
                                        let mut map = Shape::empty_map();
                                        map.insert("id".to_string(), Shape::int([]));
                                        map.insert("name".to_string(), Shape::string([]));
                                        map
                                    },
                                    [],
                                ),
                            );
                            map
                        },
                        []
                    ),
                )
                .pretty_print(),
            "Error<\"Argument $yikes to ->as must not shadow an external variable\", { id: Int, name: String }>",
        );
    }

    #[test]
    fn test_nested_loops() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!(
                r#"
                listsOfPairs: xs->map(@->as($x)->echo(ys->map([$x, @])))
            "#,
                spec
            )
            .apply_to(&json!({
                "xs": [1, 2],
                "ys": ["a", "b"]
            })),
            (
                Some(json!({
                    "listsOfPairs": [
                        [[1, "a"], [1, "b"]],
                        [[2, "a"], [2, "b"]],
                    ]
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                xs->map(@->as($x)->echo(ys->map(@->as($y)->echo([$x, $y]))))
            "#,
                spec
            )
            .apply_to(&json!({
                "xs": [1, 2],
                "ys": ["a", "b"]
            })),
            (
                Some(json!([[[1, "a"], [1, "b"]], [[2, "a"], [2, "b"]]])),
                vec![],
            ),
        );
    }

    #[test]
    fn test_nested_loop_shapes() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!(
                r#"
                listsOfPairs: xs->map(@->as($x)->echo(ys->map([$x, @])))
            "#,
                spec
            )
            .shape()
            .pretty_print(),
            "{ listsOfPairs: List<List<[$root.*.xs.*, $root.*.ys.*]>> }",
        );

        assert_eq!(
            selection!(
                r#"
                xs->map(@->as($x)->echo(ys->map(@->as($y)->echo([$x, $y]))))
            "#,
                spec
            )
            .shape()
            .pretty_print(),
            "List<List<[$root.xs.*, $root.ys.*]>>",
        );
    }

    #[test]
    fn test_invalid_sibling_path_variable_access() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!(
                r#"
                good: person->as($x) {
                    id: $x.id
                }
                bad: {
                    id: $x.id
                }
            "#,
                spec
            )
            .apply_to(&json!({
                "person": { "id": 2, "name": "Ben" },
            })),
            (
                Some(json!({
                    "good": { "id": 2 },
                    "bad": {},
                })),
                vec![ApplyToError::new(
                    "Variable $x not found".to_string(),
                    vec![],
                    Some(135..137),
                    spec,
                )],
            ),
        );
    }

    #[test]
    fn test_invalid_sibling_path_variable_access_shape() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!(
                r#"
                good: person->as($x) {
                    id: $x.id
                }
                bad: {
                    id: $x.id
                }
            "#,
                spec
            )
            .shape()
            .pretty_print(),
            "{ bad: { id: $x.id }, good: { id: $root.*.person.id } }"
        );
    }

    #[test]
    fn test_optional_as_expr_arg() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("person->as($name, @.name)->echo([@.id, $name])", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (Some(json!([1, "Alice"])), vec![]),
        );

        assert_eq!(
            selection!("person->as($oyez, 'oyez') { id name oyez: $oyez }", spec)
                .apply_to(&json!({ "person": {"id": 1, "name": "Alice" }})),
            (
                Some(json!({ "id": 1, "name": "Alice", "oyez": "oyez" })),
                vec![]
            ),
        );
    }

    #[test]
    fn test_optional_as_expr_arg_shape() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("person->as($name, @.name)->echo([@.id, $name])", spec)
                .shape()
                .pretty_print(),
            "[$root.person.id, $root.person.name]",
        );

        assert_eq!(
            selection!("person->as($oyez, 'oyez') { id name oyez: $oyez }", spec)
                .shape()
                .pretty_print(),
            "{ id: $root.person.*.id, name: $root.person.*.name, oyez: \"oyez\" }",
        );
    }
}
