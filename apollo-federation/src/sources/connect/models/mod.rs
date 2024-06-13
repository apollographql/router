mod validation;

use apollo_compiler::NodeStr;
use indexmap::IndexMap;
pub use validation::validate;
pub use validation::Code as ValidationCode;
pub use validation::Location;
pub use validation::Message as ValidationMessage;

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
    pub entity: bool,
    pub on_root_type: bool,
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

impl Connector {
    pub(crate) fn from_valid_schema(
        schema: &ValidFederationSchema,
        subgraph_name: NodeStr,
    ) -> Result<IndexMap<ConnectId, Self>, FederationError> {
        let Some(metadata) = schema.metadata() else {
            return Ok(IndexMap::new());
        };

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
                let on_root_type = schema_def
                    .query
                    .as_ref()
                    .map(|ty| ty.name == parent_type_name)
                    .or(schema_def
                        .mutation
                        .as_ref()
                        .map(|ty| ty.name == parent_type_name))
                    .unwrap_or(false);

                let id = ConnectId {
                    label: make_label(&subgraph_name, source_name, &transport),
                    subgraph_name: subgraph_name.clone(),
                    directive: args.position,
                };

                let connector = Connector {
                    id: id.clone(),
                    transport,
                    selection: args.selection,
                    entity: args.entity,
                    on_root_type,
                };

                Ok((id, connector))
            })
            .collect()
    }
}

fn make_label(subgraph_name: &NodeStr, source: Option<NodeStr>, transport: &Transport) -> String {
    let source = format!(".{}", source.as_deref().unwrap_or(""));
    format!("{}{} {}", subgraph_name, source, transport.label())
}

// --- HTTP JSON ---------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HttpJsonTransport {
    pub base_url: NodeStr,
    pub path_template: URLPathTemplate,
    pub method: HTTPMethod,
    pub headers: IndexMap<NodeStr, Option<HTTPHeaderOption>>,
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

        let base_url = if let Some(base_url) = source.as_ref().map(|source| source.base_url.clone())
        {
            base_url
        } else {
            todo!("get base url from path")
        };

        let mut headers = source
            .as_ref()
            .map(|source| source.headers.0.clone())
            .unwrap_or_default();
        headers.extend(http.headers.0.clone());

        Ok(Self {
            base_url,
            path_template: URLPathTemplate::parse(path).expect("path template"),
            method,
            headers,
            body: http.body.clone(),
        })
    }

    fn label(&self) -> String {
        format!("http: {} {}", self.method, self.path_template)
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
            Connector::from_valid_schema(&subgraph.schema, "connectors".into()).unwrap();
        assert_debug_snapshot!(&connectors, @r###"
        {
            ConnectId {
                label: "connectors.json http: Get /users",
                subgraph_name: "connectors",
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(Query.users),
                    directive_name: "connect",
                    directive_index: 0,
                },
            }: Connector {
                id: ConnectId {
                    label: "connectors.json http: Get /users",
                    subgraph_name: "connectors",
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
                        headers: {
                            "X-Auth-Token": Some(
                                As(
                                    "AuthToken",
                                ),
                            ),
                            "user-agent": Some(
                                Value(
                                    [
                                        "Firefox",
                                    ],
                                ),
                            ),
                            "X-From-Env": None,
                        },
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
                entity: false,
                on_root_type: true,
            },
            ConnectId {
                label: "connectors.json http: Get /posts",
                subgraph_name: "connectors",
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(Query.posts),
                    directive_name: "connect",
                    directive_index: 0,
                },
            }: Connector {
                id: ConnectId {
                    label: "connectors.json http: Get /posts",
                    subgraph_name: "connectors",
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
                        headers: {
                            "X-Auth-Token": Some(
                                As(
                                    "AuthToken",
                                ),
                            ),
                            "user-agent": Some(
                                Value(
                                    [
                                        "Firefox",
                                    ],
                                ),
                            ),
                            "X-From-Env": None,
                        },
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
                entity: false,
                on_root_type: true,
            },
        }
        "###);
    }
}
