use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::Name;
use http::HeaderName;
use serde_json::Value;
use url::Url;

use super::spec::ConnectHTTPArguments;
use super::spec::SourceHTTPArguments;
use super::url_template;
use super::ConnectId;
use super::JSONSelection;
use super::URLTemplate;
use crate::error::FederationError;
use crate::schema::ValidFederationSchema;
use crate::sources::connect::header::HeaderValue;
use crate::sources::connect::spec::extract_connect_directive_arguments;
use crate::sources::connect::spec::extract_source_directive_arguments;
use crate::sources::connect::ConnectSpecDefinition;
// --- Connector ---------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Connector {
    pub id: ConnectId,
    pub transport: HttpJsonTransport,
    pub selection: JSONSelection,
    pub config: Option<CustomConfiguration>,
    pub max_requests: Option<usize>,

    /// The type of entity resolver to use for this connector
    pub entity_resolver: Option<EntityResolver>,
}

pub type CustomConfiguration = Arc<HashMap<String, Value>>;

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
    ) -> Result<IndexMap<ConnectId, Self>, FederationError> {
        let Some(metadata) = schema.metadata() else {
            return Ok(IndexMap::with_hasher(Default::default()));
        };

        let Some(link) = metadata.for_identity(&ConnectSpecDefinition::identity()) else {
            return Ok(IndexMap::with_hasher(Default::default()));
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

                let transport = HttpJsonTransport::from_directive(connect_http, source_http)?;

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
                    config: None,
                    max_requests: None,
                };

                Ok((id, connector))
            })
            .collect()
    }

    pub fn field_name(&self) -> &Name {
        self.id.directive.field.field_name()
    }
}

fn make_label(
    subgraph_name: &str,
    source: &Option<String>,
    transport: &HttpJsonTransport,
) -> String {
    let source = format!(".{}", source.as_deref().unwrap_or(""));
    format!("{}{} {}", subgraph_name, source, transport.label())
}

// --- HTTP JSON ---------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct HttpJsonTransport {
    pub source_url: Option<Url>,
    pub connect_template: URLTemplate,
    pub method: HTTPMethod,
    pub headers: IndexMap<HeaderName, HeaderSource>,
    pub body: Option<JSONSelection>,
}

impl HttpJsonTransport {
    fn from_directive(
        http: ConnectHTTPArguments,
        source: Option<&SourceHTTPArguments>,
    ) -> Result<Self, FederationError> {
        let (method, connect_url) = if let Some(url) = &http.get {
            (HTTPMethod::Get, url)
        } else if let Some(url) = &http.post {
            (HTTPMethod::Post, url)
        } else if let Some(url) = &http.patch {
            (HTTPMethod::Patch, url)
        } else if let Some(url) = &http.put {
            (HTTPMethod::Put, url)
        } else if let Some(url) = &http.delete {
            (HTTPMethod::Delete, url)
        } else {
            return Err(FederationError::internal("missing http method"));
        };

        #[allow(clippy::mutable_key_type)]
        // HeaderName is internally mutable, but we don't mutate it
        let mut headers = http.headers;
        for (header_name, header_source) in
            source.map(|source| &source.headers).into_iter().flatten()
        {
            if !headers.contains_key(header_name) {
                headers.insert(header_name.clone(), header_source.clone());
            }
        }

        Ok(Self {
            source_url: source.map(|s| s.base_url.clone()),
            connect_template: connect_url.parse().map_err(
                |url_template::Error { message, .. }| {
                    FederationError::internal(format!("could not parse URL template: {message}"))
                },
            )?,
            method,
            headers,
            body: http.body.clone(),
        })
    }

