use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::debug::DebugRequest;
use apollo_federation::connectors::runtime::debug::SelectionData;
use apollo_federation::connectors::runtime::errors::Error;
use apollo_federation::connectors::runtime::errors::RuntimeError;
use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
use apollo_federation::connectors::runtime::key::ResponseKey;
use apollo_federation::connectors::runtime::mapping::Problem;
use apollo_federation::connectors::runtime::responses::HandleResponseError;
use apollo_federation::connectors::runtime::responses::MappedResponse;
use apollo_federation::connectors::runtime::responses::deserialize_response;
use apollo_federation::connectors::runtime::responses::handle_raw_response;
use axum::body::HttpBody;
use http::response::Parts;
use http_body_util::BodyExt;
use http_body_util::LengthLimitError;
use http_body_util::Limited;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tracing::Span;

use crate::Context;
use crate::graphql;
use crate::json_ext::Path;
use crate::plugins::connectors::declared_errors::DECLARED_ERROR_MARKER;
use crate::plugins::include_subgraph_errors::IncludeSubgraphErrors;
use crate::plugins::include_subgraph_errors::effective_config::EffectiveConfig;
use crate::plugins::limits::ConnectorMappingErrorLimit;
use crate::plugins::limits::ConnectorResponseSizeLimit;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_STATUS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_VERSION;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEventResponse;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;
use crate::plugins::telemetry::tracing::apollo_telemetry::emit_error_event;
use crate::services::connect::Response;
use crate::services::connector;
use crate::services::fetch::AddSubgraphNameExt;

// --- ERRORS ------------------------------------------------------------------

impl From<RuntimeError> for graphql::Error {
    fn from(error: RuntimeError) -> Self {
        let path: Path = (&error.path).into();

        let mut err = graphql::Error::builder()
            .message(&error.message)
            .extensions(error.extensions())
            .extension_code(error.code())
            .path(path)
            .build();

        // Carry over whether a span event was already emitted for this error at its source site
        // (set by `process_response`). Errors that reach this conversion without emitting — e.g.
        // coprocessor `Break` or traffic-shaping timeout/rate-limit — keep the flag `false` so the
        // catch-all in `count_operation_errors` still emits exactly one event for them.
        err.set_span_event_emitted(error.span_event_emitted());

        if let Some(subgraph_name) = &error.subgraph_name {
            err.with_subgraph_name(subgraph_name)
        } else {
            err
        }
    }
}

// --- handle_responses --------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_response<T>(
    result: Result<http::Response<T>, Error>,
    response_key: ResponseKey,
    connector: Arc<Connector>,
    context: &Context,
    debug_request: DebugRequest,
    debug_context: Option<&Arc<Mutex<ConnectorContext>>>,
    supergraph_request: Arc<http::Request<crate::graphql::Request>>,
    operation: Option<Arc<Valid<ExecutableDocument>>>,
) -> connector::request_service::Response
where
    T: HttpBody,
    T::Error: Into<tower::BoxError>,
{
    let (mut mapped_response, result) = match result {
        // This occurs when we short-circuit the request when over the limit
        Err(error) => {
            Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            (
                MappedResponse::Error {
                    error: error.to_runtime_error(&connector, &response_key),
                    key: response_key,
                    problems: Vec::new(),
                },
                Err(error),
            )
        }
        Ok(response) => {
            let (parts, body) = response.into_parts();

            let result = Ok(TransportResponse::Http(HttpResponse {
                inner: parts.clone(),
            }));

            let make_err = |message: String, code: &str| -> Box<RuntimeError> {
                let mut err = RuntimeError::new(message, &response_key);
                err.subgraph_name = Some(connector.id.subgraph_name.clone());
                err = err.with_code(code);
                err.coordinate = Some(connector.id.coordinate());
                err = err.extension(
                    "http",
                    Value::Object(Map::from_iter([(
                        "status".into(),
                        Value::Number(parts.status.as_u16().into()),
                    )])),
                );
                Box::new(err)
            };

            let make_invalid_response_err = || {
                make_err(
                    "The server returned data in an unexpected format.".to_string(),
                    "CONNECTOR_RESPONSE_INVALID",
                )
            };

            let make_limit_err = |limit: usize| {
                make_err(
                    format!("connector response body exceeded limit of {limit} bytes"),
                    "CONNECTOR_RESPONSE_SIZE_LIMIT_EXCEEDED",
                )
            };

            let response_size_limit = context
                .extensions()
                .with_lock(|e| e.get::<ConnectorResponseSizeLimit>().copied());

            let body_result: Result<_, Box<RuntimeError>> = match response_size_limit {
                Some(ConnectorResponseSizeLimit(limit)) => {
                    Limited::new(body, limit)
                        .collect()
                        .await
                        .map_err(|e| {
                            if e.downcast_ref::<LengthLimitError>().is_some() {
                                u64_counter!(
                                    "apollo.router.limits.connector_response_size.exceeded",
                                    "Number of connector responses aborted because they exceeded the configured response size limit",
                                    1,
                                    "connector.source" = connector.source_config_key()
                                );
                                tracing::Span::current()
                                    .record("apollo.connector.response.aborted", "response_size_limit");
                                make_limit_err(limit)
                            } else {
                                make_invalid_response_err()
                            }
                        })
                }
                None => body
                    .collect()
                    .await
                    .map_err(|_| make_invalid_response_err()),
            };

            let deserialized_body = body_result.and_then(|body| {
                let body = body.to_bytes();
                let raw = deserialize_response(&body, &parts.headers).map_err(|_| {
                    if let Some(debug_context) = debug_context {
                        debug_context.lock().push_invalid_response(
                            debug_request.0.clone(),
                            &parts,
                            &body,
                            &connector.error_settings,
                            debug_request.1.clone(),
                        );
                    }
                    make_invalid_response_err()
                });
                log_connectors_event(context, &body, &parts, response_key.clone(), &connector);
                raw
            });

            // If this errors, it will write to the debug context because it
            // has access to the raw bytes, so we can't write to it again
            // in any RawResponse::Error branches.
            let mut mapped = match &deserialized_body {
                Err(error) => MappedResponse::Error {
                    error: error.as_ref().clone(),
                    key: response_key,
                    problems: Vec::new(),
                },
                Ok(data) => handle_raw_response(
                    data,
                    &parts,
                    response_key,
                    &connector,
                    context,
                    supergraph_request.headers(),
                )
                .apply_operation(
                    operation
                        .as_ref()
                        .map(|arc_valid_doc| arc_valid_doc.as_ref().as_ref()),
                    &connector.schema_subtypes_map,
                ),
            };

            // Applied here, in the router, rather than inside the mapping
            // engine: the limit is operator configuration, and apollo-federation
            // has no access to it. Applied after `apply_operation` so the count
            // is of the errors that would actually have been sent.
            truncate_mapping_errors(&mut mapped, context, &connector);

            if let Some(debug) = debug_context {
                let mut debug_problems: Vec<Problem> = mapped.problems().to_vec();
                debug_problems.extend(debug_request.1);

                let selection_data = if let MappedResponse::Data { key, data, .. } = &mapped {
                    Some(SelectionData {
                        source: connector.selection.to_string(),
                        transformed: key.selection().to_string(),
                        result: Some(data.clone()),
                    })
                } else {
                    None
                };

                debug.lock().push_response(
                    debug_request.0,
                    &parts,
                    deserialized_body.ok().as_ref().unwrap_or(&Value::Null),
                    selection_data,
                    &connector.error_settings,
                    debug_problems,
                );
            }
            if matches!(mapped, MappedResponse::Data { .. }) {
                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_OK);
            } else {
                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            }

            (mapped, result)
        }
    };

    if let MappedResponse::Error { ref mut error, .. } = mapped_response {
        // Emit here so the event picks up the connector request-service span's attributes
        // (coordinate, source, etc.). Mark the error as emitted so `From<RuntimeError>` carries
        // the flag through and the centralized catch-all in `count_operation_errors` won't fire a
        // duplicate.
        emit_error_event(error.code(), &error.message, Some((*error.path).into()));
        error.set_span_event_emitted(true);
    }

    connector::request_service::Response {
        context: context.clone(),
        subgraph_name: connector.id.subgraph_name.to_string(),
        transport_result: result,
        mapped_response,
    }
}

