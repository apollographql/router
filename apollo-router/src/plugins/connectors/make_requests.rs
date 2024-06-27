use apollo_compiler::executable::Selection;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use apollo_federation::sources::connect::Connector;
use itertools::Itertools;
use serde_json_bytes::json;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use super::http_json_transport::make_request;
use super::http_json_transport::HttpJsonTransportError;
use crate::services::connect;
use crate::services::router::body::RouterBody;

const REPRESENTATIONS_VAR: &str = "representations";
const ENTITIES: &str = "_entities";
const TYPENAME: &str = "__typename";

#[derive(Debug, Default)]
struct RequestInputs {
    args: Map<ByteString, Value>,
    this: Map<ByteString, Value>,
}

impl RequestInputs {
    fn merge(self) -> Value {
        json!({
            "$args": self.args,
            "$this": self.this
        })
    }
}

#[derive(Clone)]
pub(crate) enum ResponseKey {
    RootField {
        name: String,
        typename: ResponseTypeName,
        #[allow(dead_code)]
        selection_set: apollo_compiler::executable::SelectionSet,
    },
    Entity {
        index: usize,
        typename: ResponseTypeName,
        #[allow(dead_code)]
        selection_set: apollo_compiler::executable::SelectionSet,
    },
    EntityField {
        index: usize,
        field_name: String,
        typename: ResponseTypeName,
        #[allow(dead_code)]
        selection_set: apollo_compiler::executable::SelectionSet,
    },
}

impl ResponseKey {
    #[allow(dead_code)]
    pub(crate) fn selection_set(&self) -> &apollo_compiler::executable::SelectionSet {
        match self {
            ResponseKey::RootField { selection_set, .. } => selection_set,
            ResponseKey::Entity { selection_set, .. } => selection_set,
            ResponseKey::EntityField { selection_set, .. } => selection_set,
        }
    }
}

