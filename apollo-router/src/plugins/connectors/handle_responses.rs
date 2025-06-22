use std::cell::LazyCell;
use std::sync::Arc;

use apollo_compiler::collections::HashMap;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::JSONSelection;
use apollo_federation::connectors::ProblemLocation;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::debug::ConnectorDebugHttpRequest;
use apollo_federation::connectors::runtime::debug::SelectionData;
use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
use apollo_federation::connectors::runtime::mapping::Problem;
use apollo_federation::connectors::runtime::mapping::aggregate_apply_to_errors;
use axum::body::HttpBody;
use encoding_rs::Encoding;
use encoding_rs::UTF_8;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use itertools::Itertools;
use mime::Mime;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tracing::Span;

use crate::Context;
use crate::graphql;
use crate::json_ext::Path;
use crate::plugins::connectors::make_requests::ResponseKey;
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
use crate::services::connector::request_service::Error;
use crate::services::fetch::AddSubgraphNameExt;
use crate::services::router;

const ENTITIES: &str = "_entities";
const TYPENAME: &str = "__typename";

// --- ERRORS ------------------------------------------------------------------

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(crate) enum HandleResponseError {
    /// Merge error: {0}
    MergeError(String),
}

// --- RAW RESPONSE ------------------------------------------------------------

enum RawResponse {
    /// This error type is used if:
    /// 1. We didn't even make the request (we hit the request limit)
    /// 2. We couldn't deserialize the response body
    Error {
        error: graphql::Error,
        key: ResponseKey,
    },
    /// Contains the response data directly from the HTTP response. We'll apply
    /// a selection to convert this into either `data` or `errors` based on
    /// whether it's successful or not.
    Data {
        parts: http::response::Parts,
        data: Value,
        key: ResponseKey,
        debug_request: (
            Option<Box<ConnectorDebugHttpRequest>>,
            Vec<(ProblemLocation, Problem)>,
        ),
    },
}

impl RawResponse {
    /// Returns a response with data transformed by the selection mapping.
    ///
    /// As a side effect, this will also write to the debug context.
    fn map_response(
        self,
        result: Result<TransportResponse, Error>,
        connector: Arc<Connector>,
        context: &Context,
        debug_context: &Option<Arc<Mutex<ConnectorContext>>>,
        supergraph_request: Arc<http::Request<crate::graphql::Request>>,
    ) -> connector::request_service::Response {
        let mapped_response = match self {
            RawResponse::Error { error, key } => MappedResponse::Error { error, key },
            RawResponse::Data {
                data,
                key,
                parts,
                debug_request,
            } => {
                let inputs = key
                    .inputs()
                    .clone()
                    .merger(&connector.response_variables)
                    .config(connector.config.as_ref())
                    .context(context)
                    .status(parts.status.as_u16())
                    .request(&connector.response_headers, supergraph_request.headers())
                    .response(&connector.response_headers, Some(&parts))
                    .env(&connector.env)
                    .merge();

                let (res, apply_to_errors) = key.selection().apply_with_vars(&data, &inputs);

                let mapping_problems: Vec<Problem> =
                    aggregate_apply_to_errors(apply_to_errors).collect();

                if let Some(debug) = debug_context {
                    let mut debug_problems: Vec<(ProblemLocation, Problem)> = mapping_problems
                        .iter()
                        .map(|problem| (ProblemLocation::Selection, problem.clone()))
                        .collect();
                    debug_problems.extend(debug_request.1);

                    debug.lock().push_response(
                        debug_request.0.clone(),
                        &parts,
                        &data,
                        Some(SelectionData {
                            source: connector.selection.to_string(),
                            transformed: key.selection().to_string(),
                            result: res.clone(),
                        }),
                        &connector.error_settings,
                        debug_problems,
                    );
                }

                MappedResponse::Data {
                    key,
                    data: res.unwrap_or_else(|| Value::Null),
                    problems: mapping_problems,
                }
            }
        };

        connector::request_service::Response {
            context: context.clone(),
            connector: connector.clone(),
            transport_result: result,
            mapped_response,
        }
    }

