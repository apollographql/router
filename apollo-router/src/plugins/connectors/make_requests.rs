use std::sync::Arc;

use apollo_compiler::executable::Selection;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::EntityResolver;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::http_json_transport::HttpJsonTransportError;
use apollo_federation::connectors::runtime::http_json_transport::make_request;
use apollo_federation::connectors::runtime::inputs::RequestInputs;
use apollo_federation::connectors::runtime::key::ResponseKey;
use parking_lot::Mutex;

use crate::Context;
use crate::services::connect;
use crate::services::connector::request_service::Request;

const REPRESENTATIONS_VAR: &str = "representations";
const ENTITIES: &str = "_entities";
const TYPENAME: &str = "__typename";

pub(crate) fn make_requests(
    request: connect::Request,
    context: &Context,
    connector: Arc<Connector>,
    service_name: &str,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<Vec<Request>, MakeRequestError> {
    let request_params = match connector.entity_resolver {
        Some(EntityResolver::Explicit) | Some(EntityResolver::TypeSingle) => {
            entities_from_request(connector.clone(), &request)
        }
        Some(EntityResolver::Implicit) => {
            entities_with_fields_from_request(connector.clone(), &request)
        }
        Some(EntityResolver::TypeBatch) => batch_entities_from_request(connector.clone(), &request),
        None => root_fields(connector.clone(), &request),
    }?;

    request_params_to_requests(
        context,
        connector,
        service_name,
        request_params,
        request,
        debug,
    )
}

fn request_params_to_requests(
    context: &Context,
    connector: Arc<Connector>,
    service_name: &str,
    request_params: Vec<ResponseKey>,
    original_request: connect::Request,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<Vec<Request>, MakeRequestError> {
    let mut results = vec![];
    for response_key in request_params {
        let connector = connector.clone();
        let (transport_request, mapping_problems) = make_request(
            &connector.transport,
            response_key
                .inputs()
                .clone()
                .merger(&connector.request_variable_keys)
                .config(connector.config.as_ref())
                .context(&original_request.context)
                .request(
                    &connector.request_headers,
                    original_request.supergraph_request.headers(),
                )
                .merge(),
            original_request.supergraph_request.headers(),
            debug,
        )?;

        results.push(Request {
            context: context.clone(),
            connector,
            service_name: service_name.to_string(),
            transport_request,
            key: response_key,
            mapping_problems,
            supergraph_request: original_request.supergraph_request.clone(),
        });
    }

    Ok(results)
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
/// using the schema. (This isn't true for _entities operations.)
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
    connector: Arc<Connector>,
    request: &connect::Request,
) -> Result<Vec<ResponseKey>, MakeRequestError> {
    use MakeRequestError::*;

    let op = request
        .operation
        .operations
        .get(None)
        .map_err(|_| InvalidOperation("no operation document".into()))?;

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

                let args = graphql_utils::field_arguments_map(field, &request.variables.variables)
                    .map_err(|err| {
                        InvalidArguments(format!("cannot get inputs from field arguments: {err}"))
                    })?;

                let request_inputs = RequestInputs {
                    args,
                    ..Default::default()
                };

                let response_key = ResponseKey::RootField {
                    name: response_name,
                    selection: Arc::new(connector.selection.apply_selection_set(
                        &request.operation,
                        &field.selection_set,
                        None,
                    )),
                    inputs: request_inputs,
                };

                Ok(response_key)
            }

            // The query planner removes fragments at the root so we don't have
            // to worry these branches
            Selection::FragmentSpread(_) | Selection::InlineFragment(_) => {
                Err(UnsupportedOperation(
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
    connector: Arc<Connector>,
    request: &connect::Request,
) -> Result<Vec<ResponseKey>, MakeRequestError> {
    use MakeRequestError::*;

    let Some(representations) = request.variables.variables.get(REPRESENTATIONS_VAR) else {
        return root_fields(connector, request);
    };

    let op = request
        .operation
        .operations
        .get(None)
        .map_err(|_| InvalidOperation("no operation document".into()))?;

    let (entities_field, _) = graphql_utils::get_entity_fields(&request.operation, op)?;

    let selection = Arc::new(connector.selection.apply_selection_set(
        &request.operation,
        &entities_field.selection_set,
        None,
    ));

    representations
        .as_array()
        .ok_or_else(|| InvalidRepresentations("representations is not an array".into()))?
        .iter()
        .enumerate()
        .map(|(i, rep)| {
            let request_inputs = match connector.entity_resolver {
                Some(EntityResolver::Explicit) => RequestInputs {
                    args: rep
                        .as_object()
                        .ok_or_else(|| {
                            InvalidRepresentations("representation is not an object".into())
                        })?
                        .clone(),
                    ..Default::default()
                },
                Some(EntityResolver::TypeSingle) => RequestInputs {
                    this: rep
                        .as_object()
                        .ok_or_else(|| {
                            InvalidRepresentations("representation is not an object".into())
                        })?
                        .clone(),
                    ..Default::default()
                },
                _ => {
                    return Err(InvalidRepresentations(
                        "entity resolver not supported for this connector".into(),
                    ));
                }
            };

            Ok(ResponseKey::Entity {
                index: i,
                selection: selection.clone(),
                inputs: request_inputs,
            })
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
    connector: Arc<Connector>,
    request: &connect::Request,
) -> Result<Vec<ResponseKey>, MakeRequestError> {
    use MakeRequestError::*;

    let op = request
        .operation
        .operations
        .get(None)
        .map_err(|_| InvalidOperation("no operation document".into()))?;

    let (entities_field, typename_requested) =
        graphql_utils::get_entity_fields(&request.operation, op)?;

    let types_and_fields = entities_field
        .selection_set
        .selections
        .iter()
        .map(|selection| match selection {
            Selection::Field(_) => Ok::<_, MakeRequestError>(vec![]),

            Selection::FragmentSpread(f) => {
                let Some(frag) = f.fragment_def(&request.operation) else {
                    return Err(InvalidOperation(format!(
                        "invalid operation: fragment `{}` missing",
                        f.fragment_name
                    )));
                };
                let typename = frag.type_condition();
                Ok(frag
                    .selection_set
                    .selections
                    .iter()
                    .filter_map(|sel| {
                        let field = match sel {
                            Selection::Field(f) => {
                                if f.name == TYPENAME {
                                    None
                                } else {
                                    Some(f)
                                }
                            }
                            Selection::FragmentSpread(_) | Selection::InlineFragment(_) => {
                                return Some(Err(InvalidOperation(
                                    "handling fragments inside entity selections not implemented"
                                        .into(),
                                )));
                            }
                        };
                        field.map(|f| Ok((typename, f)))
                    })
                    .collect::<Result<Vec<_>, _>>()?)
            }

            Selection::InlineFragment(frag) => {
                let typename = frag
                    .type_condition
                    .as_ref()
                    .ok_or_else(|| InvalidOperation("missing type condition".into()))?;
                Ok(frag
                    .selection_set
                    .selections
                    .iter()
                    .filter_map(|sel| {
                        let field = match sel {
                            Selection::Field(f) => {
                                if f.name == TYPENAME {
                                    None
                                } else {
                                    Some(f)
                                }
                            }
                            Selection::FragmentSpread(_) | Selection::InlineFragment(_) => {
                                return Some(Err(InvalidOperation(
                                    "handling fragments inside entity selections not implemented"
                                        .into(),
                                )));
                            }
                        };
                        field.map(|f| Ok((typename, f)))
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
            let selection = Arc::new(connector.selection.apply_selection_set(
                &request.operation,
                &field.selection_set,
                None,
            ));

            representations.iter().map(move |(i, representation)| {
                let args = graphql_utils::field_arguments_map(field, &request.variables.variables)
                    .map_err(|err| {
                        InvalidArguments(format!("cannot get inputs from field arguments: {err}"))
                    })?;

                let response_name = field
                    .alias
                    .as_ref()
                    .unwrap_or_else(|| &field.name)
                    .to_string();

                let request_inputs = RequestInputs {
                    args,
                    this: representation
                        .as_object()
                        .ok_or_else(|| {
                            InvalidRepresentations("representation is not an object".into())
                        })?
                        .clone(),
                    ..Default::default()
                };
                Ok::<_, MakeRequestError>(ResponseKey::EntityField {
                    index: *i,
                    field_name: response_name.to_string(),
                    // if the fetch node operation doesn't include __typename, then
                    // we're assuming this is for an interface object and we don't want
                    // to include a __typename in the response.
                    //
                    // TODO: is this fragile? should we just check the output
                    // type of the field and omit the typename if it's abstract?
                    typename: typename_requested.then_some(typename.clone()),
                    selection: selection.clone(),
                    inputs: request_inputs,
                })
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

// --- BATCH ENTITIES ----------------------------------------------------------------

/// Connectors on types can make a single batch request for multiple entities
/// using the `$batch` variable.
///
/// The key (pun intended) to batching is that we have to return entities in an
/// order than matches the `representations` variable. We use the "key" fields
/// to construct a HashMap key for each representation and response object,
/// which allows us to match them up and return them in the correct order.
fn batch_entities_from_request(
    connector: Arc<Connector>,
    request: &connect::Request,
) -> Result<Vec<ResponseKey>, MakeRequestError> {
    use MakeRequestError::*;

    let Some(keys) = &request.keys else {
        return Err(InvalidOperation("TODO better error type".into()));
    };

    let Some(representations) = request.variables.variables.get(REPRESENTATIONS_VAR) else {
        return Err(InvalidRepresentations(
            "batch_entities_from_request called without representations".into(),
        ));
    };

    let op = request
        .operation
        .operations
        .get(None)
        .map_err(|_| InvalidOperation("no operation document".into()))?;

    let (entities_field, _) = graphql_utils::get_entity_fields(&request.operation, op)?;

    let selection = Arc::new(connector.selection.apply_selection_set(
        &request.operation,
        &entities_field.selection_set,
        Some(keys),
    ));

    // First, let's grab all the representations into a single batch
    let batch = representations
        .as_array()
        .ok_or_else(|| InvalidRepresentations("representations is not an array".into()))?
        .iter()
        .map(|rep| {
            let obj = rep
                .as_object()
                .ok_or_else(|| InvalidRepresentations("representation is not an object".into()))?
                .clone();
            Ok::<_, MakeRequestError>(obj)
        })
        .collect::<Result<Vec<_>, _>>()?;

    // If we've got a max_size set, chunk the batch into smaller batches. Otherwise, we'll default to just a single batch.
    let max_size = connector.batch_settings.as_ref().and_then(|bs| bs.max_size);
    let batches = if let Some(size) = max_size {
        batch.chunks(size).map(|chunk| chunk.to_vec()).collect()
    } else {
        vec![batch]
    };

    // Finally, map the batches to BatchEntity. Each one of these final BatchEntity's ends up being a outgoing request
    let batch_entities = batches
        .iter()
        .map(|batch| {
            let inputs = RequestInputs {
                batch: batch.to_vec(),
                ..Default::default()
            };

            ResponseKey::BatchEntity {
                selection: selection.clone(),
                inputs,
                keys: keys.clone(),
            }
        })
        .collect();

    Ok(batch_entities)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_compiler::executable::FieldSet;
    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectBatchArguments;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
    use insta::assert_debug_snapshot;

    use crate::Context;
    use crate::graphql;
    use crate::query_planner::fetch::Variables;

    #[test]
    fn test_root_fields_simple() {
        let schema = Arc::new(
            Schema::parse_and_validate("type Query { a: A } type A { f: String }", "./").unwrap(),
        );

        let req = crate::services::connect::Request::builder()
            .service_name("subgraph_Query_a_0".into())
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(a),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("f").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::root_fields(Arc::new(connector), &req), @r#"
        Ok(
            [
                RootField {
                    name: "a",
                    selection: "f",
                    inputs: RequestInputs {
                        args: {},
                        this: {},
                        batch: []
                    },
                },
                RootField {
                    name: "a2",
                    selection: "f2: f",
                    inputs: RequestInputs {
                        args: {},
                        this: {},
                        batch: []
                    },
                },
            ],
        )
        "#);
    }

    #[test]
    fn test_root_fields_inputs() {
        let schema = Arc::new(
            Schema::parse_and_validate("type Query {b(var: String): String}", "./").unwrap(),
        );

        let req = crate::services::connect::Request::builder()
            .service_name("subgraph_Query_b_0".into())
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(b),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("$").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::root_fields(Arc::new(connector), &req), @r#"
        Ok(
            [
                RootField {
                    name: "b",
                    selection: "$",
                    inputs: RequestInputs {
                        args: {"var":"inline"},
                        this: {},
                        batch: []
                    },
                },
                RootField {
                    name: "b2",
                    selection: "$",
                    inputs: RequestInputs {
                        args: {"var":"variable"},
                        this: {},
                        batch: []
                    },
                },
            ],
        )
        "#);
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
        .service_name("subgraph_Query_c_0".into())
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(c),
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
        };

        assert_debug_snapshot!(super::root_fields(Arc::new(connector), &req), @r#"
        Ok(
            [
                RootField {
                    name: "c",
                    selection: "$.data",
                    inputs: RequestInputs {
                        args: {"var1":1,"var2":true,"var3":0.9,"var4":"123","var5":{"a":42},"var6":["item"],"var7":null},
                        this: {},
                        batch: []
                    },
                },
                RootField {
                    name: "c2",
                    selection: "$.data",
                    inputs: RequestInputs {
                        args: {"var1":1,"var2":true,"var3":0.9,"var4":"123","var5":{"a":42},"var6":["item"],"var7":null},
                        this: {},
                        batch: []
                    },
                },
            ],
        )
        "#);
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
            .service_name("subgraph_Query_entity_0".into())
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(entity),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("field").unwrap(),
            entity_resolver: Some(super::EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::entities_from_request(Arc::new(connector), &req).unwrap(), @r#"
        [
            Entity {
                index: 0,
                selection: "field\nalias: field",
                inputs: RequestInputs {
                    args: {"__typename":"Entity","id":"1"},
                    this: {},
                    batch: []
                },
            },
            Entity {
                index: 1,
                selection: "field\nalias: field",
                inputs: RequestInputs {
                    args: {"__typename":"Entity","id":"2"},
                    this: {},
                    batch: []
                },
            },
        ]
        "#);
    }

    #[test]
    fn entities_from_request_entity_with_fragment() {
        let partial_sdl = r#"
        type Query {
          entity(id: ID!): Entity
        }

        type Entity {
          field: String
        }
        "#;

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
            .service_name("subgraph_Query_entity_0".into())
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &subgraph_schema,
                    r#"
                query($representations: [_Any!]!) {
                    _entities(representations: $representations) {
                        ... _generated_Entity
                    }
                }
                fragment _generated_Entity on Entity {
                    __typename
                    field
                    alias: field
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(entity),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("field").unwrap(),
            entity_resolver: Some(super::EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::entities_from_request(Arc::new(connector), &req).unwrap(), @r#"
        [
            Entity {
                index: 0,
                selection: "field\nalias: field",
                inputs: RequestInputs {
                    args: {"__typename":"Entity","id":"1"},
                    this: {},
                    batch: []
                },
            },
            Entity {
                index: 1,
                selection: "field\nalias: field",
                inputs: RequestInputs {
                    args: {"__typename":"Entity","id":"2"},
                    this: {},
                    batch: []
                },
            },
        ]
        "#);
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
            .service_name("subgraph_Query_entity_0".into())
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(entity),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("field { field }").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::entities_from_request(Arc::new(connector), &req).unwrap(), @r#"
        [
            RootField {
                name: "a",
                selection: "field {\n  field\n}",
                inputs: RequestInputs {
                    args: {"id":"1"},
                    this: {},
                    batch: []
                },
            },
            RootField {
                name: "b",
                selection: "field {\n  alias: field\n}",
                inputs: RequestInputs {
                    args: {"id":"2"},
                    this: {},
                    batch: []
                },
            },
        ]
        "#);
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
            .service_name("subgraph_Entity_field_0".into())
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Entity),
                name!(field),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("selected").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::entities_with_fields_from_request(Arc::new(connector), &req).unwrap(), @r#"
        [
            EntityField {
                index: 0,
                field_name: "field",
                typename: Some(
                    "Entity",
                ),
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"hi"},
                    this: {"__typename":"Entity","id":"1"},
                    batch: []
                },
            },
            EntityField {
                index: 1,
                field_name: "field",
                typename: Some(
                    "Entity",
                ),
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"hi"},
                    this: {"__typename":"Entity","id":"2"},
                    batch: []
                },
            },
            EntityField {
                index: 0,
                field_name: "alias",
                typename: Some(
                    "Entity",
                ),
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"bye"},
                    this: {"__typename":"Entity","id":"1"},
                    batch: []
                },
            },
            EntityField {
                index: 1,
                field_name: "alias",
                typename: Some(
                    "Entity",
                ),
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"bye"},
                    this: {"__typename":"Entity","id":"2"},
                    batch: []
                },
            },
        ]
        "#);
    }

    #[test]
    fn entities_with_fields_from_request_with_fragment() {
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
            .service_name("subgraph_Entity_field_0".into())
            .context(Context::default())
            .operation(Arc::new(
                ExecutableDocument::parse_and_validate(
                    &subgraph_schema,
                    r#"
                query($representations: [_Any!]!, $bye: String) {
                    _entities(representations: $representations) {
                        ... _generated_Entity
                    }
                }
                fragment _generated_Entity on Entity {
                    __typename
                    field(foo: "hi") { selected }
                    alias: field(foo: $bye) { selected }
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Entity),
                name!(field),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("selected").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::entities_with_fields_from_request(Arc::new(connector), &req).unwrap(), @r#"
        [
            EntityField {
                index: 0,
                field_name: "field",
                typename: Some(
                    "Entity",
                ),
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"hi"},
                    this: {"__typename":"Entity","id":"1"},
                    batch: []
                },
            },
            EntityField {
                index: 1,
                field_name: "field",
                typename: Some(
                    "Entity",
                ),
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"hi"},
                    this: {"__typename":"Entity","id":"2"},
                    batch: []
                },
            },
            EntityField {
                index: 0,
                field_name: "alias",
                typename: Some(
                    "Entity",
                ),
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"bye"},
                    this: {"__typename":"Entity","id":"1"},
                    batch: []
                },
            },
            EntityField {
                index: 1,
                field_name: "alias",
                typename: Some(
                    "Entity",
                ),
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"bye"},
                    this: {"__typename":"Entity","id":"2"},
                    batch: []
                },
            },
        ]
        "#);
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
            .service_name("subgraph_Entity_field_0".into())
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

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Entity),
                name!(field),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("selected").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::entities_with_fields_from_request(Arc::new(connector), &req).unwrap(), @r#"
        [
            EntityField {
                index: 0,
                field_name: "field",
                typename: None,
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"bar"},
                    this: {"__typename":"Entity","id":"1"},
                    batch: []
                },
            },
            EntityField {
                index: 1,
                field_name: "field",
                typename: None,
                selection: "selected",
                inputs: RequestInputs {
                    args: {"foo":"bar"},
                    this: {"__typename":"Entity","id":"2"},
                    batch: []
                },
            },
        ]
        "#);
    }

    #[test]
    fn batch_entities_from_request() {
        let partial_sdl = r#"
        type Query {
          entity(id: ID!): Entity
        }

        type Entity {
          id: ID!
          field: String
        }
        "#;

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

        let keys = FieldSet::parse_and_validate(&subgraph_schema, name!(Entity), "id", "").unwrap();

        let req = crate::services::connect::Request::builder()
            .service_name("subgraph_Entity_0".into())
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
            .and_keys(Some(keys))
            .build();

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new_on_object(
                "subgraph_name".into(),
                None,
                name!(Entity),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("id field").unwrap(),
            entity_resolver: Some(super::EntityResolver::TypeBatch),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::batch_entities_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            BatchEntity {
                selection: "id\nfield\nalias: field",
                key: "id",
                inputs: RequestInputs {
                    args: {},
                    this: {},
                    batch: [{"__typename":"Entity","id":"1"},{"__typename":"Entity","id":"2"}]
                },
            },
        ]
        "###);
    }

    #[test]
    fn batch_entities_from_request_within_max_size() {
        let partial_sdl = r#"
        type Query {
          entity(id: ID!): Entity
        }

        type Entity {
          id: ID!
          field: String
        }
        "#;

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

        let keys = FieldSet::parse_and_validate(&subgraph_schema, name!(Entity), "id", "").unwrap();

        let req = crate::services::connect::Request::builder()
            .service_name("subgraph_Entity_0".into())
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
            .and_keys(Some(keys))
            .build();

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new_on_object(
                "subgraph_name".into(),
                None,
                name!(Entity),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("id field").unwrap(),
            entity_resolver: Some(super::EntityResolver::TypeBatch),
            config: Default::default(),
            max_requests: None,
            batch_settings: Some(ConnectBatchArguments { max_size: Some(10) }),
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::batch_entities_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            BatchEntity {
                selection: "id\nfield\nalias: field",
                key: "id",
                inputs: RequestInputs {
                    args: {},
                    this: {},
                    batch: [{"__typename":"Entity","id":"1"},{"__typename":"Entity","id":"2"}]
                },
            },
        ]
        "###);
    }

    #[test]
    fn batch_entities_from_request_above_max_size() {
        let partial_sdl = r#"
        type Query {
          entity(id: ID!): Entity
        }

        type Entity {
          id: ID!
          field: String
        }
        "#;

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

        let keys = FieldSet::parse_and_validate(&subgraph_schema, name!(Entity), "id", "").unwrap();

        let req = crate::services::connect::Request::builder()
            .service_name("subgraph_Entity_0".into())
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
                        { "__typename": "Entity", "id": "3" },
                        { "__typename": "Entity", "id": "4" },
                        { "__typename": "Entity", "id": "5" },
                        { "__typename": "Entity", "id": "6" },
                        { "__typename": "Entity", "id": "7" },
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
            .and_keys(Some(keys))
            .build();

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new_on_object(
                "subgraph_name".into(),
                None,
                name!(Entity),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("id field").unwrap(),
            entity_resolver: Some(super::EntityResolver::TypeBatch),
            config: Default::default(),
            max_requests: None,
            batch_settings: Some(ConnectBatchArguments { max_size: Some(5) }),
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::batch_entities_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            BatchEntity {
                selection: "id\nfield\nalias: field",
                key: "id",
                inputs: RequestInputs {
                    args: {},
                    this: {},
                    batch: [{"__typename":"Entity","id":"1"},{"__typename":"Entity","id":"2"},{"__typename":"Entity","id":"3"},{"__typename":"Entity","id":"4"},{"__typename":"Entity","id":"5"}]
                },
            },
            BatchEntity {
                selection: "id\nfield\nalias: field",
                key: "id",
                inputs: RequestInputs {
                    args: {},
                    this: {},
                    batch: [{"__typename":"Entity","id":"6"},{"__typename":"Entity","id":"7"}]
                },
            },
        ]
        "###);
    }

    #[test]
    fn entities_from_request_on_type() {
        let partial_sdl = r#"
        type Query {
          entity(id: ID!): Entity
        }

        type Entity {
          id: ID!
          field: String
        }
        "#;

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

        let keys = FieldSet::parse_and_validate(&subgraph_schema, name!(Entity), "id", "").unwrap();

        let req = crate::services::connect::Request::builder()
            .service_name("subgraph_Entity_0".into())
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
            .and_keys(Some(keys))
            .build();

        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new_on_object(
                "subgraph_name".into(),
                None,
                name!(Entity),
                0,
                "test label",
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path?id={$this.id}".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("id field").unwrap(),
            entity_resolver: Some(super::EntityResolver::TypeSingle),
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
        };

        assert_debug_snapshot!(super::entities_from_request(Arc::new(connector), &req).unwrap(), @r#"
        [
            Entity {
                index: 0,
                selection: "field\nalias: field",
                inputs: RequestInputs {
                    args: {},
                    this: {"__typename":"Entity","id":"1"},
                    batch: []
                },
            },
            Entity {
                index: 1,
                selection: "field\nalias: field",
                inputs: RequestInputs {
                    args: {},
                    this: {"__typename":"Entity","id":"2"},
                    batch: []
                },
            },
        ]
        "#);
    }

    #[test]
    fn make_requests() {
        let schema = Schema::parse_and_validate("type Query { hello: String }", "./").unwrap();
        let service_name = String::from("subgraph_Query_a_0");
        let req = crate::services::connect::Request::builder()
            .service_name(service_name.clone().into())
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
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(users),
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
        };

        let requests: Vec<_> = super::make_requests(
            req,
            &Context::default(),
            Arc::new(connector),
            &service_name,
            &None,
        )
        .unwrap()
        .into_iter()
        .map(|req| {
            let TransportRequest::Http(http_request) = req.transport_request;
            let (parts, _body) = http_request.inner.into_parts();
            let new_req =
                http::Request::from_parts(parts, http_body_util::Empty::<bytes::Bytes>::new());
            (new_req, req.key, http_request.debug)
        })
        .collect();

        assert_debug_snapshot!(requests, @r#"
        [
            (
                Request {
                    method: GET,
                    uri: http://localhost/api/path,
                    version: HTTP/1.1,
                    headers: {},
                    body: Empty,
                },
                RootField {
                    name: "a",
                    selection: "$.data",
                    inputs: RequestInputs {
                        args: {},
                        this: {},
                        batch: []
                    },
                },
                (
                    None,
                    [],
                ),
            ),
        ]
        "#);
    }
}

mod graphql_utils;
