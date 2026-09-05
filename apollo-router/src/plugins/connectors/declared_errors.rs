//! Carrying `->withError` errors from a connector response to the client's
//! response `extensions`.
//!
//! An error a mapping author declares with `->withError` is not a GraphQL
//! execution error: [the spec][spec] says a response position at which an
//! execution error was raised must not be present in `data`, and the field a
//! declared error describes *is* present — resolving it while recording the
//! defect is the entire point of the method. So these are reported under the
//! response's `extensions`, in a `connectorErrors` array, instead.
//!
//! Getting them there takes two hops, because the one thing they need — a
//! response path the client can resolve against the data it received — is
//! computed in the middle of the pipeline:
//!
//! 1. [`aggregate_responses`](super::handle_responses::aggregate_responses)
//!    builds the ones `include_subgraph_errors` allows — an excluded subgraph's
//!    are never built at all — and puts them in the connector subgraph
//!    response's `errors`, tagged with
//!    [`DECLARED_ERROR_MARKER`]. That array is the only thing
//!    [`FetchNode::response_at_path`](crate::query_planner::fetch::FetchNode::response_at_path)
//!    rewrites paths in, turning a connector-local `_entities/0/balance` into
//!    the client paths that entity was fetched for.
//!
//! 2. [`ConnectorDeclaredErrors::take_marked`], called by the fetch service the
//!    moment that rewrite is done, lifts them back out into the request
//!    [`Context`], where the connectors plugin reads them when it builds the
//!    supergraph response.
//!
//! The hand-off is at the fetch service and not somewhere later because that
//! point is deterministic: an error stops being a GraphQL error before any
//! plugin or error-redaction pass can observe it as one.
//!
//! Leaving the `errors` array does not make them stop counting as errors. The
//! author raised them on purpose, so telemetry counts them at the connector —
//! see `count_connector_errors` — which is before the `include_subgraph_errors`
//! decision below, so withholding one from clients does not suppress its
//! metric, exactly as redacting a subgraph error does not suppress its. What they do not do is fail their connector
//! fetch: that request succeeded, and its span says so. A request that does
//! fail is reported as a failure the way it always was, and never carries
//! declared errors — a failed response mapping demotes them to problems.
//!
//! [spec]: https://spec.graphql.org/draft/#sec-Errors.Execution-Errors

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json_bytes::Value;

use crate::Context;
use crate::graphql;

/// Extension key marking an error in a connector subgraph response as declared
/// by `->withError` rather than raised by a failure.
///
/// Private, and stripped by [`ConnectorDeclaredErrors::take_marked`] on the way
/// out. It travels in the error's `extensions` rather than in a field on
/// [`graphql::Error`] because entity errors do not survive the fetch node
/// intact: `response_at_path` rebuilds one error per inverted path with
/// `Error::builder()`, which copies the extensions and drops everything not
/// named in the builder call.
pub(crate) const DECLARED_ERROR_MARKER: &str = "apollo_private.connectors.declared_error";

/// The key the declared errors are reported under in the response
/// `extensions`.
pub(crate) const CONNECTOR_ERRORS_EXTENSION_KEY: &str = "connectorErrors";

/// The declared errors collected so far for one request, in the order the
/// fetches that produced them completed.
///
/// A [`Context`] extension rather than a return value because the fetches that
/// produce these are spread across the query plan, and nothing between a fetch
/// and the supergraph response carries data that is neither `data` nor
/// `errors`.
#[derive(Clone, Default)]
pub(crate) struct ConnectorDeclaredErrors(Arc<Mutex<Vec<graphql::Error>>>);

impl ConnectorDeclaredErrors {
    /// Moves every error marked with [`DECLARED_ERROR_MARKER`] out of `errors`
    /// and into the request's collection, stripping the marker.
    ///
    /// Called with paths already rewritten, so what lands in the context is
    /// what the client will see.
    pub(crate) fn take_marked(context: &Context, errors: &mut Vec<graphql::Error>) {
        if !errors
            .iter()
            .any(|error| error.extensions.contains_key(DECLARED_ERROR_MARKER))
        {
            return;
        }

        let mut declared = Vec::new();
        errors.retain_mut(|error| {
            if error.extensions.remove(DECLARED_ERROR_MARKER).is_none() {
                return true;
            }
            declared.push(std::mem::take(error));
            false
        });

        context
            .extensions()
            .with_lock(|lock| lock.get_or_default_mut::<ConnectorDeclaredErrors>().clone())
            .0
            .lock()
            .extend(declared);
    }