    /// Returns a `MappedResponse` with a GraphQL error.
    ///
    /// As a side effect, this will also write to the debug context.
    fn map_error(
        self,
        result: Result<TransportResponse, Error>,
        connector: Arc<Connector>,
        context: &Context,
        debug_context: &Option<Arc<Mutex<ConnectorContext>>>,
        supergraph_request: Arc<http::Request<crate::graphql::Request>>,
    ) -> connector::request_service::Response {
        use serde_json_bytes::*;

        let mapped_response = match self {
            RawResponse::Error { error, key } => MappedResponse::Error { error, key },
            RawResponse::Data {
                key,
                parts,
                debug_request,
                data,
            } => {
                let mut warnings = Vec::new();

                let inputs = LazyCell::new(|| {
                    key.inputs()
                        .clone()
                        .merger(&connector.response_variables)
                        .config(connector.config.as_ref())
                        .context(context)
                        .status(parts.status.as_u16())
                        .request(&connector.response_headers, supergraph_request.headers())
                        .response(&connector.response_headers, Some(&parts))
                        .merge()
                });

                // Do we have a error message mapping set for this connector?
                let message = if let Some(message_selection) = &connector.error_settings.message {
                    let (res, apply_to_errors) = message_selection.apply_with_vars(&data, &inputs);
                    warnings.extend(
                        aggregate_apply_to_errors(apply_to_errors)
                            .map(|problem| (ProblemLocation::ErrorsMessage, problem))
                            .collect::<Vec<_>>(),
                    );

                    res.as_ref()
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                } else {
                    "Request failed".to_string()
                };

                // Now we can create the error object using either the default message or the message calculated by the JSONSelection
                let mut error = graphql::Error::builder()
                    .message(message)
                    .path::<Path>((&key).into());

                // First, we will apply defaults... these may get overwritten below by user configured extensions
                error = error
                    .extension(
                        "http",
                        Value::Object(Map::from_iter([(
                            "status".into(),
                            Value::Number(parts.status.as_u16().into()),
                        )])),
                    )
                    .extension(
                        "connector",
                        Value::Object(Map::from_iter([(
                            "coordinate".into(),
                            Value::String(connector.id.coordinate().into()),
                        )])),
                    );

                // If we have error extensions mapping set for this connector, we will need to grab the code + the remaining extensions and map them to the error object
                // We'll merge by applying the source and then the connect. Keep in mind that these will override defaults if the key names are the same.
                // Note: that we set the extension code in this if/else but don't actually set it on the error until after the if/else. This is because the compiler
                // can't make sense of it in the if/else due to how the builder is constructed.
                let mut extension_code = "CONNECTOR_FETCH".to_string();
                if let Some(extensions_selection) = &connector.error_settings.source_extensions {
                    let (res, apply_to_errors) =
                        extensions_selection.apply_with_vars(&data, &inputs);
                    warnings.extend(
                        aggregate_apply_to_errors(apply_to_errors)
                            .map(|problem| (ProblemLocation::SourceErrorsExtensions, problem))
                            .collect::<Vec<_>>(),
                    );

                    // TODO: Currently this "fails silently". In the future, we probably add a warning to the debugger info.
                    let extensions = res
                        .and_then(|e| match e {
                            Value::Object(map) => Some(map),
                            _ => None,
                        })
                        .unwrap_or_default();

                    if let Some(code) = extensions.get("code") {
                        extension_code = code.as_str().unwrap_or_default().to_string();
                    }

                    for (key, value) in extensions {
                        error = error.extension(key.clone(), value.clone());
                    }
                }

                if let Some(extensions_selection) = &connector.error_settings.connect_extensions {
                    let (res, apply_to_errors) =
                        extensions_selection.apply_with_vars(&data, &inputs);
                    warnings.extend(
                        aggregate_apply_to_errors(apply_to_errors)
                            .map(|problem| (ProblemLocation::ConnectErrorsExtensions, problem))
                            .collect::<Vec<_>>(),
                    );

                    // TODO: Currently this "fails silently". In the future, we probably add a warning to the debugger info.
                    let extensions = res
                        .and_then(|e| match e {
                            Value::Object(map) => Some(map),
                            _ => None,
                        })
                        .unwrap_or_default();

                    if let Some(code) = extensions.get("code") {
                        extension_code = code.as_str().unwrap_or_default().to_string();
                    }

                    for (key, value) in extensions {
                        error = error.extension(key.clone(), value.clone());
                    }
                }

                // Now we can finally build the actual error!
                let error = error
                    .extension_code(extension_code)
                    .build()
                    // Always set the subgraph name and if required, it will get filtered out by the include_subgraph_errors plugin
                    .with_subgraph_name(&connector.id.subgraph_name);

                if let Some(debug) = debug_context {
                    debug.lock().push_response(
                        debug_request.0.clone(),
                        &parts,
                        &data,
                        None,
                        &connector.error_settings,
                        [debug_request.1, warnings]
                            .iter()
                            .flatten()
                            .cloned()
                            .collect(),
                    );
                }

                MappedResponse::Error { error, key }
            }
        };

        if let MappedResponse::Error {
            error: ref mapped_error,
            key: _,
        } = mapped_response
        {
            if let Some(Value::String(error_code)) = mapped_error.extensions.get("code") {
                emit_error_event(
                    error_code.as_str(),
                    &mapped_error.message,
                    mapped_error.path.clone(),
                );
            }
        }

        connector::request_service::Response {
            context: context.clone(),
            connector: connector.clone(),
            transport_result: result,
            mapped_response,
        }
    }
}

