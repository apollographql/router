use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use apollo_federation::sources::connect::ApplyTo;
use apollo_federation::sources::connect::Connector;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;

use crate::graphql;
use crate::plugins::connectors::make_requests::ResponseKey;
use crate::plugins::connectors::make_requests::ResponseTypeName;
use crate::plugins::connectors::plugin::ConnectorContext;
use crate::plugins::connectors::plugin::SelectionData;
use crate::services::connect::Response;
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
    debug: &mut Option<ConnectorContext>,
    _schema: &Valid<Schema>, // TODO for future apply_with_selection
) -> Result<Response, HandleResponseError> {
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
            let Ok(json_data) = serde_json::from_slice::<Value>(body) else {
                if let Some(ref mut debug) = debug {
                    debug.push_invalid_response(&parts, body);
                }
                return Err(InvalidResponseBody(
                    "couldn't deserialize response body".into(),
                ));
            };

            let mut res_data = {
                // TODO: caching of the transformed JSONSelection with the selection set applied?
                let transformed_selection = connector
                    .selection
                    .apply_selection_set(response_key.selection_set());

                let (res, apply_to_errors) = transformed_selection.apply_with_vars(
                    &json_data,
                    &response_key.inputs().merge(connector.config.as_ref()),
                );

                if let Some(ref mut debug) = debug {
                    debug.push_response(
                        &parts,
                        &json_data,
                        Some(SelectionData {
                            source: connector.selection.to_string(),
                            transformed: transformed_selection.to_string(),
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
                        .insert(*index, res_data);
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

            if let Some(ref mut debug) = debug {
                match serde_json::from_slice(body) {
                    Ok(json_data) => {
                        debug.push_response(&parts, &json_data, None);
                    }
                    Err(_) => {
                        debug.push_invalid_response(&parts, body);
                    }
                }
            }

            errors.push(
                graphql::Error::builder()
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
    use apollo_compiler::ast::FieldDefinition;
    use apollo_compiler::ast::Type;
    use apollo_compiler::executable::Field;
    use apollo_compiler::executable::Selection;
    use apollo_compiler::executable::SelectionSet;
    use apollo_compiler::name;
    use apollo_compiler::Name;
    use apollo_compiler::Node;
    use apollo_compiler::Schema;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::Connector;
    use apollo_federation::sources::connect::EntityResolver;
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
                None,
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
            entity_resolver: None,
            config: Default::default(),
        };

        let response1 = http::Response::builder()
            .extension(ResponseKey::RootField {
                name: "hello".to_string(),
                inputs: Default::default(),
                typename: ResponseTypeName::Concrete("String".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: Default::default(),
                },
            })
            .body(hyper::Body::from(r#"{"data":"world"}"#).into())
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(ResponseKey::RootField {
                name: "hello2".to_string(),
                inputs: Default::default(),
                typename: ResponseTypeName::Concrete("String".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: Default::default(),
                },
            })
            .body(hyper::Body::from(r#"{"data":"world"}"#).into())
            .expect("response builder");

        let schema = Schema::parse_and_validate("type Query { hello: String }", "./").unwrap();

        let res =
            super::handle_responses(vec![response1, response2], &connector, &mut None, &schema)
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
            transport: Transport::HttpJson(HttpJsonTransport {
                base_url: "http://localhost/api".into(),
                path_template: URLPathTemplate::parse("/path").unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            }),
            selection: JSONSelection::parse(".data { id }").unwrap().1,
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
        };

        let id_field_definition = FieldDefinition {
            description: None,
            name: Name::new("id").unwrap(),
            arguments: Default::default(),
            ty: Type::Named(Name::new("String").unwrap()),
            directives: Default::default(),
        };
        let id_field = Field::new(Name::new("id").unwrap(), Node::from(id_field_definition));
        let response1 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 0,
                inputs: Default::default(),
                typename: ResponseTypeName::Concrete("User".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: vec![Selection::Field(Node::new(id_field.clone()))],
                },
            })
            .body(hyper::Body::from(r#"{"data":{"id": "1"}}"#).into())
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 1,
                inputs: Default::default(),
                typename: ResponseTypeName::Concrete("User".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: vec![Selection::Field(Node::new(id_field.clone()))],
                },
            })
            .body(hyper::Body::from(r#"{"data":{"id": "2"}}"#).into())
            .expect("response builder");

        let schema = Schema::parse_and_validate(
            "type Query { user(id: ID!): User }
            type User { id: ID! }",
            "./",
        )
        .unwrap();

        let res =
            super::handle_responses(vec![response1, response2], &connector, &mut None, &schema)
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
            transport: Transport::HttpJson(HttpJsonTransport {
                base_url: "http://localhost/api".into(),
                path_template: URLPathTemplate::parse("/path").unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            entity_resolver: Some(EntityResolver::Implicit),
            config: Default::default(),
        };

        let response1 = http::Response::builder()
            .extension(ResponseKey::EntityField {
                index: 0,
                inputs: Default::default(),
                field_name: "field".to_string(),
                typename: ResponseTypeName::Concrete("User".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: Default::default(),
                },
            })
            .body(hyper::Body::from(r#"{"data":"value1"}"#).into())
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(ResponseKey::EntityField {
                index: 1,
                inputs: Default::default(),
                field_name: "field".to_string(),
                typename: ResponseTypeName::Concrete("User".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: Default::default(),
                },
            })
            .body(hyper::Body::from(r#"{"data":"value2"}"#).into())
            .expect("response builder");

        let schema = Schema::parse_and_validate(
            "type Query { _: Int } # just to make it valid
            type User { id: ID! field: String! }",
            "./",
        )
        .unwrap();

        let res =
            super::handle_responses(vec![response1, response2], &connector, &mut None, &schema)
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
            transport: Transport::HttpJson(HttpJsonTransport {
                base_url: "http://localhost/api".into(),
                path_template: URLPathTemplate::parse("/path").unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            entity_resolver: Some(EntityResolver::Explicit),
            config: Default::default(),
        };

        let response1 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 0,
                inputs: Default::default(),
                typename: ResponseTypeName::Concrete("User".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: Default::default(),
                },
            })
            .status(404)
            .body(hyper::Body::from(r#"{"error":"not found"}"#).into())
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 1,
                inputs: Default::default(),
                typename: ResponseTypeName::Concrete("User".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: Default::default(),
                },
            })
            .body(hyper::Body::from(r#"{"data":{"id":"2"}}"#).into())
            .expect("response builder");

        let response3 = http::Response::builder()
            .extension(ResponseKey::Entity {
                index: 2,
                inputs: Default::default(),
                typename: ResponseTypeName::Concrete("User".to_string()),
                selection_set: SelectionSet {
                    ty: name!(Todo), // TODO
                    selections: Default::default(),
                },
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
            &mut None,
            &schema,
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
