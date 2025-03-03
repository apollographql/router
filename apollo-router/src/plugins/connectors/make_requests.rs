use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::Selection;
use apollo_federation::sources::connect::Connector;
use apollo_federation::sources::connect::CustomConfiguration;
use apollo_federation::sources::connect::EntityResolver;
use apollo_federation::sources::connect::JSONSelection;
use apollo_federation::sources::connect::Namespace;
use parking_lot::Mutex;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use serde_json_bytes::json;

use super::http_json_transport::HttpJsonTransportError;
use super::http_json_transport::make_request;
use crate::Context;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::plugins::connectors::plugin::debug::ConnectorContext;
use crate::services::connect;
use crate::services::connector::request_service::Request;

const REPRESENTATIONS_VAR: &str = "representations";
const ENTITIES: &str = "_entities";
const TYPENAME: &str = "__typename";

#[derive(Clone, Debug, Default)]
pub(crate) struct RequestInputs {
    args: Map<ByteString, Value>,
    this: Map<ByteString, Value>,
}

impl RequestInputs {
    /// Creates a map for use in JSONSelection::apply_with_vars. It only clones
    /// values into the map if the variable namespaces (`$args`, `$this`, etc.)
    /// are actually referenced in the expressions for URLs, headers, body, or selection.
    pub(crate) fn merge(
        &self,
        variables_used: &HashSet<Namespace>,
        config: Option<&CustomConfiguration>,
        context: &Context,
        status: Option<u16>,
    ) -> IndexMap<String, Value> {
        let mut map = IndexMap::with_capacity_and_hasher(variables_used.len(), Default::default());

        // Not all connectors reference $args
        if variables_used.contains(&Namespace::Args) {
            map.insert(
                Namespace::Args.as_str().into(),
                Value::Object(self.args.clone()),
            );
        }

        // $this only applies to fields on entity types (not Query or Mutation)
        if variables_used.contains(&Namespace::This) {
            map.insert(
                Namespace::This.as_str().into(),
                Value::Object(self.this.clone()),
            );
        }

        // $context could be a large object, so we only convert it to JSON
        // if it's used. It can also be mutated between requests, so we have
        // to convert it each time.
        if variables_used.contains(&Namespace::Context) {
            let context: Map<ByteString, Value> = context
                .iter()
                .map(|r| (r.key().as_str().into(), r.value().clone()))
                .collect();
            map.insert(Namespace::Context.as_str().into(), Value::Object(context));
        }

        // $config doesn't change unless the schema reloads, but we can avoid
        // the allocation if it's unused.
        if variables_used.contains(&Namespace::Config) {
            if let Some(config) = config {
                map.insert(Namespace::Config.as_str().into(), json!(config));
            }
        }

        // $status is available only for response mapping
        if variables_used.contains(&Namespace::Status) {
            if let Some(status) = status {
                map.insert(
                    Namespace::Status.as_str().into(),
                    Value::Number(status.into()),
                );
            }
        }

        map
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ResponseKey {
    RootField {
        name: String,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
    Entity {
        index: usize,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
    EntityField {
        index: usize,
        field_name: String,
        /// Is Some only if the output type is a concrete object type. If it's
        /// an interface, it's treated as an interface object and we can't emit
        /// a __typename in the response.
        typename: Option<Name>,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
}

impl ResponseKey {
    pub(crate) fn selection(&self) -> &JSONSelection {
        match self {
            ResponseKey::RootField { selection, .. } => selection,
            ResponseKey::Entity { selection, .. } => selection,
            ResponseKey::EntityField { selection, .. } => selection,
        }
    }

    pub(crate) fn inputs(&self) -> &RequestInputs {
        match self {
            ResponseKey::RootField { inputs, .. } => inputs,
            ResponseKey::Entity { inputs, .. } => inputs,
            ResponseKey::EntityField { inputs, .. } => inputs,
        }
    }
}

/// Convert a ResponseKey into a Path for use in GraphQL errors. This mimics
/// the behavior of a GraphQL subgraph, including the `_entities` field. When
/// the path gets to [`FetchNode::response_at_path`], it will be amended and
/// appended to a parent path to create the full path to the field. For ex:
///
/// - parent path: `["posts", @, "user"]
/// - path from key: `["_entities", 0, "user", "profile"]`
/// - result: `["posts", 1, "user", "profile"]`
impl From<&ResponseKey> for Path {
    fn from(key: &ResponseKey) -> Self {
        match key {
            ResponseKey::RootField { name, .. } => {
                Path::from_iter(vec![PathElement::Key(name.to_string(), None)])
            }
            ResponseKey::Entity { index, .. } => Path::from_iter(vec![
                PathElement::Key("_entities".to_string(), None),
                PathElement::Index(*index),
            ]),
            ResponseKey::EntityField {
                index, field_name, ..
            } => Path::from_iter(vec![
                PathElement::Key("_entities".to_string(), None),
                PathElement::Index(*index),
                PathElement::Key(field_name.clone(), None),
            ]),
        }
    }
}

pub(crate) fn make_requests(
    request: connect::Request,
    context: &Context,
    connector: Arc<Connector>,
    service_name: &str,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<Vec<Request>, MakeRequestError> {
    let request_params = match connector.entity_resolver {
        Some(EntityResolver::Explicit) => entities_from_request(connector.clone(), &request),
        Some(EntityResolver::Implicit) => {
            entities_with_fields_from_request(connector.clone(), &request)
        }
        None => root_fields(connector.clone(), &request),
    }?;

    request_params_to_requests(
        context,
        connector,
        service_name,
        request_params,
        &request,
        debug,
    )
}

fn request_params_to_requests(
    context: &Context,
    connector: Arc<Connector>,
    service_name: &str,
    request_params: Vec<ResponseKey>,
    original_request: &connect::Request,
    debug: &Option<Arc<Mutex<ConnectorContext>>>,
) -> Result<Vec<Request>, MakeRequestError> {
    let mut results = vec![];
    for response_key in request_params {
        let connector = connector.clone();
        let (transport_request, mapping_problems) = make_request(
            &connector.transport,
            response_key.inputs().merge(
                &connector.request_variables,
                connector.config.as_ref(),
                &original_request.context,
                None,
            ),
            original_request,
            debug,
        )?;

        results.push(Request {
            context: context.clone(),
            connector,
            service_name: service_name.to_string(),
            transport_request,
            key: response_key,
            mapping_problems,
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
                    .map_err(|_| {
                        InvalidArguments("cannot get inputs from field arguments".into())
                    })?;

                let request_inputs = RequestInputs {
                    args,
                    this: Default::default(),
                };

                let response_key = ResponseKey::RootField {
                    name: response_name,
                    selection: Arc::new(
                        connector
                            .selection
                            .apply_selection_set(&request.operation, &field.selection_set),
                    ),
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

    let selection = Arc::new(
        connector
            .selection
            .apply_selection_set(&request.operation, &entities_field.selection_set),
    );

    representations
        .as_array()
        .ok_or_else(|| InvalidRepresentations("representations is not an array".into()))?
        .iter()
        .enumerate()
        .map(|(i, rep)| {
            let request_inputs = RequestInputs {
                args: rep
                    .as_object()
                    .ok_or_else(|| {
                        InvalidRepresentations("representation is not an object".into())
                    })?
                    .clone(),
                // entity connectors are always on Query fields, so they cannot use
                // sibling fields with $this
                this: Default::default(),
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
            let selection = Arc::new(
                connector
                    .selection
                    .apply_selection_set(&request.operation, &field.selection_set),
            );

            representations.iter().map(move |(i, representation)| {
                let args = graphql_utils::field_arguments_map(field, &request.variables.variables)
                    .map_err(|_| {
                        InvalidArguments("cannot build inputs from field arguments".into())
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_compiler::name;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::ConnectSpec;
    use apollo_federation::sources::connect::Connector;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HttpJsonTransport;
    use apollo_federation::sources::connect::JSONSelection;
    use insta::assert_debug_snapshot;
    use url::Url;

    use crate::Context;
    use crate::graphql;
    use crate::query_planner::fetch::Variables;
    use crate::services::connector::request_service::TransportRequest;

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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("f").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
        };

        assert_debug_snapshot!(super::root_fields(Arc::new(connector), &req), @r###"
        Ok(
            [
                RootField {
                    name: "a",
                    selection: Named(
                        SubSelection {
                            selections: [
                                Field(
                                    None,
                                    WithRange {
                                        node: Field(
                                            "f",
                                        ),
                                        range: Some(
                                            0..1,
                                        ),
                                    },
                                    None,
                                ),
                            ],
                            range: Some(
                                0..1,
                            ),
                        },
                    ),
                    inputs: RequestInputs {
                        args: {},
                        this: {},
                    },
                },
                RootField {
                    name: "a2",
                    selection: Named(
                        SubSelection {
                            selections: [
                                Field(
                                    Some(
                                        Alias {
                                            name: WithRange {
                                                node: Field(
                                                    "f2",
                                                ),
                                                range: None,
                                            },
                                            range: None,
                                        },
                                    ),
                                    WithRange {
                                        node: Field(
                                            "f",
                                        ),
                                        range: Some(
                                            0..1,
                                        ),
                                    },
                                    None,
                                ),
                            ],
                            range: Some(
                                0..1,
                            ),
                        },
                    ),
                    inputs: RequestInputs {
                        args: {},
                        this: {},
                    },
                },
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("$").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
        };

        assert_debug_snapshot!(super::root_fields(Arc::new(connector), &req), @r###"
        Ok(
            [
                RootField {
                    name: "b",
                    selection: Path(
                        PathSelection {
                            path: WithRange {
                                node: Var(
                                    WithRange {
                                        node: $,
                                        range: Some(
                                            0..1,
                                        ),
                                    },
                                    WithRange {
                                        node: Empty,
                                        range: Some(
                                            1..1,
                                        ),
                                    },
                                ),
                                range: Some(
                                    0..1,
                                ),
                            },
                        },
                    ),
                    inputs: RequestInputs {
                        args: {
                            "var": String(
                                "inline",
                            ),
                        },
                        this: {},
                    },
                },
                RootField {
                    name: "b2",
                    selection: Path(
                        PathSelection {
                            path: WithRange {
                                node: Var(
                                    WithRange {
                                        node: $,
                                        range: Some(
                                            0..1,
                                        ),
                                    },
                                    WithRange {
                                        node: Empty,
                                        range: Some(
                                            1..1,
                                        ),
                                    },
                                ),
                                range: Some(
                                    0..1,
                                ),
                            },
                        },
                    ),
                    inputs: RequestInputs {
                        args: {
                            "var": String(
                                "variable",
                            ),
                        },
                        this: {},
                    },
                },
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
        };

        assert_debug_snapshot!(super::root_fields(Arc::new(connector), &req), @r###"
        Ok(
            [
                RootField {
                    name: "c",
                    selection: Path(
                        PathSelection {
                            path: WithRange {
                                node: Var(
                                    WithRange {
                                        node: $,
                                        range: Some(
                                            0..1,
                                        ),
                                    },
                                    WithRange {
                                        node: Key(
                                            WithRange {
                                                node: Field(
                                                    "data",
                                                ),
                                                range: Some(
                                                    2..6,
                                                ),
                                            },
                                            WithRange {
                                                node: Empty,
                                                range: Some(
                                                    6..6,
                                                ),
                                            },
                                        ),
                                        range: Some(
                                            1..6,
                                        ),
                                    },
                                ),
                                range: Some(
                                    0..6,
                                ),
                            },
                        },
                    ),
                    inputs: RequestInputs {
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
                },
                RootField {
                    name: "c2",
                    selection: Path(
                        PathSelection {
                            path: WithRange {
                                node: Var(
                                    WithRange {
                                        node: $,
                                        range: Some(
                                            0..1,
                                        ),
                                    },
                                    WithRange {
                                        node: Key(
                                            WithRange {
                                                node: Field(
                                                    "data",
                                                ),
                                                range: Some(
                                                    2..6,
                                                ),
                                            },
                                            WithRange {
                                                node: Empty,
                                                range: Some(
                                                    6..6,
                                                ),
                                            },
                                        ),
                                        range: Some(
                                            1..6,
                                        ),
                                    },
                                ),
                                range: Some(
                                    0..6,
                                ),
                            },
                        },
                    ),
                    inputs: RequestInputs {
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
                },
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("field").unwrap(),
            entity_resolver: Some(super::EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
        };

        assert_debug_snapshot!(super::entities_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            Entity {
                index: 0,
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                None,
                            ),
                            Field(
                                Some(
                                    Alias {
                                        name: WithRange {
                                            node: Field(
                                                "alias",
                                            ),
                                            range: None,
                                        },
                                        range: None,
                                    },
                                ),
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..5,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            Entity {
                index: 1,
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                None,
                            ),
                            Field(
                                Some(
                                    Alias {
                                        name: WithRange {
                                            node: Field(
                                                "alias",
                                            ),
                                            range: None,
                                        },
                                        range: None,
                                    },
                                ),
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..5,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
        ]
        "###);
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("field").unwrap(),
            entity_resolver: Some(super::EntityResolver::Explicit),
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
        };

        assert_debug_snapshot!(super::entities_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            Entity {
                index: 0,
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                None,
                            ),
                            Field(
                                Some(
                                    Alias {
                                        name: WithRange {
                                            node: Field(
                                                "alias",
                                            ),
                                            range: None,
                                        },
                                        range: None,
                                    },
                                ),
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..5,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            Entity {
                index: 1,
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                None,
                            ),
                            Field(
                                Some(
                                    Alias {
                                        name: WithRange {
                                            node: Field(
                                                "alias",
                                            ),
                                            range: None,
                                        },
                                        range: None,
                                    },
                                ),
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..5,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("field { field }").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
        };

        assert_debug_snapshot!(super::entities_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            RootField {
                name: "a",
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                Some(
                                    SubSelection {
                                        selections: [
                                            Field(
                                                None,
                                                WithRange {
                                                    node: Field(
                                                        "field",
                                                    ),
                                                    range: Some(
                                                        8..13,
                                                    ),
                                                },
                                                None,
                                            ),
                                        ],
                                        range: Some(
                                            6..15,
                                        ),
                                    },
                                ),
                            ),
                        ],
                        range: Some(
                            0..15,
                        ),
                    },
                ),
                inputs: RequestInputs {
                    args: {
                        "id": String(
                            "1",
                        ),
                    },
                    this: {},
                },
            },
            RootField {
                name: "b",
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "field",
                                    ),
                                    range: Some(
                                        0..5,
                                    ),
                                },
                                Some(
                                    SubSelection {
                                        selections: [
                                            Field(
                                                Some(
                                                    Alias {
                                                        name: WithRange {
                                                            node: Field(
                                                                "alias",
                                                            ),
                                                            range: None,
                                                        },
                                                        range: None,
                                                    },
                                                ),
                                                WithRange {
                                                    node: Field(
                                                        "field",
                                                    ),
                                                    range: Some(
                                                        8..13,
                                                    ),
                                                },
                                                None,
                                            ),
                                        ],
                                        range: Some(
                                            6..15,
                                        ),
                                    },
                                ),
                            ),
                        ],
                        range: Some(
                            0..15,
                        ),
                    },
                ),
                inputs: RequestInputs {
                    args: {
                        "id": String(
                            "2",
                        ),
                    },
                    this: {},
                },
            },
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("selected").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
        };

        assert_debug_snapshot!(super::entities_with_fields_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            EntityField {
                index: 0,
                field_name: "field",
                typename: Some(
                    "Entity",
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            EntityField {
                index: 1,
                field_name: "field",
                typename: Some(
                    "Entity",
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            EntityField {
                index: 0,
                field_name: "alias",
                typename: Some(
                    "Entity",
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            EntityField {
                index: 1,
                field_name: "alias",
                typename: Some(
                    "Entity",
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
        ]
        "###);
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("selected").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
        };

        assert_debug_snapshot!(super::entities_with_fields_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            EntityField {
                index: 0,
                field_name: "field",
                typename: Some(
                    "Entity",
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            EntityField {
                index: 1,
                field_name: "field",
                typename: Some(
                    "Entity",
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            EntityField {
                index: 0,
                field_name: "alias",
                typename: Some(
                    "Entity",
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            EntityField {
                index: 1,
                field_name: "alias",
                typename: Some(
                    "Entity",
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
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
                source_url: Some(Url::parse("http://localhost/api").unwrap()),
                connect_template: "/path".parse().unwrap(),
                method: HTTPMethod::Get,
                headers: Default::default(),
                body: Default::default(),
            },
            selection: JSONSelection::parse("selected").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            request_variables: Default::default(),
            response_variables: Default::default(),
        };

        assert_debug_snapshot!(super::entities_with_fields_from_request(Arc::new(connector), &req).unwrap(), @r###"
        [
            EntityField {
                index: 0,
                field_name: "field",
                typename: None,
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
            EntityField {
                index: 1,
                field_name: "field",
                typename: None,
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "selected",
                                    ),
                                    range: Some(
                                        0..8,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..8,
                        ),
                    },
                ),
                inputs: RequestInputs {
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
            },
        ]
        "###);
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

        assert_debug_snapshot!(requests, @r###"
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
                    selection: Path(
                        PathSelection {
                            path: WithRange {
                                node: Var(
                                    WithRange {
                                        node: $,
                                        range: Some(
                                            0..1,
                                        ),
                                    },
                                    WithRange {
                                        node: Key(
                                            WithRange {
                                                node: Field(
                                                    "data",
                                                ),
                                                range: Some(
                                                    2..6,
                                                ),
                                            },
                                            WithRange {
                                                node: Empty,
                                                range: Some(
                                                    6..6,
                                                ),
                                            },
                                        ),
                                        range: Some(
                                            1..6,
                                        ),
                                    },
                                ),
                                range: Some(
                                    0..6,
                                ),
                            },
                        },
                    ),
                    inputs: RequestInputs {
                        args: {},
                        this: {},
                    },
                },
                None,
            ),
        ]
        "###);
    }
}

mod graphql_utils;
