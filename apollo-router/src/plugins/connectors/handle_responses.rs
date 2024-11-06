use std::sync::Arc;

use apollo_federation::sources::connect::Connector;
use http_body::Body as HttpBody;
use parking_lot::Mutex;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tracing::Span;

use super::plugin::ConnectorDebugHttpRequest;
use crate::error::FetchError;
use crate::graphql;
use crate::plugins::connectors::http::Response as ConnectorResponse;
use crate::plugins::connectors::http::Result as ConnectorResult;
use crate::plugins::connectors::make_requests::ResponseKey;
use crate::plugins::connectors::make_requests::ResponseTypeName;
use crate::plugins::connectors::plugin::ConnectorContext;
use crate::plugins::connectors::plugin::SelectionData;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;
use crate::services::connect::Response;
use crate::services::fetch::AddSubgraphNameExt;

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
    /// a selection to
    Data {
        parts: http::response::Parts,
        data: Value,
        key: ResponseKey,
        debug: Option<ConnectorDebugHttpRequest>,
    },
}

impl RawResponse {
    /// Returns a `MappedResponse` with the response data transformed by the
    /// selection mapping.
    ///
    /// As a side effect, this will also write to the debug context.
    fn map_response(
        self,
        connector: &Connector,
        debug: &Option<Arc<Mutex<ConnectorContext>>>,
    ) -> MappedResponse {
        match self {
            RawResponse::Error { error, key } => MappedResponse::Error { error, key },
            RawResponse::Data {
                data,
                key,
                parts,
                debug: debug_request,
            } => {
                let (res, apply_to_errors) = key.selection().apply_with_vars(
                    &data,
                    &key.inputs().merge(
                        connector.config.as_ref(),
                        None,
                        Some(parts.status.as_u16()),
                    ),
                );

                if let Some(ref debug) = debug {
                    debug.lock().push_response(
                        debug_request.clone(),
                        &parts,
                        &data,
                        Some(SelectionData {
                            source: connector.selection.to_string(),
                            transformed: key.selection().to_string(),
                            result: res.clone(),
                            errors: apply_to_errors,
                        }),
                    );
                }

                MappedResponse::Data {
                    key,
                    data: res.unwrap_or_else(|| Value::Null),
                }
            }
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
        connector: &Connector,
        debug: &Option<Arc<Mutex<ConnectorContext>>>,
    ) -> MappedResponse {
        match self {
            RawResponse::Error { error, key } => MappedResponse::Error { error, key },
            RawResponse::Data {
                key,
                parts,
                debug: debug_request,
                data,
            } => {
                let error = FetchError::SubrequestHttpError {
                    status_code: Some(parts.status.as_u16()),
                    service: connector.id.label.clone(),
                    reason: format!(
                        "{}: {}",
                        parts.status.as_str(),
                        parts.status.canonical_reason().unwrap_or("Unknown")
                    ),
                }
                .to_graphql_error(None)
                .add_subgraph_name(&connector.id.subgraph_name);

                if let Some(ref debug) = debug {
                    debug
                        .lock()
                        .push_response(debug_request.clone(), &parts, &data, None);
                }

                MappedResponse::Error { error, key }
            }
        }
    }
}

// --- MAPPED RESPONSE ---------------------------------------------------------

enum MappedResponse {
    /// This is equivalent to RawResponse::Error, but it also represents errors
    /// when the request is semantically unsuccessful (e.g. 404, 500).
    Error {
        error: graphql::Error,
        key: ResponseKey,
    },
    /// The is the response data after applying the selection mapping.
    Data { data: Value, key: ResponseKey },
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
                data: mut value,
                key,
                ..
            } => match key {
                ResponseKey::RootField {
                    ref name,
                    ref typename,
                    ..
                } => {
                    if let ResponseTypeName::Concrete(typename) = typename {
                        inject_typename(&mut value, typename);
                    }

                    data.insert(name.clone(), value);
                }
                ResponseKey::Entity {
                    index,
                    ref typename,
                    ..
                } => {
                    if let ResponseTypeName::Concrete(typename) = typename {
                        inject_typename(&mut value, typename);
                    }

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
                            if let ResponseTypeName::Concrete(typename) = typename {
                                entity.insert(TYPENAME, Value::String(typename.clone().into()));
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

pub(crate) async fn handle_responses<T: HttpBody>(
    responses: Vec<ConnectorResponse<T>>,
    connector: &Connector,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<Response, HandleResponseError> {
    let futures_vec = responses
        .into_iter()
        .map(|response| async move {
            let response_key = response.key;
            let debug_request = response.debug_request;

            match response.result {
                // This occurs when we short-circuit the request when over the limit
                ConnectorResult::Err(error) => RawResponse::Error {
                    error: error.to_graphql_error(connector, None),
                    key: response_key,
                },
                ConnectorResult::HttpResponse(response) => {
                    let (parts, body) = response.into_parts();

                    // If this errors, it will write to the debug context because it
                    // has access to the raw bytes, so we can't write to it again
                    // in any RawResponse::Error branches.
                    match deserialize_response(body, &parts, connector, debug).await {
                        Ok(data) => RawResponse::Data {
                            parts,
                            data,
                            key: response_key,
                            debug: debug_request,
                        },
                        Err(error) => RawResponse::Error {
                            error,
                            key: response_key,
                        },
                    }
                }
            }
        })
        .collect::<Vec<_>>();

    let responses = futures::future::join_all(futures_vec).await;

    let mut data = serde_json_bytes::Map::new();
    let mut errors = Vec::new();
    let count = responses.len();

    for raw in responses {
        let is_success = match &raw {
            RawResponse::Error { .. } => false,
            RawResponse::Data { parts, .. } => parts.status.is_success(),
        };

        let mapped = if is_success {
            raw.map_response(connector, debug)
        } else {
            raw.map_error(connector, debug)
        };

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
    connector: &Connector,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<Value, graphql::Error> {
    let make_err = || {
        FetchError::SubrequestHttpError {
            status_code: Some(parts.status.as_u16()),
            service: connector.id.label.clone(),
            reason: format!(
                "{}: {}",
                parts.status.as_str(),
                parts.status.canonical_reason().unwrap_or("Unknown")
            ),
        }
        .to_graphql_error(None)
        .add_subgraph_name(&connector.id.subgraph_name)
    };

    let body = &hyper::body::to_bytes(body).await.map_err(|_| make_err())?;
    match serde_json::from_slice::<Value>(body) {
        Ok(json_data) => Ok(json_data),
        Err(_) => {
            if let Some(ref debug) = debug {
                debug.lock().push_invalid_response(None, parts, body);
            }

            Err(make_err())
        }
    }
}

fn inject_typename(data: &mut Value, typename: &str) {
    match data {
        Value::Array(data) => {
            for data in data {
                inject_typename(data, typename);
            }
        }
        Value::Object(data) => {
            data.insert(
                ByteString::from(TYPENAME),
                Value::String(ByteString::from(typename)),
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::Connector;
    use apollo_federation::sources::connect::EntityResolver;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HttpJsonTransport;
    use apollo_federation::sources::connect::JSONSelection;
    use insta::assert_debug_snapshot;
    use url::Url;

    use crate::plugins::connectors::http::Response as ConnectorResponse;
    use crate::plugins::connectors::make_requests::ResponseKey;
    use crate::plugins::connectors::make_requests::ResponseTypeName;
    use crate::services::router::body::RouterBody;

    #[tokio::test]
    async fn test_handle_responses_root_fields() {
        let connector = Connector {
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
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":"world"}"#).into())
            .unwrap();
        let response_key1 = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("String".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response2 = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":"world"}"#).into())
            .unwrap();
        let response_key2 = ResponseKey::RootField {
            name: "hello2".to_string(),
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("String".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let res = super::handle_responses(
            vec![
                ConnectorResponse {
                    result: response1.into(),
                    key: response_key1,
                    debug_request: None,
                },
                ConnectorResponse {
                    result: response2.into(),
                    key: response_key2,
                    debug_request: None,
                },
            ],
            &connector,
            &None,
        )
        .await
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
        let connector = Connector {
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
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":{"id": "1"}}"#).into())
            .unwrap();
        let response_key1 = ResponseKey::Entity {
            index: 0,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response2 = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":{"id": "2"}}"#).into())
            .unwrap();
        let response_key2 = ResponseKey::Entity {
            index: 1,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let res = super::handle_responses(
            vec![
                ConnectorResponse {
                    result: response1.into(),
                    key: response_key1,
                    debug_request: None,
                },
                ConnectorResponse {
                    result: response2.into(),
                    key: response_key2,
                    debug_request: None,
                },
            ],
            &connector,
            &None,
        )
        .await
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
                                    "__typename": String(
                                        "User",
                                    ),
                                }),
                                Object({
                                    "id": String(
                                        "2",
                                    ),
                                    "__typename": String(
                                        "User",
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
        let connector = Connector {
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
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":"value1"}"#).into())
            .unwrap();
        let response_key1 = ResponseKey::EntityField {
            index: 0,
            inputs: Default::default(),
            field_name: "field".to_string(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response2 = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":"value2"}"#).into())
            .unwrap();
        let response_key2 = ResponseKey::EntityField {
            index: 1,
            inputs: Default::default(),
            field_name: "field".to_string(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let res = super::handle_responses(
            vec![
                ConnectorResponse {
                    result: response1.into(),
                    key: response_key1,
                    debug_request: None,
                },
                ConnectorResponse {
                    result: response2.into(),
                    key: response_key2,
                    debug_request: None,
                },
            ],
            &connector,
            &None,
        )
        .await
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
        let connector = Connector {
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
        };

        let response_plaintext: http::Response<RouterBody> = http::Response::builder()
            .body(hyper::Body::from(r#"plain text"#).into())
            .unwrap();
        let response_key_plaintext = ResponseKey::Entity {
            index: 0,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .status(404)
            .body(hyper::Body::from(r#"{"error":"not found"}"#).into())
            .unwrap();
        let response_key1 = ResponseKey::Entity {
            index: 1,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response2 = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":{"id":"2"}}"#).into())
            .unwrap();
        let response_key2 = ResponseKey::Entity {
            index: 2,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let response3 = http::Response::builder()
            .status(500)
            .body(hyper::Body::from(r#"{"error":"whoops"}"#).into())
            .unwrap();
        let response_key3 = ResponseKey::Entity {
            index: 3,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let res = super::handle_responses(
            vec![
                ConnectorResponse {
                    result: response_plaintext.into(),
                    key: response_key_plaintext,
                    debug_request: None,
                },
                ConnectorResponse {
                    result: response1.into(),
                    key: response_key1,
                    debug_request: None,
                },
                ConnectorResponse {
                    result: response2.into(),
                    key: response_key2,
                    debug_request: None,
                },
                ConnectorResponse {
                    result: response3.into(),
                    key: response_key3,
                    debug_request: None,
                },
            ],
            &connector,
            &None,
        )
        .await
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
                                    "__typename": String(
                                        "User",
                                    ),
                                }),
                                Null,
                            ]),
                        }),
                    ),
                    path: None,
                    errors: [
                        Error {
                            message: "HTTP fetch failed from 'test label': 200: OK",
                            locations: [],
                            path: None,
                            extensions: {
                                "code": String(
                                    "SUBREQUEST_HTTP_ERROR",
                                ),
                                "service": String(
                                    "test label",
                                ),
                                "reason": String(
                                    "200: OK",
                                ),
                                "http": Object({
                                    "status": Number(200),
                                }),
                                "fetch_subgraph_name": String(
                                    "subgraph_name",
                                ),
                            },
                        },
                        Error {
                            message: "HTTP fetch failed from 'test label': 404: Not Found",
                            locations: [],
                            path: None,
                            extensions: {
                                "code": String(
                                    "SUBREQUEST_HTTP_ERROR",
                                ),
                                "service": String(
                                    "test label",
                                ),
                                "reason": String(
                                    "404: Not Found",
                                ),
                                "http": Object({
                                    "status": Number(404),
                                }),
                                "fetch_subgraph_name": String(
                                    "subgraph_name",
                                ),
                            },
                        },
                        Error {
                            message: "HTTP fetch failed from 'test label': 500: Internal Server Error",
                            locations: [],
                            path: None,
                            extensions: {
                                "code": String(
                                    "SUBREQUEST_HTTP_ERROR",
                                ),
                                "service": String(
                                    "test label",
                                ),
                                "reason": String(
                                    "500: Internal Server Error",
                                ),
                                "http": Object({
                                    "status": Number(500),
                                }),
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
        let connector = Connector {
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
            selection: JSONSelection::parse("$status").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .status(201)
            .body(hyper::Body::from(r#"{}"#).into())
            .unwrap();
        let response_key1 = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("Int".to_string()),
            selection: Arc::new(JSONSelection::parse("$status").unwrap()),
        };

        let res = super::handle_responses(
            vec![ConnectorResponse {
                result: response1.into(),
                key: response_key1,
                debug_request: None,
            }],
            &connector,
            &None,
        )
        .await
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