// Vec<Selection> debug isn't deterministic when run in parallel tests
impl std::fmt::Debug for ResponseKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> std::fmt::Result {
        let selection_set = self
            .selection_set()
            .selections
            .iter()
            .map(|s| format!("{}", s))
            .join(" ");
        match self {
            ResponseKey::RootField { name, typename, .. } => f
                .debug_struct("RootField")
                .field("name", name)
                .field("typename", typename)
                .field("selection_set", &selection_set)
                .finish(),
            ResponseKey::Entity {
                index, typename, ..
            } => f
                .debug_struct("Entity")
                .field("index", index)
                .field("typename", typename)
                .field("selection_set", &selection_set)
                .finish(),
            ResponseKey::EntityField {
                index,
                field_name,
                typename,
                ..
            } => f
                .debug_struct("EntityField")
                .field("index", index)
                .field("field_name", field_name)
                .field("typename", typename)
                .field("selection_set", &selection_set)
                .finish(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ResponseTypeName {
    Concrete(String),
    /// For interfaceObject support. We don't want to include __typename in the
    /// response because this subgraph doesn't know the concrete type
    Omitted,
}

pub(crate) fn make_requests(
    request: connect::Request,
    connector: &Connector,
    schema: &Valid<Schema>,
) -> Result<Vec<(http::Request<RouterBody>, ResponseKey)>, MakeRequestError> {
    let request_params = if connector.entity {
        entities_from_request(&request, schema)
    } else if connector.on_root_type {
        root_fields(&request, schema)
    } else {
        entities_with_fields_from_request(&request, schema)
    }?;

    request_params_to_requests(connector, request_params, &request)
}

fn request_params_to_requests(
    connector: &Connector,
    request_params: Vec<(ResponseKey, RequestInputs)>,
    _original_request: &connect::Request, // TODO headers
) -> Result<Vec<(http::Request<RouterBody>, ResponseKey)>, MakeRequestError> {
    request_params
        .into_iter()
        .map(|(response_key, inputs)| {
            let request = match connector.transport {
                apollo_federation::sources::connect::Transport::HttpJson(ref transport) => {
                    make_request(transport, inputs.merge())?
                }
            };

            Ok((request, response_key))
        })
        .collect::<Result<Vec<_>, _>>()
}

// --- ERRORS ------------------------------------------------------------------

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(crate) enum MakeRequestError {
    /// Invalid request operation: {0}
    InvalidOperation(String),

    /// Unsupported request operation: {0}
    UnsupportedOperation(String),

    /// Invalid request arguments: {0}
    InvalidArguments(String),

    /// Invalid entity representation: {0}
    InvalidRepresentations(String),

    /// Cannot create HTTP request: {0}
    TransportError(#[from] HttpJsonTransportError),
}

// --- ROOT FIELDS -------------------------------------------------------------

/// Given a query, find the root fields and return a list of requests.
/// The connector subgraph must have only a single root field, but it could be
/// used multiple times with aliases.
///
/// Root fields exist in the supergraph schema so we can parse the operation
/// using the schema. (This isn't true for _entities oeprations.)
///
/// Example:
/// ```graphql
/// type Query {
///   foo(bar: String): Foo @connect(...)
/// }
/// ```
/// ```graphql
/// {
///   a: foo(bar: "a") # one request
///   b: foo(bar: "b") # another request
/// }
/// ```
fn root_fields(
    request: &connect::Request,
    schema: &Valid<Schema>,
) -> Result<Vec<(ResponseKey, RequestInputs)>, MakeRequestError> {
    use MakeRequestError::*;

    let op = request
        .operation
        .get_operation(None)
        .map_err(|_| InvalidOperation("no operation document".into()))?;

    let parent_type_name = schema.root_operation(op.operation_type).ok_or_else(|| {
        InvalidOperation(format!(
            "missing root operation of type {:?}",
            op.operation_type
        ))
    })?;

    op.selection_set
        .selections
        .iter()
        .map(|s| match s {
            Selection::Field(field) => {
                let response_name = field
                    .alias
                    .as_ref()
                    .unwrap_or_else(|| &field.name)
                    .to_string();
                let field_def = schema
                    .type_field(parent_type_name, &field.name)
                    .map_err(|_| {
                        InvalidOperation(format!(
                            "field {}.{} not found in schema",
                            parent_type_name, field.name
                        ))
                    })?;

                let response_key = ResponseKey::RootField {
                    name: response_name,
                    typename: ResponseTypeName::Concrete(
                        field_def.ty.inner_named_type().to_string(),
                    ),
                    selection_set: field.selection_set.clone(),
                };

                let args = graphql_utils::field_arguments_map(field, &request.variables.variables)
                    .map_err(|_| {
                        MakeRequestError::InvalidArguments(
                            "cannot get inputs from field arguments".into(),
                        )
                    })?;

                let request_inputs = RequestInputs {
                    args,
                    this: Default::default(),
                };

                Ok((response_key, request_inputs))
            }

            // The query planner removes fragments at the root so we don't have
            // to worry these branches
            Selection::FragmentSpread(_) | Selection::InlineFragment(_) => {
                Err(MakeRequestError::UnsupportedOperation(
                    "top-level fragments in query planner nodes should not happen".into(),
                ))
            }
        })
        .collect::<Result<Vec<_>, MakeRequestError>>()
}

// --- ENTITIES ----------------------------------------------------------------

/// Connectors marked with `entity: true` can be used as entity resolvers,
/// (resolving `_entities` queries) or regular root fields. For now we'll check
/// the existence of the `representations` variable to determine which use case
/// is relevant here.
///
/// If it's an entity resolver, we create separate requests for each item in the
/// representations array.
///
/// ```json
/// {
///   "variables": {
///      "representations": [{ "__typename": "User", "id": "1" }]
///   }
/// }
/// ```
///
/// Returns a list of request inputs and the response key (index in the array).
fn entities_from_request(
    request: &connect::Request,
    schema: &Valid<Schema>,
) -> Result<Vec<(ResponseKey, RequestInputs)>, MakeRequestError> {
    use MakeRequestError::*;

    let Some(representations) = request.variables.variables.get(REPRESENTATIONS_VAR) else {
        return root_fields(request, schema);
    };

    let op = request
        .operation
        .get_operation(None)
        .map_err(|_| InvalidOperation("no operation document".into()))?;

    let (entities_field, typename_requested) = graphql_utils::get_entity_fields(op)?;

    representations
        .as_array()
        .ok_or_else(|| InvalidRepresentations("representations is not an array".into()))?
        .iter()
        .enumerate()
        .map(|(i, rep)| {
            // TODO abstract types?
            let typename = rep
                .as_object()
                .ok_or_else(|| InvalidRepresentations("representation is not an object".into()))?
                .get(TYPENAME)
                .ok_or_else(|| {
                    InvalidRepresentations("representation is missing __typename".into())
                })?
                .as_str()
                .ok_or_else(|| InvalidRepresentations("__typename is not a string".into()))?
                .to_string();

            // if the fetch node operation doesn't include __typename, then
            // we're assuming this is for an interface object and we don't want
            // to include a __typename in the response.
            let typename = if typename_requested {
                ResponseTypeName::Concrete(typename)
            } else {
                ResponseTypeName::Omitted
            };

            Ok((
                ResponseKey::Entity {
                    index: i,
                    typename,
                    selection_set: entities_field.selection_set.clone(),
                },
                RequestInputs {
                    args: rep
                        .as_object()
                        .ok_or_else(|| {
                            InvalidRepresentations("representation is not an object".into())
                        })?
                        .clone(),
                    // entity connectors are always on Query fields, so they cannot use
                    // sibling fields with $this
                    this: Default::default(),
                },
            ))
        })
        .collect::<Result<Vec<_>, _>>()
}

// --- ENTITY FIELDS -----------------------------------------------------------

/// This is effectively the combination of the other two functions:
///
/// * It makes a request for each item in the `representations` array.
/// * If the connector field is aliased, it makes a request for each alias.
///
/// So it can return N (representations) x M (aliases) requests.
///
/// ```json
/// {
///   "query": "{ _entities(representations: $representations) { ... on User { name } } }",
///   "variables": { "representations": [{ "__typename": "User", "id": "1" }] }
/// }
/// ```
///
/// Return a list of request inputs with the response key (index in list and
/// name/alias of field) for each.
fn entities_with_fields_from_request(
    request: &connect::Request,
    _schema: &Valid<Schema>,
) -> Result<Vec<(ResponseKey, RequestInputs)>, MakeRequestError> {
    use MakeRequestError::*;

    let op = request
        .operation
        .get_operation(None)
        .map_err(|_| InvalidOperation("no operation document".into()))?;

    let (entities_field, typename_requested) = graphql_utils::get_entity_fields(op)?;

    let types_and_fields = entities_field
        .selection_set
        .selections
        .iter()
        .map(|selection| match selection {
            Selection::Field(_) => Ok(vec![]),

            Selection::FragmentSpread(_) => Err(InvalidOperation(
                "_entities selection can't be a named fragment".into(),
            )),

            Selection::InlineFragment(frag) => {
                let typename = frag
                    .type_condition
                    .as_ref()
                    .ok_or_else(|| InvalidOperation("missing type condition".into()))?;
                Ok(frag
                    .selection_set
                    .selections
                    .iter()
                    .map(|sel| {
                        let field = match sel {
                            Selection::Field(f) => f,
                            Selection::FragmentSpread(_) | Selection::InlineFragment(_) => {
                                return Err(InvalidOperation(
                                    "handling fragments inside entity selections not implemented"
                                        .into(),
                                ))
                            }
                        };
                        Ok((typename.to_string(), field))
                    })
                    .collect::<Result<Vec<_>, _>>()?)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let representations = request
        .variables
        .variables
        .get(REPRESENTATIONS_VAR)
        .ok_or_else(|| InvalidRepresentations("missing representations variable".into()))?
        .as_array()
        .ok_or_else(|| InvalidRepresentations("representations is not an array".into()))?
        .iter()
        .enumerate()
        .collect::<Vec<_>>();

    // if we have multiple fields (because of aliases, we'll flatten that list)
    // and generate requests for each field/representation pair
    types_and_fields
        .into_iter()
        .flatten()
        .flat_map(|(typename, field)| {
            representations.iter().map(move |(i, representation)| {
                let args = graphql_utils::field_arguments_map(field, &request.variables.variables)
                    .map_err(|_| {
                        InvalidArguments("cannot build inputs from field arguments".into())
                    })?;

                // if the fetch node operation doesn't include __typename, then
                // we're assuming this is for an interface object and we don't want
                // to include a __typename in the response.
                let typename = if typename_requested {
                    ResponseTypeName::Concrete(typename.to_string())
                } else {
                    ResponseTypeName::Omitted
                };

                let response_name = field
                    .alias
                    .as_ref()
                    .unwrap_or_else(|| &field.name)
                    .to_string();

                Ok::<_, MakeRequestError>((
                    ResponseKey::EntityField {
                        index: *i,
                        field_name: response_name.to_string(),
                        typename,
                        selection_set: field.selection_set.clone(),
                    },
                    RequestInputs {
                        args,
                        this: representation
                            .as_object()
                            .ok_or_else(|| {
                                InvalidRepresentations("representation is not an object".into())
                            })?
                            .clone(),
                    },
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::NodeStr;
    use apollo_compiler::Schema;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::Connector;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HttpJsonTransport;
    use apollo_federation::sources::connect::JSONSelection;
    use apollo_federation::sources::connect::URLPathTemplate;
    use insta::assert_debug_snapshot;

    use crate::graphql;
    use crate::query_planner::fetch::Variables;
    use crate::Context;

    #[test]
    fn test_root_fields_simple() {
        let schema = Arc::new(
            Schema::parse_and_validate("type Query { a: A } type A { f: String }", "./").unwrap(),
        );

        let req = crate::services::connect::Request::builder()
            .service_name(NodeStr::from("subgraph_Query_a_0"))
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &schema,
                    "query { a { f } a2: a { f2: f } }".to_string(),
                    "./",
                )
                .unwrap(),
            ))
            .variables(Variables {
                variables: Default::default(),
                inverted_paths: Default::default(),
                contextual_arguments: Default::default(),
            })
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(graphql::Request::builder().build())
                    .unwrap(),
            ))
            .build();

        assert_debug_snapshot!(super::root_fields(&req, &schema), @r###"
        Ok(
            [
                (
                    RootField {
                        name: "a",
                        typename: Concrete(
                            "A",
                        ),
                        selection_set: "f",
                    },
                    RequestInputs {
                        args: {},
                        this: {},
                    },
                ),
                (
                    RootField {
                        name: "a2",
                        typename: Concrete(
                            "A",
                        ),
                        selection_set: "f2: f",
                    },
                    RequestInputs {
                        args: {},
                        this: {},
                    },
                ),
            ],
        )
        "###);
    }

    #[test]
    fn test_root_fields_inputs() {
        let schema = Arc::new(
            Schema::parse_and_validate("type Query {b(var: String): String}", "./").unwrap(),
        );

        let req = crate::services::connect::Request::builder()
            .service_name(NodeStr::from("subgraph_Query_b_0"))
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &schema,
                    "query($var: String) { b(var: \"inline\") b2: b(var: $var) }".to_string(),
                    "./",
                )
                .unwrap(),
            ))
            .variables(Variables {
                variables: serde_json_bytes::json!({ "var": "variable" })
                    .as_object()
                    .unwrap()
                    .clone(),
                inverted_paths: Default::default(),
                contextual_arguments: Default::default(),
            })
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(graphql::Request::builder().build())
                    .unwrap(),
            ))
            .build();

        assert_debug_snapshot!(super::root_fields(&req, &schema), @r###"
        Ok(
            [
                (
                    RootField {
                        name: "b",
                        typename: Concrete(
                            "String",
                        ),
                        selection_set: "",
                    },
                    RequestInputs {
                        args: {
                            "var": String(
                                "inline",
                            ),
                        },
                        this: {},
                    },
                ),
                (
                    RootField {
                        name: "b2",
                        typename: Concrete(
                            "String",
                        ),
                        selection_set: "",
                    },
                    RequestInputs {
                        args: {
                            "var": String(
                                "variable",
                            ),
                        },
                        this: {},
                    },
                ),
            ],
        )
        "###);
    }

    #[test]
    fn test_root_fields_input_types() {
        let schema = Arc::new(Schema::parse_and_validate(
            r#"
            scalar JSON
            type Query {
              c(var1: Int, var2: Boolean, var3: Float, var4: ID, var5: JSON, var6: [String], var7: String): String
            }
          "#,
            "./",
        ).unwrap());

        let req = crate::services::connect::Request::builder()
        .service_name(NodeStr::from("subgraph_Query_c_0"))
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &schema,
                r#"
                query(
                    $var1: Int, $var2: Boolean, $var3: Float, $var4: ID, $var5: JSON, $var6: [String], $var7: String
                ) {
                    c(var1: $var1, var2: $var2, var3: $var3, var4: $var4, var5: $var5, var6: $var6, var7: $var7)
                    c2: c(
                        var1: 1,
                        var2: true,
                        var3: 0.9,
                        var4: "123",
                        var5: { a: 42 },
                        var6: ["item"],
                        var7: null
                    )
                }
                "#.to_string(),
                    "./",
                )
                .unwrap(),
            )
            )
            .variables(Variables {
                variables: serde_json_bytes::json!({
                        "var1": 1, "var2": true, "var3": 0.9,
                        "var4": "123", "var5": { "a": 42 }, "var6": ["item"],
                        "var7": null
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                inverted_paths: Default::default(),
                contextual_arguments: Default::default(),
            })
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(graphql::Request::builder().build())
                    .unwrap(),
            ))
            .build();

        assert_debug_snapshot!(super::root_fields(&req, &schema), @r###"
        Ok(
            [
                (
                    RootField {
                        name: "c",
                        typename: Concrete(
                            "String",
                        ),
                        selection_set: "",
                    },
                    RequestInputs {
                        args: {
                            "var1": Number(1),
                            "var2": Bool(
                                true,
                            ),
                            "var3": Number(0.9),
                            "var4": String(
                                "123",
                            ),
                            "var5": Object({
                                "a": Number(42),
                            }),
                            "var6": Array([
                                String(
                                    "item",
                                ),
                            ]),
                            "var7": Null,
                        },
                        this: {},
                    },
                ),
                (
                    RootField {
                        name: "c2",
                        typename: Concrete(
                            "String",
                        ),
                        selection_set: "",
                    },
                    RequestInputs {
                        args: {
                            "var1": Number(1),
                            "var2": Bool(
                                true,
                            ),
                            "var3": Number(0.9),
                            "var4": String(
                                "123",
                            ),
                            "var5": Object({
                                "a": Number(42),
                            }),
                            "var6": Array([
                                String(
                                    "item",
                                ),
                            ]),
                            "var7": Null,
                        },
                        this: {},
                    },
                ),
            ],
        )
        "###);
    }

    #[test]
    fn entities_from_request_entity() {
        let partial_sdl = r#"
        type Query {
          entity(id: ID!): Entity
        }

        type Entity {
          field: String
        }
        "#;
        let schema = Arc::new(Schema::parse_and_validate(partial_sdl, "./").unwrap());
        let subgraph_schema = Arc::new(
            Schema::parse_and_validate(
                format!(
                    r#"{partial_sdl}
        extend type Query {{
          _entities(representations: [_Any!]!): _Entity
        }}
        scalar _Any
        union _Entity = Entity
        "#
                ),
                "./",
            )
            .unwrap(),
        );

        let req = crate::services::connect::Request::builder()
            .service_name(NodeStr::from("subgraph_Query_entity_0"))
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &subgraph_schema,
                    r#"
                query($representations: [_Any!]!) {
                    _entities(representations: $representations) {
                        __typename
                        ... on Entity {
                            field
                            alias: field
                        }
                    }
                }
                "#
                    .to_string(),
                    "./",
                )
                .unwrap(),
            ))
            .variables(Variables {
                variables: serde_json_bytes::json!({
                    "representations": [
                        { "__typename": "Entity", "id": "1" },
                        { "__typename": "Entity", "id": "2" },
                    ]
                })
                .as_object()
                .unwrap()
                .clone(),
                inverted_paths: Default::default(),
                contextual_arguments: Default::default(),
            })
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(graphql::Request::builder().build())
                    .unwrap(),
            ))
            .build();

        assert_debug_snapshot!(super::entities_from_request(&req, &schema).unwrap(), @r###"
        [
            (
                Entity {
                    index: 0,
                    typename: Concrete(
                        "Entity",
                    ),
                    selection_set: "__typename ... on Entity {\n  field\n  alias: field\n}",
                },
                RequestInputs {
                    args: {
                        "__typename": String(
                            "Entity",
                        ),
                        "id": String(
                            "1",
                        ),
                    },
                    this: {},
                },
            ),
            (
                Entity {
                    index: 1,
                    typename: Concrete(
                        "Entity",
                    ),
                    selection_set: "__typename ... on Entity {\n  field\n  alias: field\n}",
                },
                RequestInputs {
                    args: {
                        "__typename": String(
                            "Entity",
                        ),
                        "id": String(
                            "2",
                        ),
                    },
                    this: {},
                },
            ),
        ]
        "###);
    }

    #[test]
    fn entities_from_request_root_field() {
        let partial_sdl = r#"
        type Query {
          entity(id: ID!): Entity
        }

        type Entity {
          field: T
        }

        type T {
          field: String
        }
        "#;
        let schema = Arc::new(Schema::parse_and_validate(partial_sdl, "./").unwrap());

        let req = crate::services::connect::Request::builder()
            .service_name(NodeStr::from("subgraph_Query_entity_0"))
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &schema,
                    r#"
                query($a: ID!, $b: ID!) {
                    a: entity(id: $a) { field { field } }
                    b: entity(id: $b) { field { alias: field } }
                }
            "#
                    .to_string(),
                    "./",
                )
                .unwrap(),
            ))
            .variables(Variables {
                variables: serde_json_bytes::json!({
                    "a": "1",
                    "b": "2"
                })
                .as_object()
                .unwrap()
                .clone(),
                inverted_paths: Default::default(),
                contextual_arguments: Default::default(),
            })
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(graphql::Request::builder().build())
                    .unwrap(),
            ))
            .build();

        assert_debug_snapshot!(super::entities_from_request(&req, &schema).unwrap(), @r###"
        [
            (
                RootField {
                    name: "a",
                    typename: Concrete(
                        "Entity",
                    ),
                    selection_set: "field {\n  field\n}",
                },
                RequestInputs {
                    args: {
                        "id": String(
                            "1",
                        ),
                    },
                    this: {},
                },
            ),
            (
                RootField {
                    name: "b",
                    typename: Concrete(
                        "Entity",
                    ),
                    selection_set: "field {\n  alias: field\n}",
                },
                RequestInputs {
                    args: {
                        "id": String(
                            "2",
                        ),
                    },
                    this: {},
                },
            ),
        ]
        "###);
    }

    #[test]
    fn entities_with_fields_from_request() {
        let partial_sdl = r#"
        type Query { _: String } # just to make it valid

        type Entity { # @key(fields: "id")
          id: ID!
          field(foo: String): T
        }

        type T {
          selected: String
        }
        "#;
        let schema = Arc::new(Schema::parse_and_validate(partial_sdl, "./").unwrap());
        let subgraph_schema = Arc::new(
            Schema::parse_and_validate(
                format!(
                    r#"{partial_sdl}
        extend type Query {{
          _entities(representations: [_Any!]!): _Entity
        }}
        scalar _Any
        union _Entity = Entity
        "#
                ),
                "./",
            )
            .unwrap(),
        );

        let req = crate::services::connect::Request::builder()
            .service_name(NodeStr::from("subgraph_Entity_field_0"))
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &subgraph_schema,
                    r#"
                query($representations: [_Any!]!, $bye: String) {
                    _entities(representations: $representations) {
                        __typename
                        ... on Entity {
                            field(foo: "hi") { selected }
                            alias: field(foo: $bye) { selected }
                        }
                    }
                }
            "#
                    .to_string(),
                    "./",
                )
                .unwrap(),
            ))
            .variables(Variables {
                variables: serde_json_bytes::json!({
                    "representations": [
                        { "__typename": "Entity", "id": "1" },
                        { "__typename": "Entity", "id": "2" },
                    ],
                    "bye": "bye"
                })
                .as_object()
                .unwrap()
                .clone(),
                inverted_paths: Default::default(),
                contextual_arguments: Default::default(),
            })
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(graphql::Request::builder().build())
                    .unwrap(),
            ))
            .build();

        assert_debug_snapshot!(super::entities_with_fields_from_request(&req, &schema).unwrap(), @r###"
        [
            (
                EntityField {
                    index: 0,
                    field_name: "field",
                    typename: Concrete(
                        "Entity",
                    ),
                    selection_set: "selected",
                },
                RequestInputs {
                    args: {
                        "foo": String(
                            "hi",
                        ),
                    },
                    this: {
                        "__typename": String(
                            "Entity",
                        ),
                        "id": String(
                            "1",
                        ),
                    },
                },
            ),
            (
                EntityField {
                    index: 1,
                    field_name: "field",
                    typename: Concrete(
                        "Entity",
                    ),
                    selection_set: "selected",
                },
                RequestInputs {
                    args: {
                        "foo": String(
                            "hi",
                        ),
                    },
                    this: {
                        "__typename": String(
                            "Entity",
                        ),
                        "id": String(
                            "2",
                        ),
                    },
                },
            ),
            (
                EntityField {
                    index: 0,
                    field_name: "alias",
                    typename: Concrete(
                        "Entity",
                    ),
                    selection_set: "selected",
                },
                RequestInputs {
                    args: {
                        "foo": String(
                            "bye",
                        ),
                    },
                    this: {
                        "__typename": String(
                            "Entity",
                        ),
                        "id": String(
                            "1",
                        ),
                    },
                },
            ),
            (
                EntityField {
                    index: 1,
                    field_name: "alias",
                    typename: Concrete(
                        "Entity",
                    ),
                    selection_set: "selected",
                },
                RequestInputs {
                    args: {
                        "foo": String(
                            "bye",
                        ),
                    },
                    this: {
                        "__typename": String(
                            "Entity",
                        ),
                        "id": String(
                            "2",
                        ),
                    },
                },
            ),
        ]
        "###);
    }

    #[test]
    fn entities_with_fields_from_request_interface_object() {
        let partial_sdl = r#"
        type Query { _: String } # just to make it valid

        type Entity { # @interfaceObject @key(fields: "id")
          id: ID!
          field(foo: String): T
        }

        type T {
          selected: String
        }
        "#;
        let schema = Arc::new(Schema::parse_and_validate(partial_sdl, "./").unwrap());
        let subgraph_schema = Arc::new(
            Schema::parse_and_validate(
                format!(
                    r#"{partial_sdl}
        extend type Query {{
          _entities(representations: [_Any!]!): _Entity
        }}
        scalar _Any
        union _Entity = Entity
        "#
                ),
                "./",
            )
            .unwrap(),
        );

        let req = crate::services::connect::Request::builder()
            .service_name(NodeStr::from("subgraph_Entity_field_0"))
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &subgraph_schema,
                    r#"
                query($representations: [_Any!]!, $foo: String) {
                    _entities(representations: $representations) {
                        ... on Entity {
                            field(foo: $foo) { selected }
                        }
                    }
                }
            "#
                    .to_string(),
                    "./",
                )
                .unwrap(),
            ))
            .variables(Variables {
                variables: serde_json_bytes::json!({
                  "representations": [
                      { "__typename": "Entity", "id": "1" },
                      { "__typename": "Entity", "id": "2" },
                  ],
                  "foo": "bar"
                })
                .as_object()
                .unwrap()
                .clone(),
                inverted_paths: Default::default(),
                contextual_arguments: Default::default(),
            })
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(graphql::Request::builder().build())
                    .unwrap(),
            ))
            .build();

        assert_debug_snapshot!(super::entities_with_fields_from_request(&req, &schema).unwrap(), @r###"
        [
            (
                EntityField {
                    index: 0,
                    field_name: "field",
                    typename: Omitted,
                    selection_set: "selected",
                },
                RequestInputs {
                    args: {
                        "foo": String(
                            "bar",
                        ),
                    },
                    this: {
                        "__typename": String(
                            "Entity",
                        ),
                        "id": String(
                            "1",
                        ),
                    },
                },
            ),
            (
                EntityField {
                    index: 1,
                    field_name: "field",
                    typename: Omitted,
                    selection_set: "selected",
                },
                RequestInputs {
                    args: {
                        "foo": String(
                            "bar",
                        ),
                    },
                    this: {
                        "__typename": String(
                            "Entity",
                        ),
                        "id": String(
                            "2",
                        ),
                    },
                },
            ),
        ]
        "###);
    }

    #[test]
    fn make_requests() {
        let schema = Schema::parse_and_validate("type Query { hello: String }", "./").unwrap();

        let req = crate::services::connect::Request::builder()
            .service_name(NodeStr::from("subgraph_Query_a_0"))
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &schema,
                    "query { a: hello }".to_string(),
                    "./",
                )
                .unwrap(),
            ))
            .variables(Variables {
                variables: Default::default(),
                inverted_paths: Default::default(),
                contextual_arguments: Default::default(),
            })
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(graphql::Request::builder().build())
                    .unwrap(),
            ))
            .build();

        let connector = Connector {
            id: ConnectId::new(
                "subgraph_name".into(),
                name!(Query),
                name!(users),
                0,
                "test label",
            ),
            transport: apollo_federation::sources::connect::Transport::HttpJson(
                HttpJsonTransport {
                    base_url: "http://localhost/api".into(),
                    path_template: URLPathTemplate::parse("/path").unwrap(),
                    method: HTTPMethod::Get,
                    headers: Default::default(),
                    body: Default::default(),
                },
            ),
            selection: JSONSelection::parse(".data").unwrap().1,
            entity: false,
            on_root_type: true,
        };

        let requests = super::make_requests(req, &connector, &schema).unwrap();

        assert_debug_snapshot!(requests, @r###"
        [
            (
                Request {
                    method: GET,
                    uri: http://localhost/api/path,
                    version: HTTP/1.1,
                    headers: {
                        "content-type": "application/json",
                    },
                    body: Body(
                        Empty,
                    ),
                },
                RootField {
                    name: "a",
                    typename: Concrete(
                        "String",
                    ),
                    selection_set: "",
                },
            ),
        ]
        "###);
    }
}

