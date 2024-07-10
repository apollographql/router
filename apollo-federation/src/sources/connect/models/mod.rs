mod config;

use std::sync::Arc;

pub use config::CustomConfiguration;
pub use config::SourceConfiguration;
pub use config::SubgraphConnectorConfiguration;
use indexmap::IndexMap;

use super::spec::ConnectHTTPArguments;
use super::spec::HTTPHeaderOption;
use super::spec::SourceHTTPArguments;
use super::ConnectId;
use super::JSONSelection;
use super::URLPathTemplate;
use crate::error::FederationError;
use crate::schema::ValidFederationSchema;
use crate::sources::connect::spec::extract_connect_directive_arguments;
use crate::sources::connect::spec::extract_source_directive_arguments;
use crate::sources::connect::ConnectSpecDefinition;
// --- Connector ---------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Connector {
    pub id: ConnectId,
    pub transport: Transport,
    pub selection: JSONSelection,
    pub config: Arc<CustomConfiguration>,

    /// The type of entity resolver to use for this connector
    pub entity_resolver: Option<EntityResolver>,
}

#[derive(Debug, Clone)]
pub enum Transport {
    HttpJson(HttpJsonTransport),
}

impl Transport {
    fn label(&self) -> String {
        match self {
            Transport::HttpJson(http) => http.label(),
        }
    }
}

/// Entity resolver type
///
/// A connector can be used as a potential entity resolver for a type, with
/// extra validation rules based on the transport args and field position within
/// a schema.
#[derive(Debug, Clone, PartialEq)]
pub enum EntityResolver {
    /// The user defined a connector on a field that acts as an entity resolver
    Explicit,

    /// The user defined a connector on a field of a type, so we need an entity resolver for that type
    Implicit,
}

impl Connector {
    pub(crate) fn from_valid_schema(
        schema: &ValidFederationSchema,
        subgraph_name: &str,
        config: Option<SubgraphConnectorConfiguration>,
    ) -> Result<IndexMap<ConnectId, Self>, FederationError> {
        let Some(metadata) = schema.metadata() else {
            return Ok(IndexMap::new());
        };
        let config = config.map(|c| Arc::new(c.custom)).unwrap_or_default();

        let Some(link) = metadata.for_identity(&ConnectSpecDefinition::identity()) else {
            return Ok(IndexMap::new());
        };

        let source_name = ConnectSpecDefinition::source_directive_name(&link);
        let source_arguments = extract_source_directive_arguments(schema, &source_name)?;

        let connect_name = ConnectSpecDefinition::connect_directive_name(&link);
        let connect_arguments = extract_connect_directive_arguments(schema, &connect_name)?;

        connect_arguments
            .into_iter()
            .map(move |args| {
                let source = if let Some(source_name) = args.source {
                    source_arguments
                        .iter()
                        .find(|source| source.name == source_name)
                } else {
                    None
                };

                let source_name = source.map(|s| s.name.clone());
                let connect_http = args.http.expect("@connect http missing");
                let source_http = source.map(|s| &s.http);

                let transport = Transport::HttpJson(HttpJsonTransport::from_directive(
                    &connect_http,
                    source_http,
                )?);

                let parent_type_name = args.position.field.type_name().clone();
                let schema_def = &schema.schema().schema_definition;
                let on_query = schema_def
                    .query
                    .as_ref()
                    .map(|ty| ty.name == parent_type_name)
                    .unwrap_or(false);
                let on_mutation = schema_def
                    .mutation
                    .as_ref()
                    .map(|ty| ty.name == parent_type_name)
                    .unwrap_or(false);
                let on_root_type = on_query || on_mutation;

                let id = ConnectId {
                    label: make_label(subgraph_name, &source_name, &transport),
                    subgraph_name: subgraph_name.to_string(),
                    source_name: source_name.clone(),
                    directive: args.position,
                };

                let entity_resolver = match (args.entity, on_root_type) {
                    (true, _) => Some(EntityResolver::Explicit),
                    (_, false) => Some(EntityResolver::Implicit),

                    _ => None,
                };

                let connector = Connector {
                    id: id.clone(),
                    transport,
                    selection: args.selection,
                    entity_resolver,
                    config: config.clone(),
                };

                Ok((id, connector))
            })
            .collect()
    }
}

fn make_label(subgraph_name: &str, source: &Option<String>, transport: &Transport) -> String {
    let source = format!(".{}", source.as_deref().unwrap_or(""));
    format!("{}{} {}", subgraph_name, source, transport.label())
}

// --- HTTP JSON ---------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HttpJsonTransport {
    pub base_url: String,
    pub path_template: URLPathTemplate,
    pub method: HTTPMethod,
    pub headers: Vec<HTTPHeader>,
    pub body: Option<JSONSelection>,
}

impl HttpJsonTransport {
    fn from_directive(
        http: &ConnectHTTPArguments,
        source: Option<&SourceHTTPArguments>,
    ) -> Result<Self, FederationError> {
        let (method, path) = if let Some(path) = &http.get {
            (HTTPMethod::Get, path)
        } else if let Some(path) = &http.post {
            (HTTPMethod::Post, path)
        } else if let Some(path) = &http.patch {
            (HTTPMethod::Patch, path)
        } else if let Some(path) = &http.put {
            (HTTPMethod::Put, path)
        } else if let Some(path) = &http.delete {
            (HTTPMethod::Delete, path)
        } else {
            return Err(FederationError::internal("missing http method"));
        };

        let mut headers = source
            .as_ref()
            .map(|source| source.headers.0.clone())
            .unwrap_or_default();
        headers.extend(http.headers.0.clone());

        Ok(Self {
            // TODO: We'll need to eventually support @connect directives without
            // a corresponding @source...
            // See: https://apollographql.atlassian.net/browse/CNN-201
            base_url: source
                .map(|s| s.base_url.clone())
                .ok_or(FederationError::internal(
                    "@connect must have a source with a base URL",
                ))?,
            path_template: URLPathTemplate::parse(path).map_err(|e| {
                FederationError::internal(format!("could not parse URL template: {e}"))
            })?,
            method,
            headers: http_headers(headers),
            body: http.body.clone(),
        })
    }

