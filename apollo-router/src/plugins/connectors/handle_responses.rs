use std::sync::Arc;

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

// --- ERRORS ------------------------------------------------------------------

impl From<RuntimeError> for graphql::Error {
    fn from(error: RuntimeError) -> Self {
        let path: Path = (&error.path).into();

        let err = graphql::Error::builder()
            .message(&error.message)
            .extensions(error.extensions())
            .extension_code(error.code())
            .path(path)
            .build();

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
    debug_request: DebugRequest,
    debug_context: Option<&Arc<Mutex<ConnectorContext>>>,
    supergraph_request: Arc<http::Request<crate::graphql::Request>>,
) -> connector::request_service::Response {
    let (mapped_response, result) = match result {
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

            let make_err = || {
                let mut err = RuntimeError::new(
                    "The server returned data in an unexpected format.".to_string(),
                    &response_key,
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

            let deserialized_body = body
                .collect()
                .await
                .map_err(|_| ())
                .and_then(|body| {
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
                    });
                    log_connectors_event(context, &body, &parts, response_key.clone(), &connector);
                    raw
                })
                .map_err(|()| make_err());

            // If this errors, it will write to the debug context because it
            // has access to the raw bytes, so we can't write to it again
            // in any RawResponse::Error branches.
            let mapped = match &deserialized_body {
                Err(error) => MappedResponse::Error {
                    error: error.clone(),
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
                ),
            };

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

    if let MappedResponse::Error { ref error, .. } = mapped_response {
        emit_error_event(error.code(), &error.message, Some((*error.path).into()));
    }

    connector::request_service::Response {
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
                transport_result: Ok(TransportResponse::Http(HttpResponse {
                    inner: parts.clone(),
                })),
                mapped_response: MappedResponse::Data {
                    data: Value::Null,
                    key: response_key,
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
                .iter()
                .map(|(name, val)| (name.to_string(), val.clone()))
                .collect();
            headers.sort_keys();
            headers
        };
        #[cfg(not(test))]
        let headers = &parts.headers;

        attrs.push(KeyValue::new(
            HTTP_RESPONSE_HEADERS,
            opentelemetry::Value::String(format!("{headers:?}").into()),
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
    use apollo_federation::connectors::runtime::inputs::RequestInputs;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use insta::assert_debug_snapshot;
    use itertools::Itertools;
    use serde_json_bytes::json;

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
                None,
                0,
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                None,
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
                None,
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
                None,
                0,
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                None,
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
                None,
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
            id: ConnectId::new_on_object("subgraph_name".into(), None, name!(User), None, 0),
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
                None,
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
                None,
                0,
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                None,
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
                None,
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
                None,
                0,
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

        let mut res = super::aggregate_responses(vec![
            process_response(
                Ok(response_plaintext),
                response_key_plaintext,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                None,
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
                None,
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
                None,
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
                None,
                supergraph_request,
            )
            .await
            .mapped_response,
        ])
        .unwrap();

        // Overwrite error IDs to avoid random Uuid mismatch.
        // Since assert_debug_snapshot does not support redactions (which would be useful for error IDs),
        // we have to do it manually.
        let body = res.response.body_mut();
        body.errors = body.errors.iter_mut().map(|e| e.with_null_id()).collect();

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
                None,
                0,
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

        let res = super::aggregate_responses(vec![
            process_response(
                Ok(response1),
                response_key1,
                connector,
                &Context::default(),
                (None, Default::default()),
                None,
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
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
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
            error_settings,
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
        let res_expect_fail = super::aggregate_responses(vec![
            process_response(
                Ok(response_fail),
                response_fail_key,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                None,
                supergraph_request.clone(),
            )
            .await
            .mapped_response,
        ])
        .unwrap()
        .response;
        assert_eq!(res_expect_fail.body().data, Some(JsonValue::Null));
        assert_eq!(res_expect_fail.body().errors.len(), 1);

        // Make succeeding request
        let res_expect_success = super::aggregate_responses(vec![
            process_response(
                Ok(response_succeed),
                response_succeed_key,
                connector.clone(),
                &Context::default(),
                (None, Default::default()),
                None,
                supergraph_request.clone(),
            )
            .await
            .mapped_response,
        ])
        .unwrap()
        .response;
        assert!(res_expect_success.body().errors.is_empty());
        assert_eq!(
            &res_expect_success.body().data,
            &Some(json!({"hello": json!(400)}))
        );
    }
}