/// The error code carried by the summary error that replaces mapping errors
/// dropped by the `limits.connector.max_mapping_errors` limit.
const TOO_MANY_MAPPING_ERRORS_CODE: &str = "CONNECTORS_TOO_MANY_ERRORS";

/// Enforce `limits.connector.max_mapping_errors` on the errors a response
/// mapping declared with `->withError`.
///
/// A `->withError` inside a `->map` records one error per element, so a mapping
/// over a large API response can contribute an error per row. When an operator
/// has set a limit, the excess is replaced by one summary error naming how many
/// were dropped, so the truncation is visible in the response rather than
/// silent. With no limit configured — the default — every declared error is
/// reported, matching how the router treats subgraph errors.
///
/// The mapping's `problems` are left alone: they are already collapsed by
/// message before reaching here, and they never leave the router, so the
/// debugger and telemetry keep the full picture regardless of the limit.
fn truncate_mapping_errors(mapped: &mut MappedResponse, context: &Context, connector: &Connector) {
    let Some(ConnectorMappingErrorLimit(limit)) = context
        .extensions()
        .with_lock(|e| e.get::<ConnectorMappingErrorLimit>().copied())
    else {
        return;
    };

    let MappedResponse::Data { errors, key, .. } = mapped else {
        return;
    };

    let total = errors.len();
    if total <= limit {
        return;
    }

    errors.truncate(limit);

    let dropped = total - limit;
    let mut overflow = RuntimeError::new(
        format!(
            "{dropped} more mapping errors were declared by this connector but not \
             reported, out of {total} total, because the configured \
             `limits.connector.max_mapping_errors` is {limit}"
        ),
        key,
    )
    .with_code(TOO_MANY_MAPPING_ERRORS_CODE);
    overflow.subgraph_name = Some(connector.id.subgraph_name.clone());
    overflow.coordinate = Some(connector.id.coordinate());
    errors.push(overflow);

    u64_counter!(
        "apollo.router.limits.connector_mapping_errors.exceeded",
        "Number of connector responses whose mapping errors were truncated because they exceeded the configured limit",
        1,
        "connector.source" = connector.source_config_key()
    );
}

/// Build the client-facing form of one error a mapping declared with
/// `->withError`, or `None` if `include_subgraph_errors` says this subgraph's
/// errors must not reach clients.
///
/// The configuration is applied *here*, where the error is built, rather than
/// on the way out. The redaction pass that governs the `errors` array runs at
/// the supergraph response and these are no longer in that array by then — but
/// the more useful reason is that an excluded error can simply not be built.
/// Redacting one after the fact would leave `extensions.connectorErrors`
/// carrying a row that says "Subgraph errors redacted" and nothing else, which
/// tells a client only that something was withheld.
///
/// Everything short of exclusion is delegated to the same
/// [`IncludeSubgraphErrors::process_error`] the `errors` array goes through, so
/// `redact_message` and the extension allow/deny lists mean exactly what they
/// mean everywhere else. The marker is added afterwards: an allow list would
/// otherwise filter it out, and an error that loses its marker never leaves
/// the `errors` array.
///
/// Telemetry is unaffected by the decision: the declared errors were counted at
/// the connector, before this runs (see `count_connector_errors`), so an error
/// withheld from clients still shows up in error metrics.
///
/// With no configuration published — which in a running router means the
/// mandatory `include_subgraph_errors` plugin did not see this request — the
/// error is dropped. Failing closed, because the alternative is a config that
/// exists to keep subgraph text away from clients being bypassed by a code
/// path that could not read it.
fn declared_error_for_client(error: RuntimeError, context: &Context) -> Option<graphql::Error> {
    let config = context
        .extensions()
        .with_lock(|lock| lock.get::<Arc<EffectiveConfig>>().cloned())?;

    let subgraph_name = error.subgraph_name.clone()?;
    if !config.for_subgraph(&subgraph_name).include_errors {
        return None;
    }

    let mut error: graphql::Error = error.into();
    IncludeSubgraphErrors::process_error(&config, &mut error);
    error
        .extensions
        .insert(DECLARED_ERROR_MARKER, Value::Bool(true));
    Some(error)
}

