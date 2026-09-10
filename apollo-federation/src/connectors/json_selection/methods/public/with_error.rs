use serde_json_bytes::Value as JSON;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::json_selection::ApplyToError;
use crate::connectors::json_selection::ApplyToInternal;
use crate::connectors::json_selection::MethodArgs;
use crate::connectors::json_selection::PrettyPrintable;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::json_selection::VarsWithPathsMap;
use crate::connectors::json_selection::immutable::InputPath;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;
use crate::connectors::spec::ConnectSpec;
use crate::impl_arrow_method;

impl_arrow_method!(WithErrorMethod, with_error_method, with_error_shape);
/// Returns its input unmodified, but records an [`ApplyToError`] built from the
/// method's arguments, each evaluated against the input value (so `@` refers to
/// the value flowing through). Together with conditional methods like
/// `->match`, this lets a mapping attach errors to values without interrupting
/// them:
///
///     status: type_code->match(
///         ["2", $("VAN")],
///         [@, @->withError("Unrecognized type code")]
///     )
///
/// Any number of arguments is allowed, and they may be of any type, in the
/// spirit of `console.log`: string arguments are interpolated as written, every
/// other value is serialized exactly as `->jsonStringify` would serialize it,
/// and the results are joined with single spaces into one message. So a
/// diagnostic can carry the offending value along with the prose describing it:
///
///     @->withError("Unrecognized type code:", @.type_code, "in", @.id)
///
/// If every argument produces a value, the input flows through unchanged, so
/// the tail applies to it exactly as if the method were absent. If any argument
/// produces no value the method produces none either, short-circuiting without
/// recording the author's message, the way every other method in the language
/// propagates an absent argument. Two errors are reported for each argument
/// that fails: the one from evaluating it, saying why it produced nothing, and
/// a distinct one from this method, saying that the message it was asked to
/// record never happened. That second error carries the syntax of the call and
/// of the argument that failed, so a discarded diagnostic can be traced back to
/// the expression meant to produce it. Every argument is evaluated either way,
/// so an author who broke more than one hears about all of them.
///
/// An author who wants the message reported even when a path may be missing
/// says so with `??`, which supplies a value where there would have been none:
///
///     @->withError("Unrecognized type code:", @.type_code ?? "<absent>")
///
/// That spells the absence out in the message text instead of losing the whole
/// message to it.
fn with_error_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    if method_args.is_none_or(|method_args| method_args.args.is_empty()) {
        // A malformed call produces no value, the same as any other method
        // called wrongly. The shape function reports this too, so a schema this
        // wrong does not compose in the first place.
        return (
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
        );
    }

    let args = method_args.map_or(&[][..], |method_args| method_args.args.as_slice());

    let mut errors = Vec::new();
    let mut parts = Vec::with_capacity(args.len());

    // Whether the author's message can still be recorded. An argument that
    // produces no value, or a value that cannot be serialized, costs the whole
    // message, but the loop runs to the end regardless, so an author who broke
    // two arguments hears about both rather than only the first. This is tracked
    // separately because it cannot be read off `errors`: an argument can produce
    // a value and report errors from inside it (a subselection over an array
    // where one element lacks a field, say), and the message is still recorded
    // in that case.
    let mut can_record_message = true;

    // The call's own syntax, so a discarded message can be traced back to the
    // expression that was supposed to produce it. Without this, an author
    // reading the problems sees that some ->withError somewhere reported
    // nothing, which is exactly the opacity the method exists to avoid. Behind a
    // closure because pretty-printing allocates and only a failing argument ever
    // reads it, and this method is meant to be cheap enough to drop into a ->map
    // over a large array.
    let printed_args = || {
        method_args
            .map(|method_args| method_args.pretty_print_with_indentation(true, 0))
            .unwrap_or_default()
    };

    for arg in args {
        let (value_opt, arg_errors) = arg.apply_to_path(data, vars, input_path, spec);
        errors.extend(arg_errors);

        match value_opt {
            // A string argument is interpolated as written, so prose reads
            // as prose rather than arriving wrapped in quotes.
            Some(JSON::String(string)) => parts.push(string.as_str().to_string()),

            // Everything else is serialized the way ->jsonStringify would
            // serialize it, so a structured argument survives legibly.
            Some(value) => match serde_json::to_string(&value) {
                Ok(json) => parts.push(json),
                Err(err) => {
                    can_record_message = false;
                    errors.push(ApplyToError::new(
                        format!(
                            "Method ->{}{} recorded no message because argument {} could not be serialized: {err}",
                            method_name.as_ref(),
                            printed_args(),
                            arg.pretty_print_with_indentation(true, 0),
                        ),
                        input_path.to_vec(),
                        arg.range(),
                        spec,
                    ));
                }
            },

            // An absent argument makes the whole method absent, the way it
            // does for every other method. arg_errors (already collected)
            // say why the argument produced nothing, and the error added
            // here says what that cost: the message the author asked for
            // was never recorded. Without it, a mapping author reading the
            // problems sees only a failed path and has no reason to connect
            // it to their missing diagnostic.
            None => {
                can_record_message = false;
                errors.push(ApplyToError::new(
                    format!(
                        "Method ->{}{} recorded no message because argument {} produced no value",
                        method_name.as_ref(),
                        printed_args(),
                        arg.pretty_print_with_indentation(true, 0),
                    ),
                    input_path.to_vec(),
                    arg.range(),
                    spec,
                ));
            }
        }
    }

    if !can_record_message {
        return (None, errors);
    }

    errors.push(ApplyToError::new(
        parts.join(" "),
        input_path.to_vec(),
        method_args.and_then(Ranged::range),
        spec,
    ));

    // Every argument produced a value, so the input flows through unchanged and
    // the dispatcher applies the tail to it.
    (Some(data.clone()), errors)
}

