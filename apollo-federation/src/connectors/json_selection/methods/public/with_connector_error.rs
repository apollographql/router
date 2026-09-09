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

impl_arrow_method!(
    WithConnectorErrorMethod,
    with_connector_error_method,
    with_connector_error_shape
);
/// Returns its input unmodified, but declares an error about it, addressed to
/// the client and reported under `extensions.connectorErrors`.
///
/// The sibling of [`->withError`](super::WithErrorMethod), and the difference
/// between them is only who reads the result. `->withError` records a
/// diagnostic for the mapping author, visible in the debugger and in telemetry.
/// `->withConnectorError` declares an error the schema author intends a client to
/// see. Writing it is the statement that this text is fit to leave the router.
///
/// ```text
/// balance: $response.balance ?? $(null)->withConnectorError("Balance unavailable")
/// ```
///
/// The field still resolves. That combination, a value in `data` and an error
/// about it, is the whole point: the author chose a default and recorded why.
/// It is also why these are not reported in `errors`. The GraphQL spec allows
/// an execution error only at a response position that is absent from `data`
/// or null, so an error about a value that is present and fine has nowhere in
/// `errors` to go. The router reports them at
/// `result.extensions.connectorErrors` instead, which is where this method's
/// name comes from: what an author writes and what a client reads are the same
/// word.
///
/// # The argument
///
/// Exactly one, and what it means is fixed by this method's name rather than by
/// its shape. Two spellings of the same thing are accepted:
///
/// * a **string**, which is the error's `message`, and
/// * an **object**, `{ message, extensions? }`, taken as written.
///
/// ```text
/// requiredField: $response.requiredField ?? $("<missing>")->withConnectorError({
///   message: "Field 'requiredField' was not found"
///   extensions: { code: "INTERNAL_SERVER_ERROR", number: 210099 }
/// })
/// ```
///
/// Everything else is an author mistake and is reported as one rather than
/// coerced. An object with no `message` declares nothing; a `message`
/// that is not a string, or `extensions` that are not an object, would reach a
/// client malformed; and a bare number or array would reach one as a message
/// reading `42`. Unlike `->withError`, which serializes any value into a
/// diagnostic only its author reads, nothing here is stringified on the
/// author's behalf.
///
/// Several errors about one value are written as several calls, which compose
/// because this method passes its input through:
///
/// ```text
/// @->withConnectorError("Code is unrecognized")->withConnectorError("Amount is negative")
/// ```
///
/// # Failure
///
/// A failed argument costs the error, never the value, exactly as in
/// `->withError`. A method that exists to annotate a value without interrupting
/// it must not delete that value when its own argument misses, and this method
/// is reached through `??` more often than not, where deleting the value would
/// destroy the very default the author supplied.
fn with_connector_error_method(
    method_name: &WithRange<String>,
    method_args: Option<&MethodArgs>,
    data: &JSON,
    vars: &VarsWithPathsMap,
    input_path: &InputPath<JSON>,
    spec: ConnectSpec,
) -> (Option<JSON>, Vec<ApplyToError>) {
    let args = method_args.map_or(&[][..], |method_args| method_args.args.as_slice());

    // The call's own syntax, so a discarded error can be traced back to the
    // expression that was supposed to produce it. Behind a closure because
    // pretty-printing allocates and only a failing call ever reads it, and this
    // method is meant to be cheap enough to drop into a ->map over a large
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
                    "Method ->{} requires exactly one argument, got {}",
                    method_name.as_ref(),
                    args.len(),
                ),
                input_path.to_vec(),
                method_name.range(),
                spec,
            )],
        );
    };

    let (value_opt, mut errors) = arg.apply_to_path(data, vars, input_path, spec);
    let args_range = method_args.and_then(Ranged::range);

    // Every path below returns the input: the value's fate depends on the
    // value, never on whether the error describing it could be built.
    let unchanged = Some(data.clone());

    let Some(value) = value_opt else {
        errors.push(ApplyToError::new(
            format!(
                "Method ->{}{} declared no error because argument {} produced no value",
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

    let (message, extensions) = match &value {
        // A string is the message. Not a shape being sniffed to choose between
        // behaviors, which is what this method exists to avoid: there is one
        // behavior, and a string is simply the short spelling of
        // `{ message: <string> }`.
        JSON::String(message) => (message.as_str().to_string(), None),

        JSON::Object(fields) => {
            let message = match fields.get("message") {
                Some(JSON::String(message)) => message.as_str().to_string(),
                other => {
                    errors.push(ApplyToError::new(
                        format!(
                            "Method ->{}{} declared no error because `message` must be a string, got {}",
                            method_name.as_ref(),
                            printed_args(),
                            json_type_name(other),
                        ),
                        input_path.to_vec(),
                        args_range,
                        spec,
                    ));
                    return (unchanged, errors);
                }
            };

            let extensions = match fields.get("extensions") {
                None => None,
                Some(extensions @ JSON::Object(_)) => Some(extensions.clone()),
                Some(other) => {
                    errors.push(ApplyToError::new(
                        format!(
                            "Method ->{}{} declared no error because `extensions` must be an object, got {}",
                            method_name.as_ref(),
                            printed_args(),
                            json_type_name(Some(other)),
                        ),
                        input_path.to_vec(),
                        args_range,
                        spec,
                    ));
                    return (unchanged, errors);
                }
            };

            // Anything else in the object is reported rather than dropped. An
            // author who wrote `{ message: ..., code: ... }` expecting `code`
            // to reach the client hears that it did not, instead of
            // discovering it from a response that is missing it. This does not
            // cost the error: it is still well-formed, just smaller than
            // intended.
            for key in fields.keys() {
                let key = key.as_str();
                if key != "message" && key != "extensions" {
                    errors.push(ApplyToError::new(
                        format!(
                            "Method ->{}{} ignored unknown field `{key}`; a connector error carries only `message` and `extensions`",
                            method_name.as_ref(),
                            printed_args(),
                        ),
                        input_path.to_vec(),
                        args_range.clone(),
                        spec,
                    ));
                }
            }

            (message, extensions)
        }

        // Deliberately not serialized into a message the way ->withError would.
        // A diagnostic reading `42` is a curiosity for its author; a
        // client-facing error reading `42` is a defect nobody chose.
        other => {
            errors.push(ApplyToError::new(
                format!(
                    "Method ->{}{} declared no error because its argument must be a string or an object with a `message`, got {}",
                    method_name.as_ref(),
                    printed_args(),
                    json_type_name(Some(other)),
                ),
                input_path.to_vec(),
                args_range,
                spec,
            ));
            return (unchanged, errors);
        }
    };

    errors.push(ApplyToError::declared(
        message,
        input_path.to_vec(),
        args_range,
        spec,
        extensions,
    ));

    (unchanged, errors)
}

/// The name of a JSON value's type, for messages that have to say what arrived
/// where something else was required.
fn json_type_name(value: Option<&JSON>) -> &'static str {
    match value {
        None => "nothing",
        Some(JSON::Null) => "null",
        Some(JSON::Bool(_)) => "a boolean",
        Some(JSON::Number(_)) => "a number",
        Some(JSON::String(_)) => "a string",
        Some(JSON::Array(_)) => "an array",
        Some(JSON::Object(_)) => "an object",
    }
}

// The output shape is the input shape: this method is an identity function on
// the value. The argument's shape is still computed so mistakes inside it
// (unknown fields, mistyped paths) surface at validation time.
fn with_connector_error_shape(
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
                "Method ->{} requires exactly one argument, got {}",
                method_name.as_ref(),
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
    use apollo_compiler::collections::IndexMap;
    use pretty_assertions::assert_eq;
    use serde_json_bytes::json;

    use crate::connectors::json_selection::ApplyToError;
    use crate::connectors::json_selection::ApplyToErrorKind;
    use crate::selection;

    /// The short spelling: a string is the message, and the result is declared,
    /// meaning an author asked for it rather than the language reporting on
    /// itself.
    #[test]
    fn a_string_argument_is_the_message() {
        let (value, errors) =
            selection!(r#"$->withConnectorError("Balance unavailable")"#).apply_to(&json!(null));

        assert_eq!(value, Some(json!(null)));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message(), "Balance unavailable");
        assert_eq!(errors[0].kind(), ApplyToErrorKind::Declared);
        assert_eq!(errors[0].extensions(), None);
    }

    /// The long spelling, and the one that carries a code to a client. The
    /// message and the extensions arrive as separate parts rather than one
    /// flattened sentence.
    #[test]
    fn an_object_argument_carries_a_message_and_extensions() {
        let (value, errors) = selection!(
            r#"$->withConnectorError({
                message: "Field 'balance' was not found"
                extensions: { code: "INTERNAL_SERVER_ERROR", number: 210099 }
            })"#
        )
        .apply_to(&json!("<missing>"));

        assert_eq!(value, Some(json!("<missing>")));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message(), "Field 'balance' was not found");
        assert_eq!(errors[0].kind(), ApplyToErrorKind::Declared);
        assert_eq!(
            errors[0].extensions(),
            Some(&json!({ "code": "INTERNAL_SERVER_ERROR", "number": 210099 })),
        );
    }

    /// `extensions` is optional, so the object form is also available to an
    /// author who simply prefers it.
    #[test]
    fn extensions_are_optional() {
        let (value, errors) =
            selection!(r#"$->withConnectorError({ message: "plain" })"#).apply_to(&json!(1));

        assert_eq!(value, Some(json!(1)));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message(), "plain");
        assert_eq!(errors[0].kind(), ApplyToErrorKind::Declared);
        assert_eq!(errors[0].extensions(), None);
    }

    /// The rule that keeps the two spellings from becoming two behaviors: an
    /// object is *always* read as an error, never serialized into a message, so
    /// one without a `message` is a mistake rather than a second meaning. This
    /// is the case the shape-sniffing form got wrong, where the same object
    /// meant an error or a message depending on its keys.
    #[test]
    fn an_object_without_a_message_is_an_author_error() {
        let (value, errors) =
            selection!(r#"$->withConnectorError({ unknown: @ })"#).apply_to(&json!("v"));

        assert_eq!(value, Some(json!("v")), "the value survives a bad argument");
        assert!(
            errors
                .iter()
                .all(|error| error.kind() == ApplyToErrorKind::Diagnostic),
            "a malformed error must not reach a client",
        );
        assert!(
            errors[0].message().contains("`message` must be a string"),
            "unexpected message: {}",
            errors[0].message(),
        );
    }

    /// A malformed error is discarded rather than handed to a client
    /// half-formed. The value still flows through: whether the error could be
    /// built says nothing about whether the field resolved.
    #[test]
    fn a_non_string_message_is_rejected() {
        let (value, errors) =
            selection!(r#"$->withConnectorError({ message: 42 })"#).apply_to(&json!("v"));

        assert_eq!(value, Some(json!("v")));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![concat!(
                "Method ->withConnectorError( { message: 42 } ) declared no error ",
                "because `message` must be a string, got a number",
            )],
        );
        assert!(
            errors
                .iter()
                .all(|e| e.kind() == ApplyToErrorKind::Diagnostic)
        );
    }

    /// Same trade for `extensions`: GraphQL says `errors[].extensions` is a
    /// map, so a scalar there cannot be forwarded and cannot be guessed at.
    #[test]
    fn non_object_extensions_are_rejected() {
        let (value, errors) =
            selection!(r#"$->withConnectorError({ message: "m", extensions: "nope" })"#)
                .apply_to(&json!(1));

        assert_eq!(value, Some(json!(1)));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![concat!(
                r#"Method ->withConnectorError( { message: "m", extensions: "nope" } ) declared no error "#,
                "because `extensions` must be an object, got a string",
            )],
        );
    }

    /// Nothing is stringified on the author's behalf here, unlike `->withError`
    /// where a JSON-encoded diagnostic is useful to the one person who reads
    /// it. A client-facing error whose message reads `42` is a defect nobody
    /// chose.
    #[test]
    fn a_bare_scalar_is_rejected() {
        let (value, errors) = selection!(r#"$->withConnectorError(42)"#).apply_to(&json!("v"));

        assert_eq!(value, Some(json!("v")));
        assert!(
            errors[0]
                .message()
                .contains("must be a string or an object with a `message`"),
            "unexpected message: {}",
            errors[0].message(),
        );
        assert!(
            errors
                .iter()
                .all(|e| e.kind() == ApplyToErrorKind::Diagnostic)
        );
    }

    /// An author who wrote a sibling of `message` expecting it to travel gets
    /// told it did not, rather than finding out from a response that is
    /// missing it. The error itself is still well-formed, so it is still
    /// declared and the value still flows through.
    #[test]
    fn unknown_fields_are_reported_but_do_not_cost_the_error() {
        let (value, errors) =
            selection!(r#"$->withConnectorError({ message: "m", code: "OOPS" })"#)
                .apply_to(&json!(1));

        assert_eq!(value, Some(json!(1)));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec![
                concat!(
                    r#"Method ->withConnectorError( { message: "m", code: "OOPS" } ) ignored unknown field "#,
                    "`code`; a connector error carries only `message` and `extensions`",
                ),
                "m",
            ],
        );
        // The complaint is the language's, the message is the author's, and
        // only the author's is eligible to reach a client.
        assert_eq!(errors[0].kind(), ApplyToErrorKind::Diagnostic);
        assert_eq!(errors[1].kind(), ApplyToErrorKind::Declared);
    }

    /// A failed argument costs the error, never the value. Reached through
    /// `??` more often than not, where deleting the value would destroy the
    /// very default the author supplied.
    #[test]
    fn a_failed_argument_costs_the_error_and_not_the_value() {
        let (value, errors) =
            selection!(r#"balance: $.amount ?? $(null)->withConnectorError(@.nope)"#)
                .apply_to(&json!({ "id": "acct-1" }));

        assert_eq!(value, Some(json!({ "balance": null })));
        assert!(
            errors
                .iter()
                .all(|error| error.kind() == ApplyToErrorKind::Diagnostic),
            "an error that was never declared must not be reported as declared",
        );
    }

    /// Several errors about one value are several calls, which compose because
    /// the method passes its input through.
    #[test]
    fn several_errors_about_one_value_are_several_calls() {
        let (value, errors) = selection!(
            r#"$->withConnectorError("Code is unrecognized")->withConnectorError("Amount is negative")"#
        )
        .apply_to(&json!("v"));

        assert_eq!(value, Some(json!("v")));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["Code is unrecognized", "Amount is negative"],
        );
        assert!(
            errors
                .iter()
                .all(|e| e.kind() == ApplyToErrorKind::Declared)
        );
    }

    /// The customer-facing shape this method exists for: a required field takes
    /// a default and records a coded error at the same time. `??`
    /// short-circuits, so the call on the right runs only when the left
    /// produced nothing — the field resolves normally, and silently, when the
    /// data is there.
    #[test]
    fn a_defaulted_field_should_record_a_coded_error_only_when_it_defaults() {
        let selection = selection!(
            r#"requiredField: value ?? $("<missing>")->withConnectorError({
                message: "Field 'requiredField' was not found"
                extensions: { code: "INTERNAL_SERVER_ERROR", number: 210099 }
            })"#
        );

        // Present: the value passes through and nothing is recorded.
        let (value, errors) = selection.apply_to(&json!({ "value": "real" }));
        assert_eq!(value, Some(json!({ "requiredField": "real" })));
        assert_eq!(errors, vec![]);

        // Absent: the field still resolves, with the default, and the coded
        // error is recorded alongside it.
        let (value, errors) = selection.apply_to(&json!({}));
        assert_eq!(value, Some(json!({ "requiredField": "<missing>" })));
        let declared = errors
            .iter()
            .filter(|error| error.kind() == ApplyToErrorKind::Declared)
            .collect::<Vec<_>>();
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].message(), "Field 'requiredField' was not found");
        assert_eq!(
            declared[0].extensions(),
            Some(&json!({ "code": "INTERNAL_SERVER_ERROR", "number": 210099 })),
        );
    }

    /// A default of `null` must still record its error. `??` steps over a null
    /// and, having run out of operands, returns that null — and it drops the
    /// errors of every operand it stepped over, which is right for the "path
    /// produced nothing" diagnostics that coalescing exists to absorb but wrong
    /// for an error the author deliberately declared. Without the carve-out,
    /// `?? $(null)->withConnectorError(...)` silently does nothing, which is the
    /// worst possible outcome for an error-reporting feature: it looks correct
    /// and reports nothing.
    #[test]
    fn a_null_default_should_still_record_its_declared_error() {
        let data = json!({ "other": 1 });

        // The value is null either way; the question is whether the error
        // survives. Both spellings must record it.
        for selection in [
            r#"f: $.field ?? $(null)->withConnectorError("boom")"#,
            r#"f: $.field ?! $(null)->withConnectorError("boom")"#,
        ] {
            let (value, errors) = selection!(selection).apply_to(&data);
            assert_eq!(value, Some(json!({ "f": null })), "for `{selection}`");
            assert_eq!(
                errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
                vec!["boom"],
                "for `{selection}`",
            );
        }
    }

    /// The other half of that carve-out: the diagnostics coalescing absorbs are
    /// still absorbed. A defaulted field must not report "Property .field not
    /// found" alongside the author's error, or every default becomes noisy.
    #[test]
    fn a_default_should_not_report_the_failed_path_as_well() {
        let (value, errors) =
            selection!(r#"f: $.field ?? $("<missing>")->withConnectorError("boom")"#)
                .apply_to(&json!({ "other": 1 }));

        assert_eq!(value, Some(json!({ "f": "<missing>" })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["boom"],
        );
    }

    /// A declared error on the losing side of a `??` survives too: the author
    /// asked to record it, and whether a later operand happened to produce a
    /// value is unrelated to that.
    #[test]
    fn a_declared_error_survives_a_later_operand_succeeding() {
        let (value, errors) = selection!(r#"f: $.a->withConnectorError("saw a") ?? "fallback""#)
            .apply_to(&json!({ "a": null }));

        assert_eq!(value, Some(json!({ "f": "fallback" })));
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["saw a"],
        );
    }

    /// Redaction, which needs no new mechanism: `$config` is in scope for
    /// response mappings, so the same selection emits detail in one
    /// environment and a safe sentence in another. Pinned here because it is
    /// the answer to "can we sanitize per environment", and an answer that
    /// rests on an untested composition is not one.
    #[test]
    fn an_error_can_redact_its_detail_from_config() {
        let selection = selection!(
            r#"$->withConnectorError({
                message: $config.verboseErrors->match([true, $.detail], [@, "An error occurred"])
                extensions: { code: "INTERNAL_SERVER_ERROR" }
            })"#
        );
        let data = json!({ "detail": "connection refused to db-7" });

        let verbose =
            IndexMap::from_iter([("$config".to_string(), json!({ "verboseErrors": true }))]);
        let (_, errors) = selection.apply_with_vars(&data, &verbose);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["connection refused to db-7"],
        );

        let redacted =
            IndexMap::from_iter([("$config".to_string(), json!({ "verboseErrors": false }))]);
        let (_, errors) = selection.apply_with_vars(&data, &redacted);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["An error occurred"],
        );
    }

    /// Arity is one, and both ways of getting it wrong are reported. The shape
    /// function reports them as well, so neither should survive composition.
    #[test]
    fn a_call_with_the_wrong_number_of_arguments_is_reported() {
        let (value, errors) = selection!("$->withConnectorError").apply_to(&json!("value"));
        assert_eq!(value, None);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["Method ->withConnectorError requires exactly one argument, got 0"],
        );

        let (value, errors) =
            selection!(r#"$->withConnectorError("a", "b")"#).apply_to(&json!("value"));
        assert_eq!(value, None);
        assert_eq!(
            errors.iter().map(ApplyToError::message).collect::<Vec<_>>(),
            vec!["Method ->withConnectorError requires exactly one argument, got 2"],
        );
    }
}