    fn label(&self) -> String {
        format!("http: {} {}", self.method.as_str(), self.connect_template)
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
pub enum HeaderSource {
    From(String),
    Value(HeaderValue),
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::schema::FederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;
    use crate::ValidFederationSubgraphs;

    static SIMPLE_SUPERGRAPH: &str = include_str!("./tests/schemas/simple.graphql");

    fn get_subgraphs(supergraph_sdl: &str) -> ValidFederationSubgraphs {
        let schema = Schema::parse(supergraph_sdl, "supergraph.graphql").unwrap();
        let supergraph_schema = FederationSchema::new(schema).unwrap();
        extract_subgraphs_from_supergraph(&supergraph_schema, Some(true)).unwrap()
    }

    #[test]
    fn test_from_schema() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();
        let connectors = Connector::from_valid_schema(&subgraph.schema, "connectors").unwrap();
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
                transport: HttpJsonTransport {
                    source_url: Some(
                        Url {
                            scheme: "https",
                            cannot_be_a_base: false,
                            username: "",
                            password: None,
                            host: Some(
                                Domain(
                                    "jsonplaceholder.typicode.com",
                                ),
                            ),
                            port: None,
                            path: "/",
                            query: None,
                            fragment: None,
                        },
                    ),
                    connect_template: URLTemplate {
                        base: None,
                        path: [
                            Component {
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
                        "authtoken": From(
                            "X-Auth-Token",
                        ),
                        "user-agent": Value(
                            HeaderValue {
                                parts: [
                                    Text(
                                        "Firefox",
                                    ),
                                ],
                            },
                        ),
                    },
                    body: None,
                },
                selection: Named(
                    Parsed {
                        node: SubSelection {
                            selections: [
                                Parsed {
                                    node: Field(
                                        None,
                                        Parsed {
                                            node: Field(
                                                "id",
                                            ),
                                            loc: Some(
                                                (
                                                    0,
                                                    2,
                                                ),
                                            ),
                                        },
                                        None,
                                    ),
                                    loc: Some(
                                        (
                                            0,
                                            2,
                                        ),
                                    ),
                                },
                                Parsed {
                                    node: Field(
                                        None,
                                        Parsed {
                                            node: Field(
                                                "name",
                                            ),
                                            loc: Some(
                                                (
                                                    3,
                                                    7,
                                                ),
                                            ),
                                        },
                                        None,
                                    ),
                                    loc: Some(
                                        (
                                            3,
                                            7,
                                        ),
                                    ),
                                },
                            ],
                            star: None,
                        },
                        loc: Some(
                            (
                                0,
                                7,
                            ),
                        ),
                    },
                ),
                config: None,
                max_requests: None,
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
                transport: HttpJsonTransport {
                    source_url: Some(
                        Url {
                            scheme: "https",
                            cannot_be_a_base: false,
                            username: "",
                            password: None,
                            host: Some(
                                Domain(
                                    "jsonplaceholder.typicode.com",
                                ),
                            ),
                            port: None,
                            path: "/",
                            query: None,
                            fragment: None,
                        },
                    ),
                    connect_template: URLTemplate {
                        base: None,
                        path: [
                            Component {
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
                        "authtoken": From(
                            "X-Auth-Token",
                        ),
                        "user-agent": Value(
                            HeaderValue {
                                parts: [
                                    Text(
                                        "Firefox",
                                    ),
                                ],
                            },
                        ),
                    },
                    body: None,
                },
                selection: Named(
                    Parsed {
                        node: SubSelection {
                            selections: [
                                Parsed {
                                    node: Field(
                                        None,
                                        Parsed {
                                            node: Field(
                                                "id",
                                            ),
                                            loc: Some(
                                                (
                                                    0,
                                                    2,
                                                ),
                                            ),
                                        },
                                        None,
                                    ),
                                    loc: Some(
                                        (
                                            0,
                                            2,
                                        ),
                                    ),
                                },
                                Parsed {
                                    node: Field(
                                        None,
                                        Parsed {
                                            node: Field(
                                                "title",
                                            ),
                                            loc: Some(
                                                (
                                                    3,
                                                    8,
                                                ),
                                            ),
                                        },
                                        None,
                                    ),
                                    loc: Some(
                                        (
                                            3,
                                            8,
                                        ),
                                    ),
                                },
                                Parsed {
                                    node: Field(
                                        None,
                                        Parsed {
                                            node: Field(
                                                "body",
                                            ),
                                            loc: Some(
                                                (
                                                    9,
                                                    13,
                                                ),
                                            ),
                                        },
                                        None,
                                    ),
                                    loc: Some(
                                        (
                                            9,
                                            13,
                                        ),
                                    ),
                                },
                            ],
                            star: None,
                        },
                        loc: Some(
                            (
                                0,
                                13,
                            ),
                        ),
                    },
                ),
                config: None,
                max_requests: None,
                entity_resolver: None,
            },
        }
        "###);
    }
}