// --- MAPPED RESPONSE ---------------------------------------------------------
#[derive(Debug)]
pub(crate) enum MappedResponse {
    /// This is equivalent to RawResponse::Error, but it also represents errors
    /// when the request is semantically unsuccessful (e.g. 404, 500).
    Error {
        error: graphql::Error,
        key: ResponseKey,
    },
    /// The response data after applying the selection mapping.
    Data {
        data: Value,
        key: ResponseKey,
        problems: Vec<Problem>,
    },
}

impl MappedResponse {
    /// Adds the response data to the `data` map or the error to the `errors`
    /// array. How data is added depends on the `ResponseKey`: it's either a
    /// property directly on the map, or stored in the `_entities` array.
    fn add_to_data(
        self,
        data: &mut serde_json_bytes::Map<ByteString, Value>,
        errors: &mut Vec<graphql::Error>,
        count: usize,
    ) -> Result<(), HandleResponseError> {
        match self {
            Self::Error { error, key, .. } => {
                match key {
                    // add a null to the "_entities" array at the right index
                    ResponseKey::Entity { index, .. } | ResponseKey::EntityField { index, .. } => {
                        let entities = data
                            .entry(ENTITIES)
                            .or_insert(Value::Array(Vec::with_capacity(count)));
                        entities
                            .as_array_mut()
                            .ok_or_else(|| {
                                HandleResponseError::MergeError("_entities is not an array".into())
                            })?
                            .insert(index, Value::Null);
                    }
                    _ => {}
                };
                errors.push(error);
            }
            Self::Data {
                data: value, key, ..
            } => match key {
                ResponseKey::RootField { ref name, .. } => {
                    data.insert(name.clone(), value);
                }
                ResponseKey::Entity { index, .. } => {
                    let entities = data
                        .entry(ENTITIES)
                        .or_insert(Value::Array(Vec::with_capacity(count)));
                    entities
                        .as_array_mut()
                        .ok_or_else(|| {
                            HandleResponseError::MergeError("_entities is not an array".into())
                        })?
                        .insert(index, value);
                }
                ResponseKey::EntityField {
                    index,
                    ref field_name,
                    ref typename,
                    ..
                } => {
                    let entities = data
                        .entry(ENTITIES)
                        .or_insert(Value::Array(Vec::with_capacity(count)))
                        .as_array_mut()
                        .ok_or_else(|| {
                            HandleResponseError::MergeError("_entities is not an array".into())
                        })?;

                    match entities.get_mut(index) {
                        Some(Value::Object(entity)) => {
                            entity.insert(field_name.clone(), value);
                        }
                        _ => {
                            let mut entity = serde_json_bytes::Map::new();
                            if let Some(typename) = typename {
                                entity.insert(TYPENAME, Value::String(typename.as_str().into()));
                            }
                            entity.insert(field_name.clone(), value);
                            entities.insert(index, Value::Object(entity));
                        }
                    };
                }
                ResponseKey::BatchEntity { keys, inputs, .. } => {
                    let Value::Array(values) = value else {
                        return Err(HandleResponseError::MergeError(
                            "Response for a batch request does not map to an array".into(),
                        ));
                    };

                    let key_selection: Result<JSONSelection, _> = keys.try_into();
                    let key_selection = key_selection
                        .map_err(|e| HandleResponseError::MergeError(e.to_string()))?;

                    // Convert representations into keys for use in the map
                    let key_values = inputs.batch.iter().map(|v| {
                        key_selection
                            .apply_to(&Value::Object(v.clone()))
                            .0
                            .unwrap_or(Value::Null)
                    });

                    // Create a map of keys to entities
                    let mut map = values
                        .into_iter()
                        .filter_map(|v| key_selection.apply_to(&v).0.map(|key| (key, v)))
                        .collect::<HashMap<_, _>>();

                    // Make a list of entities that matches the representations list
                    let new_entities = key_values
                        .map(|key| map.remove(&key).unwrap_or(Value::Null))
                        .collect_vec();

                    // Because we may have multiple batch entities requests, we should add to ENTITIES as the requests come in so it is additive
                    let entities = data
                        .entry(ENTITIES)
                        .or_insert(Value::Array(Vec::with_capacity(count)));

                    entities
                        .as_array_mut()
                        .ok_or_else(|| {
                            HandleResponseError::MergeError("_entities is not an array".into())
                        })?
                        .extend(new_entities);
                }
            },
        }

        Ok(())
    }
}