pub(crate) fn aggregate_responses(
    responses: Vec<MappedResponse>,
    context: Context,
) -> Result<Response, HandleResponseError> {
    let mut data = serde_json_bytes::Map::new();
    let mut errors = Vec::new();
    let mut declared = Vec::new();
    let count = responses.len();

    for mut mapped in responses {
        declared.extend(mapped.take_declared_errors());
        mapped.add_to_data(&mut data, &mut errors, count)?;
    }

    let data = if data.is_empty() {
        Value::Null
    } else {
        Value::Object(data)
    };

    // The span reports the *request's* outcome, so it is failed when the
    // request failed and not otherwise. Declared errors are excluded because
    // nothing failed: the request succeeded and the mapping author chose to say
    // something about its contents. They are still counted as errors, at the
    // connector layer — see `count_connector_errors`.
    Span::current().record(
        OTEL_STATUS_CODE,
        if errors.is_empty() {
            OTEL_STATUS_CODE_OK
        } else {
            OTEL_STATUS_CODE_ERROR
        },
    );

    // Declared errors ride in the `errors` array only as far as the fetch
    // service, which lifts them back out. They travel that way — rather than
    // straight to the context — because it is the only way they get a response
    // path a client can use: `FetchNode::response_at_path` is what rewrites
    // `_entities/0/balance` into the client paths the entity landed at, and it
    // rewrites nothing else. See `DECLARED_ERROR_MARKER`.
    let mut errors: Vec<graphql::Error> = errors.into_iter().map(Into::into).collect();
    errors.extend(
        declared
            .into_iter()
            .filter_map(|error| declared_error_for_client(error, &context)),
    );

    Ok(Response {
        response: http::Response::builder()
            .body(
                graphql::Response::builder()
                    .data(data)
                    .errors(errors)
                    .build(),
            )
            .unwrap(),
    })
}

