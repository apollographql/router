use std::sync::Arc;

use apollo_federation::connectors::Connector;
use apollo_federation::connectors::ProblemLocation;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::debug::ConnectorDebugHttpRequest;
use apollo_federation::connectors::runtime::errors::Error;
use apollo_federation::connectors::runtime::errors::RuntimeError;
use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
use apollo_federation::connectors::runtime::key::ResponseKey;
use apollo_federation::connectors::runtime::mapping::Problem;
use apollo_federation::connectors::runtime::responses::HandleResponseError;
use apollo_federation::connectors::runtime::responses::MappedResponse;
use apollo_federation::connectors::runtime::responses::RawResponse;
use apollo_federation::connectors::runtime::responses::handle_raw_response;
use axum::body::HttpBody;
use encoding_rs::Encoding;
use encoding_rs::UTF_8;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use mime::Mime;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tracing::Span;

use crate::Context;
use crate::graphql;
use crate::json_ext::Path;
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
use crate::services::router;

// --- ERRORS ------------------------------------------------------------------

impl From<RuntimeError> for graphql::Error {
    fn from(error: RuntimeError) -> Self {
        let path: Path = (&error.path).into();

        let mut err = graphql::Error::builder()
            .message(&error.message)
            .extension_code(error.code())
            .path(path)
            .extensions(error.extensions.clone());

        if let Some(subgraph_name) = &error.subgraph_name {
            err = err.extension("service", Value::String(subgraph_name.clone().into()));
        };

        if let Some(coordinate) = &error.coordinate {
            err = err.extension(
                "connector",
                Value::Object(Map::from_iter([(
                    "coordinate".into(),
                    Value::String(coordinate.to_string().into()),
                )])),
            );
        }

        let err = err.build();

        if let Some(subgraph_name) = &error.subgraph_name {
            err.with_subgraph_name(subgraph_name)
        } else {
            err
        }
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
    let (mapped_response, result) = match result {
        // This occurs when we short-circuit the request when over the limit
        Err(error) => {
            let raw = RawResponse::Error {
                error: error.to_runtime_error(&connector, &response_key),
                key: response_key,
            };
            Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            (
                raw.map_error(
                    &connector,
                    context,
                    debug_context,
                    supergraph_request.headers(),
                ),
                Err(error),
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

            let (mapped, is_success) = handle_raw_response(
                raw,
                &connector,
                context,
                debug_context,
                supergraph_request.headers(),
            );

            if is_success {
                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_OK);
            } else {
                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            };

            (mapped, result)
        }
    };

    if let MappedResponse::Error { ref error, .. } = mapped_response {
        emit_error_event(error.code(), &error.message, Some((*error.path).into()));
    }

    connector::request_service::Response {
        context: context.clone(),
        connector: connector.clone(),
        transport_result: result,
        mapped_response,
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
                    .errors(errors.into_iter().map(|e| e.into()).collect())
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
) -> Result<Value, RuntimeError> {
    use serde_json_bytes::*;

    let make_err = || {
        let mut err = RuntimeError::new(
            "The server returned data in an unexpected format.".to_string(),
            response_key,
        );
        err.subgraph_name = Some(connector.id.subgraph_name.clone());
        err = err.with_code("CONNECTOR_RESPONSE_INVALID");
        err.coordinate = Some(connector.id.coordinate());
        err = err.extension(
            "http",
            Value::Object(Map::from_iter([(
                "status".into(),
                Value::Number(parts.status.as_u16().into()),
            )])),
        );
        err
    };

    let body = &router::body::into_bytes(body)
        .await
        .map_err(|_| make_err())?;

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
                Err(make_err())
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
            return Err(make_err());
        }

        Ok(Value::String(decoded_body.into_owned().into()))
    } else {
        // For any other content types, all we can do is treat it as a JSON null cause we don't know what it is
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::Schema;
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::EntityResolver;
    use apollo_federation::connectors::HTTPMethod;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::Namespace;
    use apollo_federation::connectors::runtime::inputs::RequestInputs;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use insta::assert_debug_snapshot;
    use itertools::Itertools;

    use crate::Context;
    use crate::graphql;
    use crate::plugins::connectors::handle_responses::process_response;
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
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
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
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
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
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Post,
                body: Some(JSONSelection::parse("ids: $batch.id").unwrap()),
                ..Default::default()
            },
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
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
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
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
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
                                "http": Object({
                                    "status": Number(200),
                                }),
                                "service": String(
                                    "subgraph_name",
                                ),
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
                                "service": String(
                                    "subgraph_name",
                                ),
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
                                "service": String(
                                    "subgraph_name",
                                ),
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
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
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