// --- handle_responses --------------------------------------------------------

pub(crate) async fn process_response<T: HttpBody>(
    result: Result<http::Response<T>, Error>,
    response_key: ResponseKey,
    connector: Arc<Connector>,
    context: &Context,
    debug_request: (
        Option<Box<ConnectorDebugHttpRequest>>,
        Vec<(ProblemLocation, Problem)>,
    ),
    debug_context: &Option<Arc<Mutex<ConnectorContext>>>,
    supergraph_request: Arc<http::Request<crate::graphql::Request>>,
) -> connector::request_service::Response {
    match result {
        // This occurs when we short-circuit the request when over the limit
        Err(error) => {
            let raw = RawResponse::Error {
                error: error.to_graphql_error(connector.clone(), Some((&response_key).into())),
                key: response_key,
            };
            Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            raw.map_error(
                Err(error),
                connector,
                context,
                debug_context,
                supergraph_request,
            )
        }
        Ok(response) => {
            let (parts, body) = response.into_parts();

            let result = Ok(TransportResponse::Http(HttpResponse {
                inner: parts.clone(),
            }));

            // If this errors, it will write to the debug context because it
            // has access to the raw bytes, so we can't write to it again
            // in any RawResponse::Error branches.
            let raw = match deserialize_response(
                body,
                &parts,
                connector.clone(),
                context,
                &response_key,
                debug_context,
                &debug_request,
            )
            .await
            {
                Ok(data) => RawResponse::Data {
                    parts,
                    data,
                    key: response_key,
                    debug_request,
                },
                Err(error) => RawResponse::Error {
                    error,
                    key: response_key,
                },
            };
            let is_success = match &raw {
                RawResponse::Error { .. } => false,
                RawResponse::Data { parts, .. } => parts.status.is_success(),
            };
            if is_success {
                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_OK);
                raw.map_response(
                    result,
                    connector,
                    context,
                    debug_context,
                    supergraph_request,
                )
            } else {
                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                raw.map_error(
                    result,
                    connector,
                    context,
                    debug_context,
                    supergraph_request,
                )
            }
        }
    }
}