    /// Removes and returns everything collected so far, as the value of the
    /// `connectorErrors` response extension.
    ///
    /// Draining rather than reading, so a deferred response reports each error
    /// once, in the chunk the fetch that declared it completed for.
    pub(crate) fn drain(context: &Context) -> Option<Value> {
        let collected = context
            .extensions()
            .with_lock(|lock| lock.get::<ConnectorDeclaredErrors>().cloned())?;
        let errors = std::mem::take(&mut *collected.0.lock());
        if errors.is_empty() {
            return None;
        }
        Some(Value::Array(
            errors
                .into_iter()
                // A `graphql::Error` serializes to exactly the `message`,
                // `path` and `extensions` shape wanted here, minus the fields
                // it already skips when empty.
                .filter_map(|error| serde_json_bytes::to_value(error).ok())
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::json_ext::Path;

    fn declared(message: &str) -> graphql::Error {
        graphql::Error::builder()
            .message(message)
            .path(Path::from("account/balance"))
            .extension_code("CONNECTORS_MAPPING_ERROR")
            .extension(DECLARED_ERROR_MARKER, Value::Bool(true))
            .build()
    }

    fn failed(message: &str) -> graphql::Error {
        graphql::Error::builder()
            .message(message)
            .extension_code("CONNECTORS_FETCH")
            .build()
    }

    /// The partition the whole design rests on: a declared error leaves the
    /// `errors` array, a real one stays. Getting this wrong in either
    /// direction is a spec violation — a resolved field with an error, or a
    /// failed fetch reported nowhere.
    #[test]
    fn only_marked_errors_are_taken() {
        let context = Context::new();
        let mut errors = vec![failed("upstream 500"), declared("balance unavailable")];

        ConnectorDeclaredErrors::take_marked(&context, &mut errors);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "upstream 500");

        let taken = ConnectorDeclaredErrors::drain(&context).expect("the declared error");
        let taken = taken.as_array().expect("an array");
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].get("message"), Some(&json!("balance unavailable")));
        // The path `response_at_path` computed is what the client sees.
        assert_eq!(taken[0].get("path"), Some(&json!(["account", "balance"])));
    }

    /// The reported `path` follows the spec's _response path_ shape even
    /// though it rides in `extensions`, where nothing obliges it to: field
    /// segments are strings and list indices are numbers, so it resolves
    /// against `data` the same way an error's `path` does. A `->withError`
    /// inside a `->map` is the case that produces indices.
    #[test]
    fn a_reported_path_is_a_response_path() {
        let context = Context::new();
        let mut errors = vec![
            graphql::Error::builder()
                .message("bad code")
                .path(Path::from("account/rows/0/code"))
                .extension_code("CONNECTORS_MAPPING_ERROR")
                .extension(DECLARED_ERROR_MARKER, Value::Bool(true))
                .build(),
        ];

        ConnectorDeclaredErrors::take_marked(&context, &mut errors);

        let taken = ConnectorDeclaredErrors::drain(&context).expect("the declared error");
        assert_eq!(
            taken.as_array().unwrap()[0].get("path"),
            // `0` and not `"0"`: the spec's path segments for list indices are
            // integers.
            Some(&json!(["account", "rows", 0, "code"])),
        );
    }

    /// The marker is an internal hand-off. A client that sees it means the
    /// error also skipped the hand-off, since the two happen together.
    #[test]
    fn the_marker_does_not_survive_the_hand_off() {
        let context = Context::new();
        let mut errors = vec![declared("balance unavailable")];

        ConnectorDeclaredErrors::take_marked(&context, &mut errors);

        let taken = ConnectorDeclaredErrors::drain(&context).expect("the declared error");
        let extensions = taken.as_array().unwrap()[0]
            .get("extensions")
            .expect("extensions");
        assert_eq!(extensions.get(DECLARED_ERROR_MARKER), None);
        // Stripping it must not take the author's own fields with it.
        assert_eq!(
            extensions.get("code"),
            Some(&json!("CONNECTORS_MAPPING_ERROR")),
        );
    }

    /// A response with no declared errors gets no `connectorErrors` key —
    /// including one whose connectors all succeeded, which is the common case
    /// and must not grow an empty array.
    #[test]
    fn nothing_declared_reports_nothing() {
        let context = Context::new();
        let mut errors = vec![failed("upstream 500")];

        ConnectorDeclaredErrors::take_marked(&context, &mut errors);

        assert_eq!(errors.len(), 1);
        assert!(ConnectorDeclaredErrors::drain(&context).is_none());
    }

    /// Draining, not reading: a deferred response streams several chunks
    /// through the same context, and an error reported in the primary chunk
    /// must not be repeated in every later one.
    #[test]
    fn a_drained_error_is_not_reported_twice() {
        let context = Context::new();
        let mut errors = vec![declared("balance unavailable")];

        ConnectorDeclaredErrors::take_marked(&context, &mut errors);

        assert!(ConnectorDeclaredErrors::drain(&context).is_some());
        assert!(ConnectorDeclaredErrors::drain(&context).is_none());
    }

    /// Errors from separate fetches accumulate rather than replacing one
    /// another: a query can hit several connectors, and each hand-off is a
    /// separate call.
    #[test]
    fn errors_from_separate_fetches_accumulate() {
        let context = Context::new();

        ConnectorDeclaredErrors::take_marked(&context, &mut vec![declared("first")]);
        ConnectorDeclaredErrors::take_marked(&context, &mut vec![declared("second")]);

        let taken = ConnectorDeclaredErrors::drain(&context).expect("both errors");
        let taken = taken.as_array().expect("an array");
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].get("message"), Some(&json!("first")));
        assert_eq!(taken[1].get("message"), Some(&json!("second")));
    }
}