// The output shape is the input shape: this method is an identity function on
// the value. Every argument's shape is still computed so mistakes inside them
// (unknown fields, mistyped paths) surface at validation time. No argument's
// shape is constrained, since any value can be part of a message.
fn with_error_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let args = method_args.map_or(&[][..], |method_args| method_args.args.as_slice());

    if args.is_empty() {
        return Shape::error(
            format!(
                "Method ->{} requires at least one argument",
                method_name.as_ref()
            ),
            method_name.shape_location(context.source_id()),
        );
    }

    for arg in args {
        let arg_shape =
            arg.compute_output_shape(context, input_shape.clone(), dollar_shape.clone());
        if matches!(arg_shape.case(), ShapeCase::Error(_)) {
            return arg_shape;
        }
    }

    input_shape
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::collections::IndexSet;
    use apollo_compiler::executable::SelectionSet;
    use apollo_compiler::validation::Valid;
    use pretty_assertions::assert_eq;
    use serde_json_bytes::Value as JSON;
    use serde_json_bytes::json;

    use crate::connectors::JSONSelection;
    use crate::connectors::json_selection::ApplyToError;
    use crate::selection;

    /// Apply `selection` to `data`, assert the value flowed through unchanged,
    /// and hand back the messages that were recorded.
    fn recorded(selection: &JSONSelection, data: &JSON) -> Vec<String> {
        let (value, errors) = selection.apply_to(data);
        assert_eq!(
            value.as_ref(),
            Some(data),
            "->withError must leave the value alone",
        );
        errors
            .iter()
            .map(|error| error.message().to_string())
            .collect()
    }

    #[test]
    fn with_error_should_return_input_and_record_error() {
        assert_eq!(
            selection!("$->withError('This is an error')").apply_to(&json!(null)),
            (
                Some(json!(null)),
                vec![ApplyToError::from_json(&json!({
                    "message": "This is an error",
                    "path": ["->withError"],
                    "range": [12, 32],
                }))],
            ),
        );
    }

    #[test]
    fn with_error_should_stringify_non_string_messages() {
        assert_eq!(
            selection!("$->withError({ hi: @.name }) { name }").apply_to(&json!({
                "name": "Alice",
            })),
            (
                Some(json!({ "name": "Alice" })),
                vec![ApplyToError::from_json(&json!({
                    "message": "{\"hi\":\"Alice\"}",
                    "path": ["->withError"],
                    "range": [12, 28],
                }))],
            ),
        );
    }

    /// Any number of arguments, of any type, in the spirit of `console.log`:
    /// strings are interpolated as written, everything else is serialized the
    /// way ->jsonStringify would serialize it, and the parts are joined with
    /// single spaces.
    #[test]
    fn with_error_should_accept_any_number_of_arguments_of_any_type() {
        let (value, errors) = selection!(
            r#"$->withError("Unrecognized type code:", @.type_code, "for", @.id, @.meta) { id }"#
        )
        .apply_to(&json!({
            "id": "acct-1",
            "type_code": 7,
            "meta": { "source": "vendor", "retryable": false },
        }));

        assert_eq!(value, Some(json!({ "id": "acct-1" })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![r#"Unrecognized type code: 7 for acct-1 {"source":"vendor","retryable":false}"#],
        );
    }

    /// An absent argument makes the whole method absent, the way it does for
    /// every other method, rather than contributing a placeholder to the
    /// message. The author's message is not recorded, and both halves of why
    /// are reported: the argument's own failure, and the fact that it cost the
    /// message.
    #[test]
    fn with_error_should_short_circuit_when_an_argument_produces_no_value() {
        let (value, errors) =
            selection!(r#"$->withError("missing:", @.nope)"#).apply_to(&json!({ "id": 1 }));

        assert_eq!(value, None);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![
                "Property .nope not found in object",
                concat!(
                    r#"Method ->withError("missing:", @.nope) recorded no message "#,
                    "because argument @.nope produced no value",
                ),
            ],
        );
    }

    /// The escape hatch for the short-circuit above, and the reason it is not a
    /// trap: `??` supplies a value for an argument that would otherwise produce
    /// none, so an author who *wants* the message even when a path is missing
    /// says so, and gets the absence spelled out in the text rather than losing
    /// the whole message. `??` also swallows the failed path's own error, since
    /// the fallback counts as a successful evaluation.
    #[test]
    fn with_error_should_accept_a_coalesced_argument_in_place_of_a_missing_one() {
        let (value, errors) = selection!(r#"$->withError("missing:", @.nope ?? "<absent>")"#)
            .apply_to(&json!({ "id": 1 }));

        assert_eq!(value, Some(json!({ "id": 1 })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["missing: <absent>"],
        );
    }

    /// `??` and `?!` say different things inside a message, and the difference
    /// is the useful part. `??` treats an explicit null the same as an absent
    /// path, so both become the fallback. `?!` fills in only for the absent
    /// one and lets a real null reach the message as `null`. An author who
    /// needs "the API omitted this field" to read differently from "the API
    /// sent null" reaches for `?!`.
    #[test]
    fn coalescing_operators_should_distinguish_an_absent_field_from_a_null_one() {
        let absent = json!({ "id": "acct-1" });
        let null = json!({ "id": "acct-1", "code": null });

        let nullish = selection!(r#"$->withError("code:", @.code ?? "<absent>")"#);
        assert_eq!(recorded(&nullish, &absent), vec!["code: <absent>"]);
        assert_eq!(recorded(&nullish, &null), vec!["code: <absent>"]);

        let none_only = selection!(r#"$->withError("code:", @.code ?! "<absent>")"#);
        assert_eq!(recorded(&none_only, &absent), vec!["code: <absent>"]);
        assert_eq!(recorded(&none_only, &null), vec!["code: null"]);
    }

    /// Every argument is evaluated even after one has already cost the
    /// message, so an author who broke two of them is told about both rather
    /// than fixing the first and rediscovering the second.
    #[test]
    fn with_error_should_report_every_argument_that_produced_no_value() {
        let (value, errors) = selection!(r#"$->withError("missing:", @.nope, "and", @.also_nope)"#)
            .apply_to(&json!({ "id": 1 }));

        assert_eq!(value, None);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![
                "Property .nope not found in object",
                concat!(
                    r#"Method ->withError("missing:", @.nope, "and", @.also_nope) "#,
                    "recorded no message because argument @.nope produced no value",
                ),
                "Property .also_nope not found in object",
                concat!(
                    r#"Method ->withError("missing:", @.nope, "and", @.also_nope) "#,
                    "recorded no message because argument @.also_nope produced no value",
                ),
            ],
        );
    }

    /// An argument can produce a value *and* report errors from inside it, so
    /// "did anything go wrong" is not the same question as "can the message be
    /// recorded". Here the subselection misses a field on one array element and
    /// still yields a value, and the author's message is recorded alongside
    /// that error rather than being discarded by it.
    #[test]
    fn with_error_should_record_a_message_whose_argument_also_reported_errors() {
        let (value, errors) = selection!(r#"$->withError("rows:", @.rows { id }) { count }"#)
            .apply_to(&json!({
                "count": 2,
                "rows": [{ "id": "a" }, { "name": "b" }],
            }));

        assert_eq!(value, Some(json!({ "count": 2 })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![
                "Property .id not found in object",
                r#"rows: [{"id":"a"},{}]"#,
            ],
        );
    }

    /// The method records and steps aside, so dropping a tap into the middle of
    /// a chain cannot change what the chain computes. Asserted against the same
    /// selection without the tap rather than against a hardcoded result, since
    /// the claim is an equality between the two.
    #[test]
    fn with_error_should_leave_the_rest_of_the_chain_unchanged() {
        let data = json!({ "cents": 250 });

        let (untapped, no_errors) = selection!("dollars: cents->div(100)").apply_to(&data);
        let (tapped, errors) =
            selection!(r#"dollars: cents->withError("saw cents:", @)->div(100)"#).apply_to(&data);

        assert_eq!(tapped, untapped);
        assert_eq!(no_errors, vec![]);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["saw cents: 250"],
        );
    }

    /// A message can draw on the mapping's variables and not only on the value
    /// flowing through, which is what lets a tap name the request that produced
    /// the response it is complaining about.
    #[test]
    fn with_error_should_evaluate_variables_inside_a_message() {
        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "id": "acct-1" }));

        let (value, errors) =
            selection!(r#"$->withError("no balance for", $args.id, "in", @.region)"#)
                .apply_with_vars(&json!({ "region": "us-east" }), &vars);

        assert_eq!(value, Some(json!({ "region": "us-east" })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["no balance for acct-1 in us-east"],
        );
    }

    /// A call with no arguments is malformed rather than merely absent, and a
    /// malformed call produces no value, the same as any other method called
    /// wrongly. The shape function reports it as well, so this should not
    /// survive composition.
    #[test]
    fn with_error_should_report_a_call_with_no_arguments() {
        assert_eq!(
            selection!("$->withError").apply_to(&json!("value")),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->withError requires at least one argument",
                    "path": ["->withError"],
                    "range": [3, 12],
                }))],
            ),
        );
    }

    /// The ->match arms give each branch its own ->withError, so which error
    /// is recorded depends on which branch actually runs — the tap pattern
    /// this method exists for.
    #[test]
    fn with_error_should_fire_only_in_the_taken_match_branch() {
        let match_error_selection = selection!(
            r#"
            result: input->match(
                ["hi", $("hello")->withError("Ok error")],
                [@, @->withError({ "unknown": @ })]
            )
            "#
        );

        assert_eq!(
            match_error_selection.apply_to(&json!({ "input": "hi" })),
            (
                Some(json!({ "result": "hello" })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Ok error",
                    "path": ["input", "->match", "->withError"],
                    "range": [79, 91],
                }))],
            ),
        );

        assert_eq!(
            match_error_selection.apply_to(&json!({ "input": null })),
            (
                Some(json!({ "result": null })),
                vec![ApplyToError::from_json(&json!({
                    "message": "{\"unknown\":null}",
                    "path": ["input", "->match", "->withError"],
                    "range": [126, 144],
                }))],
            ),
        );
    }

    /// Parse an operation and hand back the selection set of its first root
    /// field, the same helper shape selection_set.rs's tests use.
    fn root_field_selection_set(
        schema: &Valid<Schema>,
        query: &str,
    ) -> (ExecutableDocument, SelectionSet) {
        let document = ExecutableDocument::parse_and_validate(schema, query, "./").unwrap();
        let set = document
            .operations
            .anonymous
            .as_ref()
            .unwrap()
            .selection_set
            .fields()
            .next()
            .unwrap()
            .selection_set
            .clone();
        (document.into_inner(), set)
    }

    /// Which errors a mapping reports already depends on the client's query
    /// shape, and it is worth pinning that here rather than leaving it to be
    /// discovered and mistaken for something `->withError` introduced. The
    /// router narrows a mapping to the requested selection set before running
    /// it, so an expression producing a field nobody asked for is gone before
    /// evaluation starts, and the error it would have recorded never happens.
    #[test]
    fn with_error_does_not_fire_for_a_selection_the_client_did_not_request() {
        let schema = Schema::parse_and_validate(
            r#"
            type Query { t: T }
            type T { id: ID name: String }
            "#,
            "./",
        )
        .unwrap();

        let selection =
            JSONSelection::parse("id name: full_name->withError('name is deprecated')").unwrap();
        let data = json!({ "id": "1", "full_name": "Alice" });

        // Requested: the error fires.
        let (document, set) = root_field_selection_set(&schema, "{ t { id name } }");
        let requested = selection.apply_selection_set(&IndexSet::default(), &document, &set, None);
        let (value, errors) = requested.apply_to(&data);
        assert_eq!(value, Some(json!({ "id": "1", "name": "Alice" })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["name is deprecated"],
        );

        // Not requested: the narrowed mapping no longer contains the
        // expression, so there is nothing left to record an error.
        let (document, set) = root_field_selection_set(&schema, "{ t { id } }");
        let narrowed = selection.apply_selection_set(&IndexSet::default(), &document, &set, None);
        let (value, errors) = narrowed.apply_to(&data);
        assert_eq!(value, Some(json!({ "id": "1" })));
        assert_eq!(
            errors,
            vec![],
            "a selection the client did not request must not report errors",
        );
    }
}
