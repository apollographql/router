use std::sync::Arc;

use apollo_federation::sources::connect::Connector;
use axum::body::HttpBody;
use http::header::CONTENT_LENGTH;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tracing::Span;

use crate::Context;
use crate::graphql;
use crate::json_ext::Path;
use crate::plugins::connectors::make_requests::ResponseKey;
use crate::plugins::connectors::mapping::Problem;
use crate::plugins::connectors::mapping::aggregate_apply_to_errors;
use crate::plugins::connectors::plugin::debug::ConnectorContext;
use crate::plugins::connectors::plugin::debug::ConnectorDebugHttpRequest;
use crate::plugins::connectors::plugin::debug::SelectionData;
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
use crate::services::connector::request_service::TransportResponse;
use crate::services::connector::request_service::transport::http::HttpResponse;
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
        debug_request: Option<ConnectorDebugHttpRequest>,
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
    ) -> connector::request_service::Response {
        let mapped_response = match self {
            RawResponse::Error { error, key } => MappedResponse::Error { error, key },
            RawResponse::Data {
                data,
                key,
                parts,
                debug_request,
            } => {
                let inputs = key.inputs().merge(
                    &connector.response_variables,
                    connector.config.as_ref(),
                    context,
                    Some(parts.status.as_u16()),
                );

                let (res, apply_to_errors) = key.selection().apply_with_vars(&data, &inputs);

                let mapping_problems = aggregate_apply_to_errors(&apply_to_errors);

                if let Some(debug) = debug_context {
                    debug.lock().push_response(
                        debug_request.clone(),
                        &parts,
                        &data,
                        Some(SelectionData {
                            source: connector.selection.to_string(),
                            transformed: key.selection().to_string(),
                            result: res.clone(),
                            errors: mapping_problems.clone(),
                        }),
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
    // TODO: This is where we'd map the response to a top-level GraphQL error
    // once we have an error mapping. For now, it just creates a basic top-level
    // error with the status code.
    fn map_error(
        self,
        result: Result<TransportResponse, Error>,
        connector: Arc<Connector>,
        context: &Context,
        debug_context: &Option<Arc<Mutex<ConnectorContext>>>,
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
                let error = graphql::Error::builder()
                    .message("Request failed".to_string())
                    .extension_code("CONNECTOR_FETCH")
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
                    .path::<Path>((&key).into())
                    .build()
                    .add_subgraph_name(&connector.id.subgraph_name); // for include_subgraph_errors

                if let Some(debug) = debug_context {
                    debug
                        .lock()
                        .push_response(debug_request.clone(), &parts, &data, None);
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
                emit_error_event(error_code.as_str(), "Connector error occurred");
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
    debug_request: Option<ConnectorDebugHttpRequest>,
    debug_context: &Option<Arc<Mutex<ConnectorContext>>>,
) -> connector::request_service::Response {
    match result {
        // This occurs when we short-circuit the request when over the limit
        Err(error) => {
            let raw = RawResponse::Error {
                error: error.to_graphql_error(connector.clone(), Some((&response_key).into())),
                key: response_key,
            };
            Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            raw.map_error(Err(error), connector, context, debug_context)
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
                raw.map_response(result, connector, context, debug_context)
            } else {
                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                raw.map_error(result, connector, context, debug_context)
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
    debug_request: &Option<ConnectorDebugHttpRequest>,
) -> Result<Value, graphql::Error> {
    use serde_json_bytes::*;

    let make_err = |path: Path| {
        graphql::Error::builder()
            .message("Request failed".to_string())
            .extension_code("CONNECTOR_FETCH")
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
            .add_subgraph_name(&connector.id.subgraph_name) // for include_subgraph_errors
    };

    let path: Path = response_key.into();
    let body = &router::body::into_bytes(body)
        .await
        .map_err(|_| make_err(path.clone()))?;

    let log_response_level = context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorEventResponse>().cloned())
        .and_then(|event| match event.0.condition() {
            Some(condition) => {
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
                if condition.lock().evaluate_response(&response) {
                    Some(event.0.level())
                } else {
                    None
                }
            }
            None => Some(event.0.level()),
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

    match serde_json::from_slice::<Value>(body) {
        Ok(json_data) => Ok(json_data),
        Err(_) => {
            if let Some(debug_context) = debug_context {
                debug_context
                    .lock()
                    .push_invalid_response(debug_request.clone(), parts, body);
            }

            Err(make_err(path))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::ConnectSpec;
    use apollo_federation::sources::connect::Connector;
    use apollo_federation::sources::connect::EntityResolver;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HttpJsonTransport;
    use apollo_federation::sources::connect::JSONSelection;
    use insta::assert_debug_snapshot;
    use url::Url;

    use crate::Context;
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                None,
                &None,
            )
            .await
            .mapped_response,
            process_response(
                Ok(response2),
                response_key2,
                connector,
                &Context::default(),
                None,
                &None,
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("$.data { id }").unwrap(),
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                None,
                &None,
            )
            .await
            .mapped_response,
            process_response(
                Ok(response2),
                response_key2,
                connector,
                &Context::default(),
                None,
                &None,
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: Some(EntityResolver::Implicit),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                None,
                &None,
            )
            .await
            .mapped_response,
            process_response(
                Ok(response2),
                response_key2,
                connector,
                &Context::default(),
                None,
                &None,
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response_plaintext),
                response_key_plaintext,
                connector.clone(),
                &Context::default(),
                None,
                &None,
            )
            .await
            .mapped_response,
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                None,
                &None,
            )
            .await
            .mapped_response,
            process_response(
                Ok(response2),
                response_key2,
                connector.clone(),
                &Context::default(),
                None,
                &None,
            )
            .await
            .mapped_response,
            process_response(
                Ok(response3),
                response_key3,
                connector,
                &Context::default(),
                None,
                &None,
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
                                    "CONNECTOR_FETCH",
                                ),
                                "fetch_subgraph_name": String(
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
                                "service": String(
                                    "subgraph_name",
                                ),
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
                                "fetch_subgraph_name": String(
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
                                "service": String(
                                    "subgraph_name",
                                ),
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
                                "fetch_subgraph_name": String(
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
        "###);
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: selection.clone(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: selection.external_variables().collect(),
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector,
                &Context::default(),
                None,
                &None,
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
