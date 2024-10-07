use std::sync::Arc;

use apollo_federation::sources::connect::Connector;
use http_body::Body as HttpBody;
use parking_lot::Mutex;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tracing::Span;

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
    /// Invalid response body: {0}
    InvalidResponseBody(String),

    /// Merge error: {0}
    MergeError(String),
}

// --- RESPONSES ---------------------------------------------------------------

pub(crate) async fn handle_responses<T: HttpBody>(
    responses: Vec<ConnectorResponse<T>>,
    connector: &Connector,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<Response, HandleResponseError> {
    use HandleResponseError::*;

    let mut data = serde_json_bytes::Map::new();
    let mut errors = Vec::new();
    let count = responses.len();
    for response in responses {
        let mut error = None;
        let response_key = response.key;
        let debug_request = response.debug_request;

        match response.result {
            ConnectorResult::Err(e) => {
                error = Some(e.to_graphql_error(connector, None));
            }
            ConnectorResult::HttpResponse(response) => {
                let (parts, body) = response.into_parts();
                let body = &hyper::body::to_bytes(body).await.map_err(|_| {
                    InvalidResponseBody("couldn't retrieve http response body".into())
                })?;

                if parts.status.is_success() {
                    let json_data = match serde_json::from_slice::<Value>(body) {
                        Ok(body) => body,
                        Err(e) => {
                            if let Some(debug) = debug {
                                debug
                                    .lock()
                                    .push_invalid_response(debug_request, &parts, body);
                            }
                            Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                            // TODO this stops processing all responses
                            return Err(InvalidResponseBody(format!(
                                "couldn't deserialize response body: {e}"
                            )));
                        }
                    };

                    let mut res_data = {
                        let (res, apply_to_errors) = response_key.selection().apply_with_vars(
                            &json_data,
                            &response_key.inputs().merge(connector.config.as_ref(), None),
                        );

                        if let Some(ref debug) = debug {
                            debug.lock().push_response(
                                debug_request,
                                &parts,
                                &json_data,
                                Some(SelectionData {
                                    source: connector.selection.to_string(),
                                    transformed: response_key.selection().to_string(),
                                    result: res.clone(),
                                    errors: apply_to_errors,
                                }),
                            );
                        }
                        res.unwrap_or_else(|| Value::Null)
                    };

                    match response_key {
                        // add the response to the "data" using the root field name or alias
                        ResponseKey::RootField {
                            ref name,
                            ref typename,
                            ..
                        } => {
                            if let ResponseTypeName::Concrete(typename) = typename {
                                inject_typename(&mut res_data, typename);
                            }

                            data.insert(name.clone(), res_data);
                        }

                        // add the response to the "_entities" array at the right index
                        ResponseKey::Entity {
                            index,
                            ref typename,
                            ..
                        } => {
                            if let ResponseTypeName::Concrete(typename) = typename {
                                inject_typename(&mut res_data, typename);
                            }

                            let entities = data
                                .entry(ENTITIES)
                                .or_insert(Value::Array(Vec::with_capacity(count)));
                            entities
                                .as_array_mut()
                                .ok_or_else(|| MergeError("entities is not an array".into()))?
                                .insert(index, res_data);
                        }

                        // make an entity object and assign the response to the appropriate field or aliased field,
                        // then add the object to the _entities array at the right index (or add the field to an existing object)
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
                                .ok_or_else(|| MergeError("entities is not an array".into()))?;

                            match entities.get_mut(index) {
                                Some(Value::Object(entity)) => {
                                    entity.insert(field_name.clone(), res_data);
                                }
                                _ => {
                                    let mut entity = serde_json_bytes::Map::new();
                                    if let ResponseTypeName::Concrete(typename) = typename {
                                        entity.insert(
                                            TYPENAME,
                                            Value::String(typename.clone().into()),
                                        );
                                    }
                                    entity.insert(field_name.clone(), res_data);
                                    entities.insert(index, Value::Object(entity));
                                }
                            };
                        }
                    }
                } else {
                    error = Some(
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
                        .add_subgraph_name(&connector.id.subgraph_name),
                    );
                    if let Some(ref debug) = debug {
                        match serde_json::from_slice(body) {
                            Ok(json_data) => {
                                debug
                                    .lock()
                                    .push_response(debug_request, &parts, &json_data, None);
                            }
                            Err(_) => {
                                debug
                                    .lock()
                                    .push_invalid_response(debug_request, &parts, body);
                            }
                        }
                    }
                }
            }
        }

        if let Some(error) = error {
            match response_key {
                // add a null to the "_entities" array at the right index
                ResponseKey::Entity { index, .. } | ResponseKey::EntityField { index, .. } => {
                    let entities = data
                        .entry(ENTITIES)
                        .or_insert(Value::Array(Vec::with_capacity(count)));
                    entities
                        .as_array_mut()
                        .ok_or_else(|| MergeError("entities is not an array".into()))?
                        .insert(index, Value::Null);
                }
                _ => {}
            };
            errors.push(error);
        }
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
            selection: JSONSelection::parse("$.data").unwrap().1,
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":"world"}"#).into())
            .expect("response builder");
        let response_key1 = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("String".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
        };

        let response2 = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":"world"}"#).into())
            .expect("response builder");
        let response_key2 = ResponseKey::RootField {
            name: "hello2".to_string(),
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("String".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
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
            selection: JSONSelection::parse("$.data { id }").unwrap().1,
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":{"id": "1"}}"#).into())
            .expect("response builder");
        let response_key1 = ResponseKey::Entity {
            index: 0,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
        };

        let response2 = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":{"id": "2"}}"#).into())
            .expect("response builder");
        let response_key2 = ResponseKey::Entity {
            index: 1,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
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
            selection: JSONSelection::parse("$.data").unwrap().1,
            entity_resolver: Some(EntityResolver::Implicit),
            config: Default::default(),
            max_requests: None,
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":"value1"}"#).into())
            .expect("response builder");
        let response_key1 = ResponseKey::EntityField {
            index: 0,
            inputs: Default::default(),
            field_name: "field".to_string(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
        };

        let response2 = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":"value2"}"#).into())
            .expect("response builder");
        let response_key2 = ResponseKey::EntityField {
            index: 1,
            inputs: Default::default(),
            field_name: "field".to_string(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
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
            selection: JSONSelection::parse("$.data").unwrap().1,
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
        };

        let response1: http::Response<RouterBody> = http::Response::builder()
            .status(404)
            .body(hyper::Body::from(r#"{"error":"not found"}"#).into())
            .expect("response builder");
        let response_key1 = ResponseKey::Entity {
            index: 0,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
        };

        let response2 = http::Response::builder()
            .body(hyper::Body::from(r#"{"data":{"id":"2"}}"#).into())
            .expect("response builder");
        let response_key2 = ResponseKey::Entity {
            index: 1,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
        };

        let response3 = http::Response::builder()
            .status(500)
            .body(hyper::Body::from(r#"{"error":"whoops"}"#).into())
            .expect("response builder");
        let response_key3 = ResponseKey::Entity {
            index: 2,
            inputs: Default::default(),
            typename: ResponseTypeName::Concrete("User".to_string()),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap().1),
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
}