pub(crate) fn aggregate_responses(
    responses: Vec<MappedResponse>,
) -> Result<Response, HandleResponseError> {
    let mut data = serde_json_bytes::Map::new();
    let mut errors = Vec::new();
    let count = responses.len();

    for mapped in responses {
        mapped.add_to_data(&mut data, &mut errors, count)?;
    }

    let data = if data.is_empty() {
        Value::Null
    } else {
        Value::Object(data)
    };

    Span::current().record(
        OTEL_STATUS_CODE,
        if errors.is_empty() {
            OTEL_STATUS_CODE_OK
        } else {
            OTEL_STATUS_CODE_ERROR
        },
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

/// Converts the response body to bytes and deserializes it into a json Value.
/// This is the last time we have access to the original bytes, so it's the only
/// opportunity to write the invalid response to the debug context.
async fn deserialize_response<T: HttpBody>(
    body: T,
    parts: &http::response::Parts,
    connector: Arc<Connector>,
    context: &Context,
    response_key: &ResponseKey,
    debug_context: &Option<Arc<Mutex<ConnectorContext>>>,
    debug_request: &(
        Option<Box<ConnectorDebugHttpRequest>>,
        Vec<(ProblemLocation, Problem)>,
    ),
) -> Result<Value, graphql::Error> {
    use serde_json_bytes::*;

    let make_err = |path: Path| {
        graphql::Error::builder()
            .message("The server returned data in an unexpected format.".to_string())
            .extension_code("CONNECTOR_RESPONSE_INVALID")
            .extension("service", connector.id.subgraph_name.clone())
            .extension(
                "http",
                Value::Object(Map::from_iter([(
                    "status".into(),
                    Value::Number(parts.status.as_u16().into()),
                )])),
            )
            .extension(
                "connector",
                Value::Object(Map::from_iter([(
                    "coordinate".into(),
                    Value::String(connector.id.coordinate().into()),
                )])),
            )
            .path(path)
            .build()
            .with_subgraph_name(&connector.id.subgraph_name) // for include_subgraph_errors
    };

    let path: Path = response_key.into();
    let body = &router::body::into_bytes(body)
        .await
        .map_err(|_| make_err(path.clone()))?;

    let log_response_level = context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorEventResponse>().cloned())
        .and_then(|event| {
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
                connector: connector.clone(),
                transport_result: Ok(TransportResponse::Http(HttpResponse {
                    inner: parts.clone(),
                })),
                mapped_response: MappedResponse::Data {
                    data: Value::Null,
                    key: response_key.clone(),
                    problems: vec![],
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
        #[cfg(test)]
        let headers = {
            let mut headers: indexmap::IndexMap<String, http::HeaderValue> = parts
                .headers
                .clone()
                .into_iter()
                .filter_map(|(name, val)| Some((name?.to_string(), val)))
                .collect();
            headers.sort_keys();
            headers
        };
        #[cfg(not(test))]
        let headers = &parts.headers;

        attrs.push(KeyValue::new(
            HTTP_RESPONSE_HEADERS,
            opentelemetry::Value::String(format!("{:?}", headers).into()),
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
            opentelemetry::Value::String(
                String::from_utf8(body.clone().to_vec())
                    .unwrap_or_default()
                    .into(),
            ),
        ));

        log_event(
            level,
            "connector.response",
            attrs,
            &format!(
                "Response from connector {label:?}",
                label = connector.id.label
            ),
        );
    }

    // If the body is obviously empty, don't try to parse it
    if let Some(content_length) = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|len| len.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
    {
        if content_length == 0 {
            return Ok(Value::Null);
        }
    }

    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok()?.parse::<Mime>().ok());

    if content_type.is_none()
        || content_type
            .as_ref()
            .is_some_and(|ct| ct.subtype() == mime::JSON || ct.suffix() == Some(mime::JSON))
    {
        // Treat any JSON-y like content types as JSON
        // Also, because the HTTP spec says we should effectively "guess" the content type if there is no content type (None), we're going to guess it is JSON if the server has not specified one
        match serde_json::from_slice::<Value>(body) {
            Ok(json_data) => Ok(json_data),
            Err(_) => {
                if let Some(debug_context) = debug_context {
                    debug_context.lock().push_invalid_response(
                        debug_request.0.clone(),
                        parts,
                        body,
                        &connector.error_settings,
                        debug_request.1.clone(),
                    );
                }
                Err(make_err(path))
            }
        }
    } else if content_type
        .as_ref()
        .is_some_and(|ct| ct.type_() == mime::TEXT && ct.subtype() == mime::PLAIN)
    {
        // Plain text we can't parse as JSON so we'll instead return it as a JSON string
        // Before we can do that, we need to figure out the charset and attempt to decode the string
        let encoding = content_type
            .as_ref()
            .and_then(|ct| Encoding::for_label(ct.get_param("charset")?.as_str().as_bytes()))
            .unwrap_or(UTF_8);
        let (decoded_body, _, had_errors) = encoding.decode(body);

        if had_errors {
            if let Some(debug_context) = debug_context {
                debug_context.lock().push_invalid_response(
                    debug_request.0.clone(),
                    parts,
                    body,
                    &connector.error_settings,
                    debug_request.1.clone(),
                );
            }
            return Err(make_err(path));
        }

        Ok(Value::String(decoded_body.into_owned().into()))
    } else {
        // For any other content types, all we can do is treat it as a JSON null cause we don't know what it is
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use apollo_compiler::Schema;
    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::EntityResolver;
    use apollo_federation::connectors::HTTPMethod;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::runtime::inputs::RequestInputs;
    use http::Uri;
    use insta::assert_debug_snapshot;
    use itertools::Itertools;

    use crate::Context;
    use crate::graphql;
    use crate::plugins::connectors::handle_responses::process_response;
    use crate::plugins::connectors::make_requests::ResponseKey;
    use crate::services::router;
    use crate::services::router::body::RouterBody;

    #[tokio::test]
    async fn test_handle_responses_root_fields() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_url: Some(Uri::from_str("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            env: Default::default(),
            error_settings: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request.clone(),
            )
            .await
            .mapped_response,
            process_response(
                Ok(response2),
                response_key2,
                connector,
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request,
            )
            .await
            .mapped_response,
        ])
        .unwrap();

        assert_debug_snapshot!(res, @r###"
        Response {
            response: Response {
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
            },
        }
        "###);
    }

    #[tokio::test]
    async fn test_handle_responses_entities() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(user),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_url: Some(Uri::from_str("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("$.data { id }").unwrap(),
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            env: Default::default(),
            error_settings: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request.clone(),
            )
            .await
            .mapped_response,
            process_response(
                Ok(response2),
                response_key2,
                connector,
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request,
            )
            .await
            .mapped_response,
        ])
        .unwrap();

        assert_debug_snapshot!(res, @r###"
        Response {
            response: Response {
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
            },
        }
        "###);
    }

    #[tokio::test]
    async fn test_handle_responses_batch() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_2,
            id: ConnectId::new_on_object(
                "subgraph_name".into(),
                None,
                name!(User),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_url: Some(Uri::from_str("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Post,
                body: Some(JSONSelection::parse("ids: $batch.id").unwrap()),
                ..Default::default()
            },
            selection: JSONSelection::parse("$.data { id name }").unwrap(),
            entity_resolver: Some(EntityResolver::TypeBatch),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            env: Default::default(),
            error_settings: Default::default(),
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
            .cloned()
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request,
            )
            .await
            .mapped_response,
        ])
        .unwrap();

        assert_debug_snapshot!(res, @r#"
        Response {
            response: Response {
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
            },
        }
        "#);
    }

    #[tokio::test]
    async fn test_handle_responses_entity_field() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(User),
                name!(field),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_url: Some(Uri::from_str("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: Some(EntityResolver::Implicit),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            env: Default::default(),
            error_settings: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request.clone(),
            )
            .await
            .mapped_response,
            process_response(
                Ok(response2),
                response_key2,
                connector,
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request,
            )
            .await
            .mapped_response,
        ])
        .unwrap();

        assert_debug_snapshot!(res, @r###"
        Response {
            response: Response {
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
            },
        }
        "###);
    }

    #[tokio::test]
    async fn test_handle_responses_errors() {
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(user),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_url: Some(Uri::from_str("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            env: Default::default(),
            error_settings: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response_plaintext),
                response_key_plaintext,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request.clone(),
            )
            .await
            .mapped_response,
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request.clone(),
            )
            .await
            .mapped_response,
            process_response(
                Ok(response2),
                response_key2,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request.clone(),
            )
            .await
            .mapped_response,
            process_response(
                Ok(response3),
                response_key3,
                connector,
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request,
            )
            .await
            .mapped_response,
        ])
        .unwrap();

        assert_debug_snapshot!(res, @r#"
        Response {
            response: Response {
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
                                "service": String(
                                    "subgraph_name",
                                ),
                                "http": Object({
                                    "status": Number(200),
                                }),
                                "connector": Object({
                                    "coordinate": String(
                                        "subgraph_name:Query.user@connect[0]",
                                    ),
                                }),
                                "code": String(
                                    "CONNECTOR_RESPONSE_INVALID",
                                ),
                                "apollo.private.subgraph.name": String(
                                    "subgraph_name",
                                ),
                            },
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
                                "http": Object({
                                    "status": Number(404),
                                }),
                                "connector": Object({
                                    "coordinate": String(
                                        "subgraph_name:Query.user@connect[0]",
                                    ),
                                }),
                                "code": String(
                                    "CONNECTOR_FETCH",
                                ),
                                "apollo.private.subgraph.name": String(
                                    "subgraph_name",
                                ),
                            },
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
                                "http": Object({
                                    "status": Number(500),
                                }),
                                "connector": Object({
                                    "coordinate": String(
                                        "subgraph_name:Query.user@connect[0]",
                                    ),
                                }),
                                "code": String(
                                    "CONNECTOR_FETCH",
                                ),
                                "apollo.private.subgraph.name": String(
                                    "subgraph_name",
                                ),
                            },
                        },
                    ],
                    extensions: {},
                    has_next: None,
                    subscribed: None,
                    created_at: None,
                    incremental: [],
                },
            },
        }
        "#);
    }

    #[tokio::test]
    async fn test_handle_responses_status() {
        let selection = JSONSelection::parse("$status").unwrap();
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_url: Some(Uri::from_str("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: selection.clone(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            batch_settings: None,
            response_variables: selection
                .variable_references()
                .map(|var_ref| var_ref.namespace.namespace)
                .collect(),
            request_headers: Default::default(),
            response_headers: Default::default(),
            env: Default::default(),
            error_settings: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector,
                &Context::default(),
                (None, Default::default()),
                &None,
                supergraph_request,
            )
            .await
            .mapped_response,
        ])
        .unwrap();

        assert_debug_snapshot!(res, @r###"
        Response {
            response: Response {
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
            },
        }
        "###);
    }
}