fn log_connectors_event(
    context: &Context,
    body: &[u8],
    parts: &Parts,
    response_key: ResponseKey,
    connector: &Connector,
) {
    let log_response_level = context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorEventResponse>().cloned())
        .and_then(|event| {
            // TODO: evaluate if this is still needed now that we're cloning the body anyway
            // Create a temporary response here so we can evaluate the condition. This response
            // is missing any information about the mapped response, because we don't have that
            // yet. This means that we cannot correctly evaluate any condition that relies on
            // the mapped response data or mapping problems. But we can't wait until we do have
            // that information, because this is the only place we have the body bytes (without
            // making an expensive clone of the body). So we either need to not expose any
            // selector which can be used as a condition that requires mapping information, or
            // we must document that such selectors cannot be used as conditions on standard
            // connectors events.

            let response = connector::request_service::Response {
                context: context.clone(),
                subgraph_name: connector.id.subgraph_name.to_string(),
                transport_result: Ok(TransportResponse::Http(HttpResponse {
                    inner: parts.clone(),
                })),
                mapped_response: MappedResponse::Data {
                    data: Value::Null,
                    key: response_key,
                    problems: vec![],
                    errors: vec![],
                },
            };
            if event.condition.evaluate_response(&response) {
                Some(event.level)
            } else {
                None
            }
        });

    if let Some(level) = log_response_level {
        let mut attrs = Vec::with_capacity(4);

        let header_string = crate::services::header_masking::masked_headers_for_log(
            context,
            crate::services::header_masking::Direction::Response,
            Some(connector.id.subgraph_name.as_str()),
            &parts.headers,
        );

        attrs.push(KeyValue::new(
            HTTP_RESPONSE_HEADERS,
            opentelemetry::Value::String(header_string.into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_RESPONSE_STATUS,
            opentelemetry::Value::String(format!("{}", parts.status).into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_RESPONSE_VERSION,
            opentelemetry::Value::String(format!("{:?}", parts.version).into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_RESPONSE_BODY,
            opentelemetry::Value::String(String::from_utf8_lossy(body).into_owned().into()),
        ));

        log_event(
            level,
            "connector.response",
            attrs,
            &format!("Response from connector {label:?}", label = connector.label),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::Schema;
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::name;
    use apollo_compiler::response::JsonValue;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::ConnectorErrorsSettings;
    use apollo_federation::connectors::EntityResolver;
    use apollo_federation::connectors::HTTPMethod;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::Label;
    use apollo_federation::connectors::Namespace;
    use apollo_federation::connectors::runtime::errors::RuntimeError;
    use apollo_federation::connectors::runtime::inputs::RequestInputs;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use insta::assert_debug_snapshot;
    use itertools::Itertools;
    use serde_json_bytes::json;

    use crate::Context;
    use crate::graphql;
    use crate::plugins::connectors::declared_errors::ConnectorDeclaredErrors;
    use crate::plugins::connectors::declared_errors::DECLARED_ERROR_MARKER;
    use crate::plugins::connectors::handle_responses::MappedResponse;
    use crate::plugins::connectors::handle_responses::TOO_MANY_MAPPING_ERRORS_CODE;
    use crate::plugins::connectors::handle_responses::aggregate_responses;
    use crate::plugins::connectors::handle_responses::handle_raw_response;
    use crate::plugins::connectors::handle_responses::process_response;
    use crate::plugins::connectors::handle_responses::truncate_mapping_errors;
    use crate::plugins::include_subgraph_errors::config::Config as IncludeSubgraphErrorsConfig;
    use crate::plugins::include_subgraph_errors::effective_config::EffectiveConfig;
    use crate::plugins::limits::ConnectorMappingErrorLimit;
    use crate::services::router;
    use crate::services::router::body::RouterBody;

    /// `->withError` has to be reachable from a connector schema, and the
    /// `is_public()` gate that decides so cannot be observed from
    /// apollo-federation's own tests: `ArrowMethod::lookup` resolves every
    /// method under `cfg!(test)`, public or not. Here apollo-federation is a
    /// dependency compiled without `--test`, so the gate is live and demoting
    /// `->withError` back to the `future` namespace fails this test.
    #[test]
    fn with_error_is_available_to_connector_schemas() {
        let selection =
            JSONSelection::parse("id status: code->withError('unrecognized type code')").unwrap();

        let (value, errors) = selection.apply_to(&json!({ "id": "1", "code": 7 }));

        // The value flows through untouched: ->withError records, never rewrites.
        assert_eq!(value, Some(json!({ "id": "1", "status": 7 })));
        assert_eq!(
            errors.iter().map(|error| error.message()).collect_vec(),
            vec!["unrecognized type code"],
        );
    }

    /// The customer-facing payload of a declared `->withError`: the author's
    /// message, code and structured fields, intact, while the field it
    /// accompanies still resolves.
    ///
    /// It leaves `aggregate_responses` in the subgraph response's `errors`,
    /// marked — that is the ride to the fetch service, which is the only place
    /// paths get rewritten — and this asserts both halves: the marker is on it
    /// there, and `take_marked` lifts it back out, leaving nothing behind in
    /// `errors` for a client to see as an execution error.
    /// A `Context` carrying the effective `include_subgraph_errors`
    /// configuration the mandatory plugin publishes for every request, built
    /// from the same YAML shape an operator writes under `all:`.
    fn included_subgraph_errors(config: serde_json::Value) -> Context {
        let config: IncludeSubgraphErrorsConfig =
            serde_json::from_value(serde_json::json!({ "all": config })).expect("valid config");
        let effective: EffectiveConfig = config.try_into().expect("valid effective config");

        let context = Context::new();
        context
            .extensions()
            .with_lock(|lock| lock.insert::<Arc<EffectiveConfig>>(Arc::new(effective)));
        context
    }

    /// A mapped response whose mapping resolved `balance` with a default and
    /// declared a structured error about it, from the subgraph
    /// `subgraph_name`.
    fn mapped_with_structured_declared_error() -> MappedResponse {
        let selection = JSONSelection::parse(
            r#"balance: amount ?? $("<missing>")->withError({
                message: "Field 'amount' was not found"
                extensions: { code: "INTERNAL_SERVER_ERROR", number: 210099 }
            })"#,
        )
        .unwrap();
        let response_key = ResponseKey::RootField {
            name: "account".to_string(),
            inputs: Default::default(),
            selection: Arc::new(selection),
        };

        let connector = Connector {
            spec: ConnectSpec::V0_5,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(account),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: ConnectorErrorsSettings::default(),
            output_type: None,
            label: "test label".into(),
        };

        let parts = http::Response::builder()
            .status(200)
            .body(())
            .unwrap()
            .into_parts()
            .0;

        handle_raw_response(
            &json!({ "id": "acct-1" }),
            &parts,
            response_key,
            &connector,
            &Context::new(),
            &http::HeaderMap::new(),
        )
    }

    #[test]
    fn a_declared_error_reaches_the_response_extensions() {
        let mapped = mapped_with_structured_declared_error();

        let aggregated = aggregate_responses(
            vec![mapped],
            included_subgraph_errors(serde_json::json!(true)),
        )
        .expect("aggregation succeeds");
        let mut response = aggregated.response.into_body();

        // The field resolved with its default rather than being nulled out.
        assert_eq!(
            response.data,
            Some(json!({ "account": { "balance": "<missing>" } })),
        );

        // In transit it looks like an error, but only to the fetch service:
        // the marker is what tells that apart from a real one.
        assert_eq!(response.errors.len(), 1);
        assert_eq!(
            response.errors[0].extensions.get(DECLARED_ERROR_MARKER),
            Some(&json!(true)),
        );

        let context = Context::new();
        ConnectorDeclaredErrors::take_marked(&context, &mut response.errors);

        // Nothing is left for the client to see as an execution error...
        assert!(response.errors.is_empty());

        // ...and the client is still told why, under `extensions`.
        let declared = ConnectorDeclaredErrors::drain(&context).expect("an error was collected");
        let declared = declared.as_array().expect("an array");
        assert_eq!(declared.len(), 1);
        let error = &declared[0];

        assert_eq!(
            error.get("message"),
            Some(&json!("Field 'amount' was not found"))
        );
        let extensions = error.get("extensions").expect("extensions");
        assert_eq!(
            extensions.get("code"),
            Some(&json!("INTERNAL_SERVER_ERROR")),
        );
        assert_eq!(extensions.get("number"), Some(&json!(210099)));

        // The marker was private to the hand-off and does not reach the client.
        assert_eq!(extensions.get(DECLARED_ERROR_MARKER), None);

        // The path resolves against the data above: `account` → `balance`,
        // the field the mapping writes, not `amount`, the field it reads.
        assert_eq!(error.get("path"), Some(&json!(["account", "balance"])));
    }

    /// `include_subgraph_errors` governs these too. An operator who has said a
    /// subgraph's errors must not reach clients has said it about the text a
    /// mapping author wrote as well: it is the same subgraph's words, and it
    /// can interpolate the same API data.
    ///
    /// Omitted rather than redacted. A redacted row in `connectorErrors` would
    /// say only that something was withheld, and this is the default
    /// configuration, so it would be what most routers emit.
    #[test]
    fn a_declared_error_is_omitted_when_the_subgraph_is_excluded() {
        let aggregated = aggregate_responses(
            vec![mapped_with_structured_declared_error()],
            // `all: false`, which is also the default when the operator
            // configures nothing.
            included_subgraph_errors(serde_json::json!(false)),
        )
        .expect("aggregation succeeds");
        let response = aggregated.response.into_body();

        // The field still resolved with its default: excluding the error does
        // not change the data.
        assert_eq!(
            response.data,
            Some(json!({ "account": { "balance": "<missing>" } })),
        );
        // And nothing was built to carry onwards — not even a redacted row.
        assert!(response.errors.is_empty());
    }

    /// Failing closed: with no effective configuration published — which in a
    /// running router means the mandatory `include_subgraph_errors` plugin did
    /// not see the request — nothing is reported. A config whose job is to
    /// keep subgraph text away from clients must not be bypassed by a code
    /// path that could not read it.
    #[test]
    fn a_declared_error_is_omitted_when_no_configuration_was_published() {
        let aggregated = aggregate_responses(
            vec![mapped_with_structured_declared_error()],
            Context::new(),
        )
        .expect("aggregation succeeds");
        let response = aggregated.response.into_body();

        assert!(response.errors.is_empty());
    }

    /// Everything short of exclusion is the existing redaction, applied at
    /// build time: `redact_message` replaces the author's message and the
    /// extension lists filter the author's fields, exactly as they do for the
    /// `errors` array. Asserted through the real `Config` so the
    /// interpretation of the YAML is the shared one.
    #[test]
    fn a_declared_error_obeys_message_and_extension_redaction() {
        let aggregated = aggregate_responses(
            vec![mapped_with_structured_declared_error()],
            included_subgraph_errors(serde_json::json!({
                "deny_extensions_keys": ["number"],
                "redact_message": true,
            })),
        )
        .expect("aggregation succeeds");
        let response = aggregated.response.into_body();

        assert_eq!(response.errors.len(), 1);
        let error = &response.errors[0];

        // The author's message is gone...
        assert_eq!(error.message, "Subgraph errors redacted");
        // ...as is the denied extension...
        assert_eq!(error.extensions.get("number"), None);
        // ...while what the deny list did not name survives.
        assert_eq!(
            error.extensions.get("code"),
            Some(&json!("INTERNAL_SERVER_ERROR")),
        );

        // The marker is added after redaction, so an allow list cannot strip
        // it and strand a declared error in the `errors` array.
        assert_eq!(
            error.extensions.get(DECLARED_ERROR_MARKER),
            Some(&json!(true)),
        );
    }

    /// Build a `MappedResponse::Data` carrying `count` declared errors, as a
    /// mapping with a `->withError` inside a `->map` would produce.
    fn mapped_with_declared_errors(count: usize) -> (MappedResponse, Connector) {
        let selection =
            JSONSelection::parse(r#"$.rows->map(@.code->withError("bad code:", @))"#).unwrap();
        let response_key = ResponseKey::RootField {
            name: "rows".to_string(),
            inputs: Default::default(),
            selection: Arc::new(selection),
        };

        let connector = Connector {
            spec: ConnectSpec::V0_5,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(rows),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: ConnectorErrorsSettings::default(),
            output_type: None,
            label: "test label".into(),
        };

        let parts = http::Response::builder()
            .status(200)
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let rows = (0..count)
            .map(|index| json!({ "code": index }))
            .collect::<Vec<_>>();

        let mapped = handle_raw_response(
            &json!({ "rows": rows }),
            &parts,
            response_key,
            &connector,
            &Context::new(),
            &http::HeaderMap::new(),
        );

        (mapped, connector)
    }

    /// With no limit configured — the default — every declared error is
    /// reported, the same way the router passes through every subgraph error.
    #[test]
    fn mapping_errors_are_not_truncated_without_a_configured_limit() {
        let (mut mapped, connector) = mapped_with_declared_errors(250);

        truncate_mapping_errors(&mut mapped, &Context::new(), &connector);

        let MappedResponse::Data { errors, .. } = &mapped else {
            panic!("expected data, got: {mapped:?}");
        };
        assert_eq!(errors.len(), 250);
    }

    /// With a limit configured, the excess is replaced by one summary error, so
    /// a client can tell the list was shortened rather than silently receiving
    /// a partial picture.
    #[test]
    fn mapping_errors_are_truncated_to_the_configured_limit() {
        let (mut mapped, connector) = mapped_with_declared_errors(250);

        let context = Context::new();
        context
            .extensions()
            .with_lock(|e| e.insert(ConnectorMappingErrorLimit(100)));

        truncate_mapping_errors(&mut mapped, &context, &connector);

        let MappedResponse::Data { errors, .. } = &mapped else {
            panic!("expected data, got: {mapped:?}");
        };
        // 100 kept, plus the summary.
        assert_eq!(errors.len(), 101);
        let overflow = errors.last().unwrap();
        assert_eq!(overflow.code(), TOO_MANY_MAPPING_ERRORS_CODE);
        assert!(
            overflow.message.starts_with("150 more mapping errors"),
            "unexpected overflow message: {}",
            overflow.message,
        );
    }

    /// A response at or under the limit is untouched — no summary error is
    /// appended when nothing was dropped.
    #[test]
    fn mapping_errors_at_the_limit_are_left_alone() {
        let (mut mapped, connector) = mapped_with_declared_errors(100);

        let context = Context::new();
        context
            .extensions()
            .with_lock(|e| e.insert(ConnectorMappingErrorLimit(100)));

        truncate_mapping_errors(&mut mapped, &context, &connector);

        let MappedResponse::Data { errors, .. } = &mapped else {
            panic!("expected data, got: {mapped:?}");
        };
        assert_eq!(errors.len(), 100);
        assert!(
            errors
                .iter()
                .all(|e| e.code() != TOO_MANY_MAPPING_ERRORS_CODE)
        );
    }

    #[test]
    fn from_runtime_error_transfers_span_event_emitted_flag() {
        let response_key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        // An error that never had a span event emitted at its source (e.g. coprocessor `Break`
        // or traffic-shaping timeout/rate-limit) must keep the flag `false` so the catch-all in
        // `count_operation_errors` still emits exactly one event for it.
        let not_emitted = RuntimeError::new("boom", &response_key);
        let converted: graphql::Error = not_emitted.into();
        assert!(
            !converted.span_event_emitted(),
            "errors that never emitted a span event must not be marked as emitted"
        );

        // An error whose source site already emitted (process_response sets this) must carry the
        // flag through so the catch-all doesn't fire a duplicate.
        let mut emitted = RuntimeError::new("boom", &response_key);
        emitted.set_span_event_emitted(true);
        let converted: graphql::Error = emitted.into();
        assert!(
            converted.span_event_emitted(),
            "errors whose source already emitted must stay marked as emitted"
        );
    }

    #[tokio::test]
    async fn test_handle_responses_root_fields() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        });

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":"world"}"#))
            .unwrap();
        let response_key1 = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response2 = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":"world"}"#))
            .unwrap();
        let response_key2 = ResponseKey::RootField {
            name: "hello2".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let res = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response1),
                    response_key1,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request.clone(),
                    Default::default(),
                )
                .await
                .mapped_response,
                process_response(
                    Ok(response2),
                    response_key2,
                    connector,
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        assert_debug_snapshot!(res.response, @r#"
        Response {
            status: 200,
            version: HTTP/1.1,
            headers: {},
            body: Response {
                label: None,
                data: Some(
                    Object({
                        "hello": String(
                            "world",
                        ),
                        "hello2": String(
                            "world",
                        ),
                    }),
                ),
                path: None,
                errors: [],
                extensions: {},
                has_next: None,
                subscribed: None,
                created_at: None,
                incremental: [],
            },
        }
        "#);
    }

    #[tokio::test]
    async fn test_handle_responses_entities() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(user),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data { id }").unwrap(),
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        });

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":{"id": "1"}}"#))
            .unwrap();
        let response_key1 = ResponseKey::Entity {
            index: 0,
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response2 = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":{"id": "2"}}"#))
            .unwrap();
        let response_key2 = ResponseKey::Entity {
            index: 1,
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let res = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response1),
                    response_key1,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request.clone(),
                    Default::default(),
                )
                .await
                .mapped_response,
                process_response(
                    Ok(response2),
                    response_key2,
                    connector,
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        assert_debug_snapshot!(res.response, @r#"
        Response {
            status: 200,
            version: HTTP/1.1,
            headers: {},
            body: Response {
                label: None,
                data: Some(
                    Object({
                        "_entities": Array([
                            Object({
                                "id": String(
                                    "1",
                                ),
                            }),
                            Object({
                                "id": String(
                                    "2",
                                ),
                            }),
                        ]),
                    }),
                ),
                path: None,
                errors: [],
                extensions: {},
                has_next: None,
                subscribed: None,
                created_at: None,
                incremental: [],
            },
        }
        "#);
    }

    #[tokio::test]
    async fn test_handle_responses_batch() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_2,
            id: ConnectId::new_on_object("subgraph_name".into(), None, name!(User), None, 0),
            schema_subtypes_map: Default::default(),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Post,
                body: Some(JSONSelection::parse("ids: $batch.id").unwrap()),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data { id name }").unwrap(),
            entity_resolver: Some(EntityResolver::TypeBatch),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        });

        let keys = connector
            .resolvable_key(
                &Schema::parse_and_validate("type Query { _: ID } type User { id: ID! }", "")
                    .unwrap(),
            )
            .unwrap()
            .unwrap();

        let response1: http::Response<RouterBody> = http::Response::builder()
            // different order from the request inputs
            .body(router::body::from_bytes(
                r#"{"data":[{"id": "2","name":"B"},{"id": "1","name":"A"}]}"#,
            ))
            .unwrap();

        let mut inputs: RequestInputs = RequestInputs::default();
        let representations = serde_json_bytes::json!([{"__typename": "User", "id": "1"}, {"__typename": "User", "id": "2"}]);
        inputs.batch = representations
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect_vec();

        let response_key1 = ResponseKey::BatchEntity {
            selection: Arc::new(JSONSelection::parse("$.data { id name }").unwrap()),
            keys,
            inputs,
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let res = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response1),
                    response_key1,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        assert_debug_snapshot!(res.response, @r#"
        Response {
            status: 200,
            version: HTTP/1.1,
            headers: {},
            body: Response {
                label: None,
                data: Some(
                    Object({
                        "_entities": Array([
                            Object({
                                "id": String(
                                    "1",
                                ),
                                "name": String(
                                    "A",
                                ),
                            }),
                            Object({
                                "id": String(
                                    "2",
                                ),
                                "name": String(
                                    "B",
                                ),
                            }),
                        ]),
                    }),
                ),
                path: None,
                errors: [],
                extensions: {},
                has_next: None,
                subscribed: None,
                created_at: None,
                incremental: [],
            },
        }
        "#);
    }

    #[tokio::test]
    async fn test_handle_responses_entity_field() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(User),
                name!(field),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: Some(EntityResolver::Implicit),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        });

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":"value1"}"#))
            .unwrap();
        let response_key1 = ResponseKey::EntityField {
            index: 0,
            inputs: Default::default(),
            field_name: "field".to_string(),
            typename: Some(name!("User")),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response2 = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":"value2"}"#))
            .unwrap();
        let response_key2 = ResponseKey::EntityField {
            index: 1,
            inputs: Default::default(),
            field_name: "field".to_string(),
            typename: Some(name!("User")),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let res = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response1),
                    response_key1,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request.clone(),
                    Default::default(),
                )
                .await
                .mapped_response,
                process_response(
                    Ok(response2),
                    response_key2,
                    connector,
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        assert_debug_snapshot!(res.response, @r#"
        Response {
            status: 200,
            version: HTTP/1.1,
            headers: {},
            body: Response {
                label: None,
                data: Some(
                    Object({
                        "_entities": Array([
                            Object({
                                "__typename": String(
                                    "User",
                                ),
                                "field": String(
                                    "value1",
                                ),
                            }),
                            Object({
                                "__typename": String(
                                    "User",
                                ),
                                "field": String(
                                    "value2",
                                ),
                            }),
                        ]),
                    }),
                ),
                path: None,
                errors: [],
                extensions: {},
                has_next: None,
                subscribed: None,
                created_at: None,
                incremental: [],
            },
        }
        "#);
    }

    #[tokio::test]
    async fn test_handle_responses_errors() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(user),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        });

        let response_plaintext: http::Response<RouterBody> = http::Response::builder()
            .body(router::body::from_bytes(r#"plain text"#))
            .unwrap();
        let response_key_plaintext = ResponseKey::Entity {
            index: 0,
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .status(404)
            .body(router::body::from_bytes(r#"{"error":"not found"}"#))
            .unwrap();
        let response_key1 = ResponseKey::Entity {
            index: 1,
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response2 = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":{"id":"2"}}"#))
            .unwrap();
        let response_key2 = ResponseKey::Entity {
            index: 2,
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response3: http::Response<RouterBody> = http::Response::builder()
            .status(500)
            .body(router::body::from_bytes(r#"{"error":"whoops"}"#))
            .unwrap();
        let response_key3 = ResponseKey::Entity {
            index: 3,
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let mut res = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response_plaintext),
                    response_key_plaintext,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request.clone(),
                    Default::default(),
                )
                .await
                .mapped_response,
                process_response(
                    Ok(response1),
                    response_key1,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request.clone(),
                    Default::default(),
                )
                .await
                .mapped_response,
                process_response(
                    Ok(response2),
                    response_key2,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request.clone(),
                    Default::default(),
                )
                .await
                .mapped_response,
                process_response(
                    Ok(response3),
                    response_key3,
                    connector,
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        // Overwrite error IDs to avoid random Uuid mismatch.
        // Since assert_debug_snapshot does not support redactions (which would be useful for error IDs),
        // we have to do it manually.
        let body = res.response.body_mut();
        body.errors = body.errors.iter_mut().map(|e| e.with_null_id()).collect();

        assert_debug_snapshot!(res.response, @r#"
        Response {
            status: 200,
            version: HTTP/1.1,
            headers: {},
            body: Response {
                label: None,
                data: Some(
                    Object({
                        "_entities": Array([
                            Null,
                            Null,
                            Object({
                                "id": String(
                                    "2",
                                ),
                            }),
                            Null,
                        ]),
                    }),
                ),
                path: None,
                errors: [
                    Error {
                        message: "The server returned data in an unexpected format.",
                        locations: [],
                        path: Some(
                            Path(
                                [
                                    Key(
                                        "_entities",
                                        None,
                                    ),
                                    Index(
                                        0,
                                    ),
                                ],
                            ),
                        ),
                        extensions: {
                            "code": String(
                                "CONNECTOR_RESPONSE_INVALID",
                            ),
                            "service": String(
                                "subgraph_name",
                            ),
                            "connector": Object({
                                "coordinate": String(
                                    "subgraph_name:Query.user[0]",
                                ),
                            }),
                            "http": Object({
                                "status": Number(200),
                            }),
                            "apollo.private.subgraph.name": String(
                                "subgraph_name",
                            ),
                        },
                        apollo_id: 00000000-0000-0000-0000-000000000000,
                        span_event_emitted: true,
                    },
                    Error {
                        message: "Request failed",
                        locations: [],
                        path: Some(
                            Path(
                                [
                                    Key(
                                        "_entities",
                                        None,
                                    ),
                                    Index(
                                        1,
                                    ),
                                ],
                            ),
                        ),
                        extensions: {
                            "code": String(
                                "CONNECTOR_FETCH",
                            ),
                            "service": String(
                                "subgraph_name",
                            ),
                            "connector": Object({
                                "coordinate": String(
                                    "subgraph_name:Query.user[0]",
                                ),
                            }),
                            "http": Object({
                                "status": Number(404),
                            }),
                            "apollo.private.subgraph.name": String(
                                "subgraph_name",
                            ),
                        },
                        apollo_id: 00000000-0000-0000-0000-000000000000,
                        span_event_emitted: true,
                    },
                    Error {
                        message: "Request failed",
                        locations: [],
                        path: Some(
                            Path(
                                [
                                    Key(
                                        "_entities",
                                        None,
                                    ),
                                    Index(
                                        3,
                                    ),
                                ],
                            ),
                        ),
                        extensions: {
                            "code": String(
                                "CONNECTOR_FETCH",
                            ),
                            "service": String(
                                "subgraph_name",
                            ),
                            "connector": Object({
                                "coordinate": String(
                                    "subgraph_name:Query.user[0]",
                                ),
                            }),
                            "http": Object({
                                "status": Number(500),
                            }),
                            "apollo.private.subgraph.name": String(
                                "subgraph_name",
                            ),
                        },
                        apollo_id: 00000000-0000-0000-0000-000000000000,
                        span_event_emitted: true,
                    },
                ],
                extensions: {},
                has_next: None,
                subscribed: None,
                created_at: None,
                incremental: [],
            },
        }
        "#);
    }

    #[tokio::test]
    async fn test_handle_responses_status() {
        let selection = JSONSelection::parse("$status").unwrap();
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: selection.clone(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: IndexMap::from_iter([(Namespace::Status, Default::default())]),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        });

        let response1: http::Response<RouterBody> = http::Response::builder()
            .status(201)
            .body(router::body::from_bytes(r#"{}"#))
            .unwrap();
        let response_key1 = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$status").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let res = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response1),
                    response_key1,
                    connector,
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        assert_debug_snapshot!(res.response, @r#"
        Response {
            status: 200,
            version: HTTP/1.1,
            headers: {},
            body: Response {
                label: None,
                data: Some(
                    Object({
                        "hello": Number(201),
                    }),
                ),
                path: None,
                errors: [],
                extensions: {},
                has_next: None,
                subscribed: None,
                created_at: None,
                incremental: [],
            },
        }
        "#);
    }

    #[tokio::test]
    async fn test_handle_response_with_is_success() {
        let is_success = JSONSelection::parse("$status ->eq(400)").unwrap();
        let selection = JSONSelection::parse("$status").unwrap();
        let error_settings: ConnectorErrorsSettings = ConnectorErrorsSettings {
            message: Default::default(),
            source_extensions: Default::default(),
            connect_extensions: Default::default(),
            connect_is_success: Some(is_success.clone()),
        };
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: selection.clone(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: IndexMap::from_iter([(Namespace::Status, Default::default())]),
            error_settings,
            output_type: None,
            label: Label::from("test label"),
        });

        // First request should be marked as error as status is NOT 400
        let response_fail: http::Response<RouterBody> = http::Response::builder()
            .status(201)
            .body(router::body::from_bytes(r#"{}"#))
            .unwrap();
        let response_fail_key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$status").unwrap()),
        };

        // Second response should be marked as a success as the status is 400!
        let response_succeed: http::Response<RouterBody> = http::Response::builder()
            .status(400)
            .body(router::body::from_bytes(r#"{}"#))
            .unwrap();
        let response_succeed_key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$status").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        // Make failing request
        let res_expect_fail = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response_fail),
                    response_fail_key,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request.clone(),
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap()
        .response;
        assert_eq!(res_expect_fail.body().data, Some(JsonValue::Null));
        assert_eq!(res_expect_fail.body().errors.len(), 1);

        // Make succeeding request
        let res_expect_success = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response_succeed),
                    response_succeed_key,
                    connector.clone(),
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request.clone(),
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap()
        .response;
        assert!(res_expect_success.body().errors.is_empty());
        assert_eq!(
            &res_expect_success.body().data,
            &Some(json!({"hello": json!(400)}))
        );
    }

    fn make_connector() -> Arc<Connector> {
        Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        })
    }

    fn make_supergraph_request() -> Arc<http::Request<graphql::Request>> {
        Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn process_response_under_size_limit() {
        use crate::plugins::limits::ConnectorResponseSizeLimit;

        let ctx = Context::new();
        ctx.extensions()
            .with_lock(|e| e.insert(ConnectorResponseSizeLimit(1000)));

        let key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };
        let response = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":"world"}"#))
            .unwrap();

        let result = process_response(
            Ok(response),
            key,
            make_connector(),
            &ctx,
            (None, Default::default()),
            None,
            make_supergraph_request(),
            Default::default(),
        )
        .await;

        let graphql_response =
            super::aggregate_responses(vec![result.mapped_response], Context::new())
                .unwrap()
                .response;
        assert!(
            graphql_response.body().errors.is_empty(),
            "expected no errors when response is under the limit"
        );
    }

    #[tokio::test]
    async fn process_response_exceeds_size_limit() {
        use crate::plugins::limits::ConnectorResponseSizeLimit;

        let ctx = Context::new();
        // Limit of 5 bytes — well under the response body size
        ctx.extensions()
            .with_lock(|e| e.insert(ConnectorResponseSizeLimit(5)));

        let key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };
        let response = http::Response::builder()
            .body(router::body::from_bytes(r#"{"data":"world"}"#))
            .unwrap();

        let result = process_response(
            Ok(response),
            key,
            make_connector(),
            &ctx,
            (None, Default::default()),
            None,
            make_supergraph_request(),
            Default::default(),
        )
        .await;

        let graphql_response =
            super::aggregate_responses(vec![result.mapped_response], Context::new())
                .unwrap()
                .response;
        let errors = &graphql_response.body().errors;
        assert!(!errors.is_empty(), "expected an error for exceeded limit");
        assert!(
            errors[0].message.contains("exceeded limit of 5 bytes"),
            "unexpected error message: {}",
            errors[0].message
        );
    }

    // Reproduction for CNN-1095: when `isSuccess` returns false and the user has
    // configured `errors.message` and `errors.extensions`, the resulting GraphQL
    // error should use the mapped values (sourced from the response body) and
    // still expose the default `http.status` alongside them.
    //
    // Per the public docs at
    // https://www.apollographql.com/docs/graphos/connectors/responses/error-handling,
    // the `errors.message` mapping expression yields the error message and
    // `errors.extensions` is merged into `extensions` (overriding defaults like
    // `code` when keys collide, preserving defaults like `http.status` when they
    // don't).
    #[tokio::test]
    async fn errors_as_data_maps_message_and_extensions_when_is_success_false() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_2,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: ConnectorErrorsSettings {
                message: Some(JSONSelection::parse("error.message").unwrap()),
                connect_extensions: Some(
                    JSONSelection::parse("code: error.code\nhint: error.hint").unwrap(),
                ),
                source_extensions: None,
                connect_is_success: Some(JSONSelection::parse("$status->eq(200)").unwrap()),
            },
            output_type: None,
            label: "test label".into(),
        });

        let response: http::Response<RouterBody> = http::Response::builder()
            .status(500)
            .body(router::body::from_bytes(
                r#"{"error":{"message":"no good","code":"BAD_THING","hint":"try again"}}"#,
            ))
            .unwrap();
        let response_key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let result = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response),
                    response_key,
                    connector,
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        let errors = &result.response.body().errors;
        assert_eq!(
            errors.len(),
            1,
            "expected exactly one error, got: {errors:?}"
        );
        let error = &errors[0];

        assert_eq!(
            error.message, "no good",
            "errors.message should be mapped from the response body"
        );

        let code = error
            .extensions
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_eq!(
            code, "BAD_THING",
            "errors.extensions.code should override default CONNECTOR_FETCH"
        );

        let hint = error
            .extensions
            .get("hint")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_eq!(
            hint, "try again",
            "errors.extensions.hint should be mapped from the response body"
        );

        let http_status = error
            .extensions
            .get("http")
            .and_then(|v| v.as_object())
            .and_then(|m| m.get("status"))
            .and_then(|v| v.as_i64());
        assert_eq!(
            http_status,
            Some(500),
            "default extensions.http.status should be preserved alongside the mapped extensions"
        );
    }

    // Reproduction for CNN-1095: when `errors.extensions` writes a nested key
    // that collides with a default extension (e.g. `http`), the public docs at
    // https://www.apollographql.com/docs/graphos/connectors/responses/error-handling
    // say the user-supplied values should be merged into the default object
    // (so `extensions.http.status` is preserved alongside `extensions.http.myField`).
    //
    // The current implementation in `runtime/responses.rs::map_error` calls
    // `error.extension("http", user_value)` after the default `http: { status }`
    // is set, which replaces the entire `http` object — so `status` is lost.
    #[tokio::test]
    async fn errors_as_data_deep_merges_nested_extensions_with_defaults() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_2,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: ConnectorErrorsSettings {
                message: None,
                connect_extensions: Some(
                    JSONSelection::parse("http: { myField: $(\"literal Value\") }").unwrap(),
                ),
                source_extensions: None,
                connect_is_success: Some(JSONSelection::parse("$status->eq(200)").unwrap()),
            },
            output_type: None,
            label: "test label".into(),
        });

        let response: http::Response<RouterBody> = http::Response::builder()
            .status(500)
            .body(router::body::from_bytes(r#"{}"#))
            .unwrap();
        let response_key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let result = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response),
                    response_key,
                    connector,
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        let errors = &result.response.body().errors;
        assert_eq!(
            errors.len(),
            1,
            "expected exactly one error, got: {errors:?}"
        );
        let http = errors[0]
            .extensions
            .get("http")
            .and_then(|v| v.as_object())
            .expect("extensions.http should be an object");

        assert_eq!(
            http.get("myField").and_then(|v| v.as_str()),
            Some("literal Value"),
            "user-supplied extensions.http.myField should appear in the response"
        );
        assert_eq!(
            http.get("status").and_then(|v| v.as_i64()),
            Some(500),
            "default extensions.http.status should be preserved when the user sets sibling keys under extensions.http"
        );
    }

    // Covers the nested-collision case across all three contributors to
    // `extensions`: the default (`http: { status }`), the source-level
    // `errors.extensions` mapping, and the connect-level `errors.extensions`
    // mapping. With deep-merge, sibling keys under a shared nested object
    // (`http`) from each layer should all survive — last-writer-wins only at
    // a leaf collision, not at the parent object level.
    #[tokio::test]
    async fn errors_as_data_deep_merges_nested_extensions_across_source_and_connect() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_2,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: ConnectorErrorsSettings {
                message: None,
                source_extensions: Some(
                    JSONSelection::parse("http: { fromSource: $(\"a\") }").unwrap(),
                ),
                connect_extensions: Some(
                    JSONSelection::parse("http: { fromConnect: $(\"b\") }").unwrap(),
                ),
                connect_is_success: Some(JSONSelection::parse("$status->eq(200)").unwrap()),
            },
            output_type: None,
            label: "test label".into(),
        });

        let response: http::Response<RouterBody> = http::Response::builder()
            .status(500)
            .body(router::body::from_bytes(r#"{}"#))
            .unwrap();
        let response_key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let supergraph_request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );

        let result = super::aggregate_responses(
            vec![
                process_response(
                    Ok(response),
                    response_key,
                    connector,
                    &Context::default(),
                    (None, Default::default()),
                    None,
                    supergraph_request,
                    Default::default(),
                )
                .await
                .mapped_response,
            ],
            Context::new(),
        )
        .unwrap();

        let errors = &result.response.body().errors;
        assert_eq!(
            errors.len(),
            1,
            "expected exactly one error, got: {errors:?}"
        );
        let http = errors[0]
            .extensions
            .get("http")
            .and_then(|v| v.as_object())
            .expect("extensions.http should be an object");

        assert_eq!(
            http.get("status").and_then(|v| v.as_i64()),
            Some(500),
            "default extensions.http.status should be preserved alongside source- and connect-supplied siblings"
        );
        assert_eq!(
            http.get("fromSource").and_then(|v| v.as_str()),
            Some("a"),
            "source_extensions sibling under extensions.http should survive the connect_extensions merge"
        );
        assert_eq!(
            http.get("fromConnect").and_then(|v| v.as_str()),
            Some("b"),
            "connect_extensions sibling under extensions.http should appear alongside the source sibling"
        );
    }
}
