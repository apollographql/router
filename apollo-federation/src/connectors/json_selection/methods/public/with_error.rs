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
/// Returns its input unmodified, but records an [`ApplyToError`] built from
/// the method's argument, evaluated against the input value (so `@` refers to
/// the value flowing through). Together with conditional methods like
/// `->match`, this lets a mapping attach diagnostics to values without
/// interrupting them:
///
/// ```text
/// status: type_code->match(
///     ["2", "VAN"],
///     [@, @->withError("Unrecognized type code")]
/// )
/// ```
///
/// The diagnostic is addressed to the **mapping author**: it reaches the
/// connectors debugger and telemetry, and never a client. An error a schema
/// author intends a client to read is declared with
/// [`->withConnectorError`](super::WithConnectorErrorMethod) instead, which is a
/// separate method because it is a separate audience, not a separate spelling.
///
/// # The argument
///
/// Exactly one, of any type. A string is the message as written; every other
/// value is JSON-encoded into it, exactly as `->jsonStringify` would encode it,
/// so a structured value survives legibly:
///
/// ```text
/// @->withError(@.type_code)
/// ```
///
/// One argument rather than several deliberately. A variadic form would have to
/// answer how its arguments become one sentence, which means a separator and a
/// rendering rule the language would be committing to forever. It buys nothing:
/// several diagnostics about one value are several calls, and they compose
/// because this method passes its input through.
///
/// ```text
/// @->withError("Code is unrecognized")->withError(@.audit)
/// ```
///
/// An author who wants data inside one sentence builds the sentence, with the
/// primitive the language already has for it:
///
/// ```text
/// @->withError(["Unrecognized type code:", @.type_code]->joinNotNull(" "))
/// ```
///
/// # Failure
///
/// The input flows through unchanged either way, so the tail applies to it
/// exactly as if the method were absent. **A failed argument costs the message,
/// never the value.** Whether the argument evaluated is a fact about the
/// diagnostic, not about the field: a method whose whole purpose is to record
/// something without interrupting the value must not interrupt the value when
/// its own argument misses. The alternative deletes the field the author was
/// annotating, and does it most often to `x ?? $(default)->withError(...)`,
/// which exists precisely to keep a value in place.
///
/// Two errors are reported when the argument fails: the one from evaluating it,
/// saying why it produced nothing, and a distinct one from this method, saying
/// that the message it was asked to record never happened. That second error
/// carries the syntax of the call and of the argument, so a discarded
/// diagnostic can be traced back to the expression meant to produce it.
///
/// An author who wants the message recorded even when a path may be missing
/// says so with `??`, which supplies a value where there would have been none:
///
/// ```text
/// @->withError(["type code:", @.type_code ?? "<absent>"]->joinNotNull(" "))
/// ```
///
/// That spells the absence out in the message text instead of losing the
/// message to it.
///
/// Note that this is a different trap from a missing *input*. `->withError` can
/// only annotate a value that exists: in `@.missing->withError("...")` the
/// chain aborts before the method runs, so nothing is recorded at all. Saying
/// something about an absent field means supplying a value first, as in
/// `@.missing ?? $(null)->withError("...")`.
fn with_error_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let args = method_args.map_or(&[][..], |method_args| method_args.args.as_slice());

    // The call's own syntax, so a discarded message can be traced back to the
    // expression that was supposed to produce it. Behind a closure because
    // pretty-printing allocates and only a failing argument ever reads it, and
    // this method is meant to be cheap enough to drop into a ->map over a large
    // array.
    let printed_args = || {
        method_args
            .map(|method_args| method_args.pretty_print_with_indentation(true, 0))
            .unwrap_or_default()
    };

    let [arg] = args else {
        // A malformed call produces no value, the same as any other method
        // called wrongly. The shape function reports this too, so a schema this
        // wrong does not compose in the first place.
        return (
            None,
            vec![ApplyToError::new(
                format!(
                    "Method ->{}{} requires exactly one argument, the value to record \
                     as the message, got {}",
                    method_name.as_ref(),
                    printed_args(),
                    args.len(),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    let (value_opt, mut errors) = arg.apply_to_path(data, vars, input_path, spec);

    // Every path below returns the input: the value's fate depends on the
    // value, never on whether the message describing it could be built.
    let unchanged = Some(data.clone());

    let Some(value) = value_opt else {
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
        return (unchanged, errors);
    };

    // A string is the message as written, so prose reads as prose rather than
    // arriving wrapped in quotes. Everything else is JSON-encoded, the way
    // ->jsonStringify would encode it. Encoding a `JSON` cannot fail: it holds
    // no value serde_json rejects, which is why there is no error branch here.
    let message = match &value {
        JSON::String(string) => string.as_str().to_string(),
        value => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    };

    errors.push(ApplyToError::new(
        message,
        input_path.to_vec(),
        method_args.and_then(Ranged::range),
        spec,
    ));

    (unchanged, errors)
}

// The output shape is the input shape: this method is an identity function on
// the value. The argument's shape is still computed so mistakes inside it
// (unknown fields, mistyped paths) surface at validation time. Its shape is
// otherwise unconstrained, since any value can become a diagnostic message.
fn with_error_shape(
    context: &ShapeContext,
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    input_shape: Shape,
    dollar_shape: Shape,
) -> Shape {
    let args = method_args.map_or(&[][..], |method_args| method_args.args.as_slice());

    let [arg] = args else {
        return Shape::error(
            format!(
                "Method ->{}{} requires exactly one argument, the value to record \
                 as the message, got {}",
                method_name.as_ref(),
                method_args
                    .map(|method_args| method_args.pretty_print_with_indentation(true, 0))
                    .unwrap_or_default(),
                args.len(),
            ),
            method_name.shape_location(context.source_id()),
        );
    };

    let arg_shape = arg.compute_output_shape(context, input_shape.clone(), dollar_shape);
    if matches!(arg_shape.case(), ShapeCase::Error(_)) {
        return arg_shape;
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

    use crate::assert_snapshot;
    use crate::connectors::JSONSelection;
    use crate::connectors::json_selection::ApplyToError;
    use crate::connectors::json_selection::ApplyToErrorKind;
    use crate::selection;

    /// Every way this method can be called wrongly, and the exact text an
    /// author is told. The companion to the same snapshot in
    /// `with_connector_error.rs`, and worth reading beside it: the two methods
    /// deliberately differ in strictness, because a diagnostic is read only by
    /// its author while a declared error reaches a client, and the difference
    /// should be visible in what they say.
    #[test]
    fn every_with_error_diagnostic() {
        let cases = [
            ("no arguments", r#"$->withError"#),
            ("two arguments", r#"$->withError("a", "b")"#),
            (
                "an argument that produces nothing",
                r#"$->withError(@.nope)"#,
            ),
            (
                "a non-string argument, which is serialized rather than refused",
                r#"$->withError($(42))"#,
            ),
        ];

        let mut report = String::new();
        for (label, selection) in cases {
            let (_, errors) = selection!(selection).apply_to(&json!({ "id": 1 }));
            report.push_str(&format!("{label}\n  {selection}\n"));
            for error in &errors {
                report.push_str(&format!("    [{:?}] {}\n", error.kind(), error.message()));
            }
            report.push('\n');
        }

        assert_snapshot!(report);
    }

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

    /// What it records is addressed to the mapping author, not to a client, so
    /// it is a diagnostic. A client-facing error is a different method.
    #[test]
    fn with_error_records_a_diagnostic_and_never_a_declared_error() {
        let (_, errors) = selection!(r#"$->withError("anything")"#).apply_to(&json!(null));

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind(), ApplyToErrorKind::Diagnostic);
    }

    /// A string is the message as written; every other value is JSON-encoded
    /// into it, so a structured value survives legibly.
    #[test]
    fn with_error_should_json_encode_a_non_string_message() {
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

    /// One argument, one diagnostic. Several diagnostics about one value are
    /// several calls, which compose because the method passes its input
    /// through — so the language never has to specify how several arguments
    /// would become one sentence.
    #[test]
    fn several_diagnostics_about_one_value_are_several_calls() {
        let (value, errors) =
            selection!(r#"$->withError("code is unrecognized")->withError(@.type_code)"#)
                .apply_to(&json!({ "type_code": 7 }));

        assert_eq!(value, Some(json!({ "type_code": 7 })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["code is unrecognized", "7"],
        );
    }

    /// Building a sentence out of data is the language's job, not this
    /// method's: `->joinNotNull` already exists and lets the author choose the
    /// separator, which a built-in concatenation would have decided for them.
    #[test]
    fn a_message_can_be_built_from_parts_with_join_not_null() {
        let (value, errors) = selection!(
            r#"$->withError(["Unrecognized type code:", @.type_code]->joinNotNull(" ")) { id }"#
        )
        .apply_to(&json!({ "id": "acct-1", "type_code": 7 }));

        assert_eq!(value, Some(json!({ "id": "acct-1" })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["Unrecognized type code: 7"],
        );
    }

    /// A failed argument costs the message and both halves of why are
    /// reported: the argument's own failure, and the fact that it cost the
    /// message.
    ///
    /// The value survives regardless. A failed argument is a fact about the
    /// diagnostic, not about the field, and a method that exists to annotate a
    /// value without interrupting it must not delete that value when its own
    /// argument misses.
    #[test]
    fn a_failed_argument_costs_the_message_and_not_the_value() {
        let (value, errors) = selection!(r#"$->withError(@.nope)"#).apply_to(&json!({ "id": 1 }));

        assert_eq!(value, Some(json!({ "id": 1 })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![
                "Property .nope not found in object",
                concat!(
                    "Method ->withError(@.nope) recorded no message ",
                    "because argument @.nope produced no value",
                ),
            ],
        );
    }

    /// The shape this matters most for: a field defaulted with `??` whose
    /// diagnostic references a path that is not there. Deleting the value on a
    /// failed argument would take the default down with it, so the field the
    /// author was defending would be the one that vanished.
    #[test]
    fn a_defaulted_field_survives_a_diagnostic_whose_argument_fails() {
        let (value, errors) =
            selection!(r#"balance: $.amount ?? $("<missing>")->withError(@.nope)"#)
                .apply_to(&json!({ "id": "acct-1" }));

        assert_eq!(value, Some(json!({ "balance": "<missing>" })));
        assert!(
            errors
                .iter()
                .all(|error| error.kind() == ApplyToErrorKind::Diagnostic),
            "a message that was never recorded must not be reported as declared",
        );
    }

    /// The way to keep the message as well as the value: `??` supplies a value
    /// where the path would otherwise produce none, so an author who *wants*
    /// the message even when a path is missing says so, and gets the absence
    /// spelled out in the text rather than losing the message. `??` also
    /// swallows the failed path's own error, since the fallback counts as a
    /// successful evaluation.
    #[test]
    fn with_error_should_accept_a_coalesced_argument_in_place_of_a_missing_one() {
        let (value, errors) =
            selection!(r#"$->withError(@.nope ?? "<absent>")"#).apply_to(&json!({ "id": 1 }));

        assert_eq!(value, Some(json!({ "id": 1 })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["<absent>"],
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

        let nullish = selection!(r#"$->withError(@.code ?? "<absent>")"#);
        assert_eq!(recorded(&nullish, &absent), vec!["<absent>"]);
        assert_eq!(recorded(&nullish, &null), vec!["<absent>"]);

        let none_only = selection!(r#"$->withError(@.code ?! "<absent>")"#);
        assert_eq!(recorded(&none_only, &absent), vec!["<absent>"]);
        assert_eq!(recorded(&none_only, &null), vec!["null"]);
    }

    /// An argument can produce a value *and* report errors from inside it, so
    /// "did anything go wrong" is not the same question as "can the message be
    /// recorded". Here the subselection misses a field on one array element and
    /// still yields a value, and the author's message is recorded alongside
    /// that error rather than being discarded by it.
    #[test]
    fn with_error_should_record_a_message_whose_argument_also_reported_errors() {
        let (value, errors) =
            selection!(r#"$->withError(@.rows { id }) { count }"#).apply_to(&json!({
                "count": 2,
                "rows": [{ "id": "a" }, { "name": "b" }],
            }));

        assert_eq!(value, Some(json!({ "count": 2 })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["Property .id not found in object", r#"[{"id":"a"},{}]"#,],
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
            selection!(r#"dollars: cents->withError(@)->div(100)"#).apply_to(&data);

        assert_eq!(tapped, untapped);
        assert_eq!(no_errors, vec![]);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["250"],
        );
    }

    /// A message can draw on the mapping's variables and not only on the value
    /// flowing through, which is what lets a tap name the request that produced
    /// the response it is complaining about.
    #[test]
    fn with_error_should_evaluate_variables_inside_a_message() {
        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "id": "acct-1" }));

        let (value, errors) = selection!(r#"$->withError($args.id)"#)
            .apply_with_vars(&json!({ "region": "us-east" }), &vars);

        assert_eq!(value, Some(json!({ "region": "us-east" })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["acct-1"],
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
                    "message": concat!(
                        "Method ->withError requires exactly one argument, ",
                        "the value to record as the message, got 0",
                    ),
                    "path": ["->withError"],
                    "range": [3, 12],
                }))],
            ),
        );
    }

    /// Likewise a call with several. The arity is one because the message is
    /// one value, and a second argument has no meaning to fall back on.
    #[test]
    fn with_error_should_report_a_call_with_several_arguments() {
        let (value, errors) = selection!(r#"$->withError("a", "b")"#).apply_to(&json!("value"));

        assert_eq!(value, None);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![concat!(
                r#"Method ->withError("a", "b") requires exactly one argument, "#,
                "the value to record as the message, got 2",
            )],
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