    fn label(&self) -> String {
        format!("http: {} {}", self.method.as_str(), self.path_template)
    }
}

/// The HTTP arguments needed for a connect request
#[derive(Debug, Clone, strum_macros::Display)]
pub enum HTTPMethod {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl HTTPMethod {
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            HTTPMethod::Get => "GET",
            HTTPMethod::Post => "POST",
            HTTPMethod::Patch => "PATCH",
            HTTPMethod::Put => "PUT",
            HTTPMethod::Delete => "DELETE",
        }
    }
}

#[derive(Clone, Debug)]
pub enum HTTPHeader {
    Propagate {
        name: String,
    },
    Rename {
        original_name: String,
        new_name: String,
    },
    Inject {
        name: String,
        value: String,
    },
}

fn http_headers(mappings: IndexMap<String, Option<HTTPHeaderOption>>) -> Vec<HTTPHeader> {
    let mut headers = vec![];
    for (name, value) in mappings {
        match value {
            Some(HTTPHeaderOption::As(new_name)) => headers.push(HTTPHeader::Rename {
                original_name: name.clone(),
                new_name,
            }),
            Some(HTTPHeaderOption::Value(values)) => {
                for value in values {
                    headers.push(HTTPHeader::Inject {
                        name: name.clone(),
                        value: value.clone(),
                    });
                }
            }
            None => headers.push(HTTPHeader::Propagate { name: name.clone() }),
        };
    }
    headers
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph;
    use crate::schema::FederationSchema;
    use crate::ValidFederationSubgraphs;

    static SIMPLE_SUPERGRAPH: &str = include_str!("../tests/schemas/simple.graphql");

    fn get_subgraphs(supergraph_sdl: &str) -> ValidFederationSubgraphs {
        let schema = Schema::parse(supergraph_sdl, "supergraph.graphql").unwrap();
        let supergraph_schema = FederationSchema::new(schema).unwrap();
        extract_subgraphs_from_supergraph(&supergraph_schema, Some(true)).unwrap()
    }

    #[test]
    fn test_from_schema() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();
        let connectors =
            Connector::from_valid_schema(&subgraph.schema, "connectors", None).unwrap();
        assert_debug_snapshot!(&connectors, @r###"
        {
            ConnectId {
                label: "connectors.json http: GET /users",
                subgraph_name: "connectors",
                source_name: Some(
                    "json",
                ),
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(Query.users),
                    directive_name: "connect",
                    directive_index: 0,
                },
            }: Connector {
                id: ConnectId {
                    label: "connectors.json http: GET /users",
                    subgraph_name: "connectors",
                    source_name: Some(
                        "json",
                    ),
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(Query.users),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
                transport: HttpJson(
                    HttpJsonTransport {
                        base_url: "https://jsonplaceholder.typicode.com/",
                        path_template: URLPathTemplate {
                            path: [
                                ParameterValue {
                                    parts: [
                                        Text(
                                            "users",
                                        ),
                                    ],
                                },
                            ],
                            query: {},
                        },
                        method: Get,
                        headers: [
                            Rename {
                                original_name: "X-Auth-Token",
                                new_name: "AuthToken",
                            },
                            Inject {
                                name: "user-agent",
                                value: "Firefox",
                            },
                            Propagate {
                                name: "X-From-Env",
                            },
                        ],
                        body: None,
                    },
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                "id",
                                None,
                            ),
                            Field(
                                None,
                                "name",
                                None,
                            ),
                        ],
                        star: None,
                    },
                ),
                entity_resolver: None,
            },
            ConnectId {
                label: "connectors.json http: GET /posts",
                subgraph_name: "connectors",
                source_name: Some(
                    "json",
                ),
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(Query.posts),
                    directive_name: "connect",
                    directive_index: 0,
                },
            }: Connector {
                id: ConnectId {
                    label: "connectors.json http: GET /posts",
                    subgraph_name: "connectors",
                    source_name: Some(
                        "json",
                    ),
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(Query.posts),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
                transport: HttpJson(
                    HttpJsonTransport {
                        base_url: "https://jsonplaceholder.typicode.com/",
                        path_template: URLPathTemplate {
                            path: [
                                ParameterValue {
                                    parts: [
                                        Text(
                                            "posts",
                                        ),
                                    ],
                                },
                            ],
                            query: {},
                        },
                        method: Get,
                        headers: [
                            Rename {
                                original_name: "X-Auth-Token",
                                new_name: "AuthToken",
                            },
                            Inject {
                                name: "user-agent",
                                value: "Firefox",
                            },
                            Propagate {
                                name: "X-From-Env",
                            },
                        ],
                        body: None,
                    },
                ),
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                "id",
                                None,
                            ),
                            Field(
                                None,
                                "title",
                                None,
                            ),
                            Field(
                                None,
                                "body",
                                None,
                            ),
                        ],
                        star: None,
                    },
                ),
                entity_resolver: None,
            },
        }
        "###);
    }
}