mod graphql_utils {
    use apollo_compiler::executable::Field;
    use apollo_compiler::executable::Operation;
    use apollo_compiler::executable::Selection;
    use apollo_compiler::schema::Value;
    use apollo_compiler::Node;
    use serde_json::Number;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Map;
    use serde_json_bytes::Value as JSONValue;
    use tower::BoxError;

    use super::MakeRequestError;
    use super::ENTITIES;

    pub(super) fn field_arguments_map(
        field: &Node<Field>,
        variables: &Map<ByteString, JSONValue>,
    ) -> Result<Map<ByteString, JSONValue>, BoxError> {
        let mut arguments = Map::new();
        for argument in field.arguments.iter() {
            match &*argument.value {
                apollo_compiler::schema::Value::Variable(name) => {
                    if let Some(value) = variables.get(name.as_str()) {
                        arguments.insert(argument.name.as_str(), value.clone());
                    }
                }
                _ => {
                    arguments.insert(
                        argument.name.as_str(),
                        argument_value_to_json(&argument.value)?,
                    );
                }
            }
        }
        Ok(arguments)
    }

    pub(super) fn argument_value_to_json(
        value: &apollo_compiler::ast::Value,
    ) -> Result<JSONValue, BoxError> {
        match value {
            Value::Null => Ok(JSONValue::Null),
            Value::Enum(e) => Ok(JSONValue::String(e.as_str().into())),
            Value::Variable(_) => Err(BoxError::from("variables not supported")),
            Value::String(s) => Ok(JSONValue::String(s.as_str().into())),
            Value::Float(f) => Ok(JSONValue::Number(
                Number::from_f64(
                    f.try_to_f64()
                        .map_err(|_| BoxError::from("try_to_f64 failed"))?,
                )
                .ok_or_else(|| BoxError::from("Number::from_f64 failed"))?,
            )),
            Value::Int(i) => Ok(JSONValue::Number(Number::from(
                i.try_to_i32().map_err(|_| "invalid int")?,
            ))),
            Value::Boolean(b) => Ok(JSONValue::Bool(*b)),
            Value::List(l) => Ok(JSONValue::Array(
                l.iter()
                    .map(|v| argument_value_to_json(v))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Value::Object(o) => Ok(JSONValue::Object(
                o.iter()
                    .map(|(k, v)| argument_value_to_json(v).map(|v| (k.as_str().into(), v)))
                    .collect::<Result<Map<_, _>, _>>()?,
            )),
        }
    }

    pub(super) fn get_entity_fields(
        op: &Node<Operation>,
    ) -> Result<(&Node<Field>, bool), MakeRequestError> {
        use MakeRequestError::*;

        let root_field = op
            .selection_set
            .selections
            .iter()
            .find_map(|s| match s {
                Selection::Field(f) if f.name == ENTITIES => Some(f),
                _ => None,
            })
            .ok_or_else(|| InvalidOperation("missing entities root field".into()))?;

        let mut typename_requested = false;

        for selection in root_field.selection_set.selections.iter() {
            match selection {
                Selection::Field(f) => {
                    if f.name == "__typename" {
                        typename_requested = true;
                    }
                }
                Selection::FragmentSpread(_) => {
                    return Err(UnsupportedOperation("fragment spread not supported".into()))
                }
                Selection::InlineFragment(f) => {
                    for selection in f.selection_set.selections.iter() {
                        match selection {
                            Selection::Field(f) => {
                                if f.name == "__typename" {
                                    typename_requested = true;
                                }
                            }
                            Selection::FragmentSpread(_) | Selection::InlineFragment(_) => {
                                return Err(UnsupportedOperation(
                                    "fragment spread not supported".into(),
                                ))
                            }
                        }
                    }
                }
            }
        }

        Ok((root_field, typename_requested))
    }
}
