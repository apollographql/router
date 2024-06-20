use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use apollo_federation::sources::connect::ApplyTo;
use apollo_federation::sources::connect::Connector;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;

use crate::plugins::connectors::make_requests::ResponseKey;
use crate::plugins::connectors::make_requests::ResponseTypeName;
use crate::services::connect;
use crate::services::router::body::RouterBody;

const ENTITIES: &str = "_entities";
const TYPENAME: &str = "__typename";

// --- ERRORS ------------------------------------------------------------------

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(crate) enum HandleResponseError {
    /// Missing response key
    MissingResponseKey,

    /// Invalid response body: {0}
    InvalidResponseBody(String),

    /// Merge error: {0}
    MergeError(String),
}

// --- RESPONSES ---------------------------------------------------------------

pub(crate) async fn handle_responses(
    responses: Vec<http::Response<RouterBody>>,
    connector: &Connector,
    _schema: &Valid<Schema>,   // TODO for future apply_with_selection
    _document: Option<String>, // TODO pass in relevant selection set, not the whole operation
) -> Result<connect::Response, HandleResponseError> {
    use HandleResponseError::*;

    let mut data = serde_json_bytes::Map::new();
    let mut errors = Vec::new();
    let count = responses.len();

    for response in responses {
        let (parts, body) = response.into_parts();

        let response_key = parts
            .extensions
            .get::<ResponseKey>()
            .ok_or(MissingResponseKey)?;

        let body = &hyper::body::to_bytes(body)
            .await
            .map_err(|_| InvalidResponseBody("couldn't retrieve http response body".into()))?;

        if parts.status.is_success() {
            let json_data: Value = serde_json::from_slice(body)
                .map_err(|_| InvalidResponseBody("couldn't deserialize response body".into()))?;

            let mut res_data = {
                let (res, _apply_to_errors) = connector.selection.apply_to(&json_data);
                // TODO log apply_to_errors as diagnostics
                res.unwrap_or_else(|| Value::Null)
            };

            match response_key {
                // add the response to the "data" using the root field name or alias
                ResponseKey::RootField {
                    ref name,
                    ref typename,
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
                        .insert(*index, res_data);
                }

                // make an entity object and assign the response to the appropriate field or aliased field,
                // then add the object to the _entities array at the right index (or add the field to an existing object)
                ResponseKey::EntityField {
                    index,
                    ref field_name,
                    ref typename,
                } => {
                    let entities = data
                        .entry(ENTITIES)
                        .or_insert(Value::Array(Vec::with_capacity(count)))
                        .as_array_mut()
                        .ok_or_else(|| MergeError("entities is not an array".into()))?;

                    match entities.get_mut(*index) {
                        Some(Value::Object(entity)) => {
                            entity.insert(field_name.clone(), res_data);
                        }
                        _ => {
                            let mut entity = serde_json_bytes::Map::new();
                            if let ResponseTypeName::Concrete(typename) = typename {
                                entity.insert(TYPENAME, Value::String(typename.clone().into()));
                            }
                            entity.insert(field_name.clone(), res_data);
                            entities.insert(*index, Value::Object(entity));
                        }
                    };
                }
            }
        } else {
            match response_key {
                // add a null to the "_entities" array at the right index
                ResponseKey::Entity { index, .. } | ResponseKey::EntityField { index, .. } => {
                    let entities = data
                        .entry(ENTITIES)
                        .or_insert(Value::Array(Vec::with_capacity(count)));
                    entities
                        .as_array_mut()
                        .ok_or_else(|| MergeError("entities is not an array".into()))?
                        .insert(*index, Value::Null);
                }
                _ => {}
            };

            errors.push(
                crate::graphql::Error::builder()
                    .message(format!("http error: {}", parts.status))
                    // todo path: ["_entities", i, "???"]
                    .extension_code(format!("{}", parts.status.as_u16()))
                    .extension("connector", connector.id.label.clone())
                    .build(),
            );
        }
    }

    let data = if data.is_empty() {
        Value::Null
    } else {
        Value::Object(data)
    };
    Ok((data, errors))
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
    use apollo_compiler::name;
    use apollo_compiler::Schema;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::Connector;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HttpJsonTransport;
    use apollo_federation::sources::connect::JSONSelection;
    use apollo_federation::sources::connect::Transport;
    use apollo_federation::sources::connect::URLPathTemplate;
    use insta::assert_debug_snapshot;

    use crate::plugins::connectors::make_requests::ResponseKey;
    use crate::plugins::connectors::make_requests::ResponseTypeName;

    #[tokio::test]
    async fn test_handle_responses_root_fields() {
        let connector = Connector {
            id: ConnectId::new(
                "subgraph_name".into(),
                name!(Query),
                name!(hello),
                0,
                "test label",
            ),
            transport: Transport::HttpJson(HttpJsonTransport {
                base_url: "http://localhost/api".into(),
                path_template: URLPathTemplate::parse("/path").unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            entity: false,
            on_root_type: true,
        };

        let response1 = http::Response::builder()
            .extension(ResponseKey::RootField {
                name: "hello".to_string(),
                typename: ResponseTypeName::Concrete("String".to_string()),
            })
            .body(hyper::Body::from(r#"{"data":"world"}"#).into())
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(ResponseKey::RootField {
                name: "hello2".to_string(),
                typename: ResponseTypeName::Concrete("String".to_string()),
            })
            .body(hyper::Body::from(r#"{"data":"world"}"#).into())
            .expect("response builder");

        let schema = Schema::parse_and_validate("type Query { hello: String }", "./").unwrap();

        let res = super::handle_responses(
            vec![response1, response2],
            &connector,
            &schema,
            Some("{hello hello2: hello}".to_string()),
        )
        .await
        .unwrap();

        assert_debug_snapshot!(res, @r###"
        (
            Object({
                "hello": String(
                    "world",
                ),
                "hello2": String(
                    "world",
                ),
            }),
            [],
        )
        "###);
    }

    #[tokio::test]
    async fn test_handle_responses_entities() {
        let connector = Connector {
            id: ConnectId::new(
                "subgraph_name".into(),
                name!(Query),
                name!(user),
                0,
                "test label",
            ),
            transport: Transport::HttpJson(HttpJsonTransport {
                base_url: "http://localhost/api".into(),
                path_template: URLPathTemplate::parse("/path").unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            }),
            selection: JSONSelection::parse(".data { id }").unwrap().1,
            entity: true,
            on_root_type: true,
        };

        let response1 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 0,
                typename: ResponseTypeName::Concrete("User".to_string()),
            })
            .body(hyper::Body::from(r#"{"data":{"id": "1"}}"#).into())
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 1,
                typename: ResponseTypeName::Concrete("User".to_string()),
            })
            .body(hyper::Body::from(r#"{"data":{"id": "2"}}"#).into())
            .expect("response builder");

        let schema = Schema::parse_and_validate(
            "type Query { user(id: ID!): User }
            type User { id: ID! }",
            "./",
        )
        .unwrap();

        let res = super::handle_responses(
            vec![response1, response2],
            &connector,
            &schema,
            Some("query ($representations: [_Any]) {_entities(representations: $representations) { ... on User { id }}}".to_string()),
        )
        .await
        .unwrap();

        assert_debug_snapshot!(res, @r###"
        (
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
            [],
        )
        "###);
    }

    #[tokio::test]
    async fn test_handle_responses_entity_field() {
        let connector = Connector {
            id: ConnectId::new(
                "subgraph_name".into(),
                name!(User),
                name!(field),
                0,
                "test label",
            ),
            transport: Transport::HttpJson(HttpJsonTransport {
                base_url: "http://localhost/api".into(),
                path_template: URLPathTemplate::parse("/path").unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            entity: false,
            on_root_type: false,
        };

        let response1 = http::Response::builder()
            .extension(ResponseKey::EntityField {
                index: 0,
                field_name: "field".to_string(),
                typename: ResponseTypeName::Concrete("User".to_string()),
            })
            .body(hyper::Body::from(r#"{"data":"value1"}"#).into())
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(ResponseKey::EntityField {
                index: 1,
                field_name: "field".to_string(),
                typename: ResponseTypeName::Concrete("User".to_string()),
            })
            .body(hyper::Body::from(r#"{"data":"value2"}"#).into())
            .expect("response builder");

        let schema = Schema::parse_and_validate(
            "type Query { _: Int } # just to make it valid
            type User { id: ID! field: String! }",
            "./",
        )
        .unwrap();

        let res = super::handle_responses(
            vec![response1, response2],
            &connector,
            &schema,
            Some("query ($representations: [_Any]) {_entities(representations: $representations) { ... on User { field }}}".to_string()),
        )
        .await
        .unwrap();

        assert_debug_snapshot!(res, @r###"
        (
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
            [],
        )
        "###);
    }

    #[tokio::test]
    async fn test_handle_responses_errors() {
        let connector = Connector {
            id: ConnectId::new(
                "subgraph_name".into(),
                name!(Query),
                name!(user),
                0,
                "test label",
            ),
            transport: Transport::HttpJson(HttpJsonTransport {
                base_url: "http://localhost/api".into(),
                path_template: URLPathTemplate::parse("/path").unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            entity: true,
            on_root_type: true,
        };

        let response1 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 0,

                typename: ResponseTypeName::Concrete("User".to_string()),
            })
            .status(404)
            .body(hyper::Body::from(r#"{"error":"not found"}"#).into())
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 1,

                typename: ResponseTypeName::Concrete("User".to_string()),
            })
            .body(hyper::Body::from(r#"{"data":{"id":"2"}}"#).into())
            .expect("response builder");

        let response3 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 2,
                typename: ResponseTypeName::Concrete("User".to_string()),
            })
            .status(500)
            .body(hyper::Body::from(r#"{"error":"whoops"}"#).into())
            .expect("response builder");

        let schema = Schema::parse_and_validate(
            "type Query { user(id: ID): User }
            type User { id: ID! }",
            "./",
        )
        .unwrap();

        let res = super::handle_responses(
            vec![response1, response2, response3],
            &connector,
            &schema,
            Some("query ($representations: [_Any]) {_entities(representations: $representations) { ... on User { id }}}".to_string()),
        )
        .await
        .unwrap();

        assert_debug_snapshot!(res, @r###"
        (
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
            [
                Error {
                    message: "http error: 404 Not Found",
                    locations: [],
                    path: None,
                    extensions: {
                        "connector": String(
                            "test label",
                        ),
                        "code": String(
                            "404",
                        ),
                    },
                },
                Error {
                    message: "http error: 500 Internal Server Error",
                    locations: [],
                    path: None,
                    extensions: {
                        "connector": String(
                            "test label",
                        ),
                        "code": String(
                            "500",
                        ),
                    },
                },
            ],
        )
        "###);
    }
}
