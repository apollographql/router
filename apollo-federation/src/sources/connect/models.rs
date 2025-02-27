mod keys;

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Display;
use std::fmt::Formatter;
use std::str::FromStr;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::parser::SourceSpan;
use apollo_compiler::validation::Valid;
use either::Either;
use http::HeaderName;
use http::header;
use keys::make_key_field_set_from_variables;
use serde_json::Value;
use url::Url;

use super::ConnectId;
use super::JSONSelection;
use super::PathSelection;
use super::URLTemplate;
use super::json_selection::ExternalVarPaths;
use super::spec::ConnectHTTPArguments;
use super::spec::SourceHTTPArguments;
use super::spec::schema::ConnectDirectiveArguments;
use super::spec::schema::SourceDirectiveArguments;
use super::string_template;
use super::variable::Namespace;
use super::variable::VariableReference;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::Link;
use crate::sources::connect::ConnectSpec;
use crate::sources::connect::header::HeaderValue;
use crate::sources::connect::spec::extract_connect_directive_arguments;
use crate::sources::connect::spec::extract_source_directive_arguments;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME;

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
    /// Which version of the connect spec is this connector using?
    pub spec: ConnectSpec,

    pub request_variables: HashSet<Namespace>,
    pub response_variables: HashSet<Namespace>,
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
    /// Get a map of connectors from an apollo_compiler::Schema.
    ///
    /// Note: the function assumes that we've checked that the schema is valid
    /// before calling this function. We can't take a Valid<Schema> or ValidFederationSchema
    /// because we use this code in validation, which occurs before we've augmented
    /// the schema with types from `@link` directives.
    pub(crate) fn from_schema(
        schema: &Schema,
        subgraph_name: &str,
        spec: ConnectSpec,
    ) -> Result<IndexMap<ConnectId, Self>, FederationError> {
        let connect_identity = ConnectSpec::identity();
        let Some((link, _)) = Link::for_identity(schema, &connect_identity) else {
            return Ok(Default::default());
        };

        let source_name = ConnectSpec::source_directive_name(&link);
        let source_arguments = extract_source_directive_arguments(schema, &source_name)?;

        let connect_name = ConnectSpec::connect_directive_name(&link);
        let connect_arguments = extract_connect_directive_arguments(schema, &connect_name)?;

        connect_arguments
            .into_iter()
            .map(|args| Self::from_directives(schema, subgraph_name, spec, args, &source_arguments))
            .collect()
    }

    fn from_directives(
        schema: &Schema,
        subgraph_name: &str,
        spec: ConnectSpec,
        connect: ConnectDirectiveArguments,
        source_arguments: &[SourceDirectiveArguments],
    ) -> Result<(ConnectId, Self), FederationError> {
        let source = connect
            .source
            .as_ref()
            .and_then(|name| source_arguments.iter().find(|s| s.name == *name));

        let source_name = source.map(|s| s.name.clone());
        let connect_http = connect
            .http
            .ok_or_else(|| internal_error!("@connect(http:) missing"))?;
        let source_http = source.map(|s| &s.http);

        let transport = HttpJsonTransport::from_directive(connect_http, source_http)?;

        let parent_type_name = connect.position.field.type_name().clone();
        let schema_def = &schema.schema_definition;
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
            directive: connect.position,
        };

        let entity_resolver = match (connect.entity, on_root_type) {
            (true, _) => Some(EntityResolver::Explicit),
            (_, false) => Some(EntityResolver::Implicit),
            _ => None,
        };

        let request_variables = transport.variables().collect();
        let response_variables = connect.selection.external_variables().collect();

        let connector = Connector {
            id: id.clone(),
            transport,
            selection: connect.selection,
            entity_resolver,
            config: None,
            max_requests: None,
            spec,
            request_variables,
            response_variables,
        };

        Ok((id, connector))
    }

    pub fn field_name(&self) -> &Name {
        self.id.directive.field.field_name()
    }

    pub(crate) fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> {
        self.transport.variable_references().chain(
            self.selection
                .external_var_paths()
                .into_iter()
                .flat_map(PathSelection::variable_reference),
        )
    }

    /// Create a field set for a `@key` using $args and $this variables.
    pub(crate) fn resolvable_key(
        &self,
        schema: &Schema,
    ) -> Result<Option<Valid<FieldSet>>, FederationError> {
        match &self.entity_resolver {
            None => Ok(None),
            Some(EntityResolver::Explicit) => {
                let output_type = self
                    .id
                    .directive
                    .field
                    .get(schema)
                    .map(|f| f.ty.inner_named_type())
                    .map_err(|_| {
                        internal_error!(
                            "Missing field {}.{}",
                            self.id.directive.field.type_name(),
                            self.id.directive.field.field_name()
                        )
                    })?;
                make_key_field_set_from_variables(
                    schema,
                    output_type,
                    self.variable_references(),
                    EntityResolver::Explicit,
                )
                .map_err(|_| {
                    internal_error!("Failed to create key for connector {}", self.id.label)
                })
            }
            Some(EntityResolver::Implicit) => make_key_field_set_from_variables(
                schema,
                self.id.directive.field.type_name(),
                self.variable_references(),
                EntityResolver::Implicit,
            )
            .map_err(|_| internal_error!("Failed to create key for connector {}", self.id.label)),
        }
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
            connect_template: connect_url.parse().map_err(|e: string_template::Error| {
                FederationError::internal(format!(
                    "could not parse URL template: {message}",
                    message = e.message
                ))
            })?,
            method,
            headers,
            body: http.body.clone(),
        })
    }

    fn label(&self) -> String {
        format!("http: {} {}", self.method.as_str(), self.connect_template)
    }

    fn variables(&self) -> impl Iterator<Item = Namespace> {
        self.variable_references()
            .map(|var_ref| var_ref.namespace.namespace)
    }

    fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> {
        let url_selections = self.connect_template.expressions().map(|e| &e.expression);
        let header_selections = self
            .headers
            .iter()
            .flat_map(|(_, source)| source.expressions());
        url_selections
            .chain(header_selections)
            .chain(self.body.iter())
            .flat_map(|b| {
                b.external_var_paths()
                    .into_iter()
                    .flat_map(PathSelection::variable_reference)
            })
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

impl FromStr for HTTPMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "GET" => Ok(HTTPMethod::Get),
            "POST" => Ok(HTTPMethod::Post),
            "PATCH" => Ok(HTTPMethod::Patch),
            "PUT" => Ok(HTTPMethod::Put),
            "DELETE" => Ok(HTTPMethod::Delete),
            _ => Err(format!("Invalid HTTP method: {s}")),
        }
    }
}

#[derive(Clone, Debug)]
pub enum HeaderSource {
    From(HeaderName),
    Value(HeaderValue),
}

impl HeaderSource {
    pub(crate) fn expressions(&self) -> impl Iterator<Item = &JSONSelection> {
        match self {
            HeaderSource::From(_) => Either::Left(std::iter::empty()),
            HeaderSource::Value(value) => Either::Right(value.expressions().map(|e| &e.expression)),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Header<'a> {
    pub(crate) name: HeaderName,
    pub(crate) name_node: &'a Node<ast::Value>,
    pub(crate) source: HeaderSource,
    pub(crate) source_node: &'a Node<ast::Value>,
}

impl<'a> Header<'a> {
    /// Get a list of headers from the `headers` argument in a `@connect` or `@source` directive.
    pub(crate) fn from_headers_arg(
        node: &'a Node<ast::Value>,
    ) -> Vec<Result<Self, HeaderParseError<'a>>> {
        if let Some(values) = node.as_list() {
            values.iter().map(Self::from_single).collect()
        } else if node.as_object().is_some() {
            vec![Self::from_single(node)]
        } else {
            vec![Err(HeaderParseError::Other {
                message: format!("`{HEADERS_ARGUMENT_NAME}` must be an object or list of objects"),
                node,
            })]
        }
    }

    /// Build a single [`Self`] from a single entry in the `headers` arg.
    fn from_single(node: &'a Node<ast::Value>) -> Result<Self, HeaderParseError<'a>> {
        let mappings = node.as_object().ok_or_else(|| HeaderParseError::Other {
            message: "the HTTP header mapping is not an object".to_string(),
            node,
        })?;
        let name_node = mappings
            .iter()
            .find_map(|(name, value)| {
                (*name == HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME).then_some(value)
            })
            .ok_or_else(|| HeaderParseError::Other {
                message: format!("missing `{HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME}` field"),
                node,
            })?;
        let name = name_node
            .as_str()
            .ok_or_else(|| format!("`{HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME}` is not a string"))
            .and_then(|name_str| {
                HeaderName::try_from(name_str)
                    .map_err(|_| format!("the value `{name_str}` is an invalid HTTP header name"))
            })
            .map_err(|message| HeaderParseError::Other {
                message,
                node: name_node,
            })?;

        if Self::is_reserved(&name) {
            return Err(HeaderParseError::Other {
                message: format!("header '{name}' is reserved and cannot be set by a connector"),
                node: name_node,
            });
        }

        let from = mappings
            .iter()
            .find(|(name, _value)| *name == HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME);
        let value = mappings
            .iter()
            .find(|(name, _value)| *name == HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME);

        match (from, value) {
            (Some(_), None) if Self::is_static(&name) => {
                Err(HeaderParseError::Other{ message: format!(
                    "header '{name}' can't be set with `{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}`, only with `{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}`"
                ), node: name_node})
            }
            (Some((_, from_node)), None) => {
                from_node.as_str()
                    .ok_or_else(|| format!("`{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}` is not a string"))
                    .and_then(|from_str| {
                        HeaderName::try_from(from_str).map_err(|_| {
                            format!("the value `{from_str}` is an invalid HTTP header name")
                        })
                    })
                    .map(|from| Self {
                        name,
                        name_node,
                        source: HeaderSource::From(from),
                        source_node: from_node,
                    })
                    .map_err(|message| HeaderParseError::Other{ message, node: from_node})
            }
            (None, Some((_, value_node))) => {
                value_node
                    .as_str()
                    .ok_or_else(|| HeaderParseError::Other{ message: format!("`{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}` field in HTTP header mapping must be a string"), node: value_node})
                    .and_then(|value_str| {
                        value_str
                            .parse::<HeaderValue>()
                            .map_err(|err| HeaderParseError::ValueError {err, node: value_node})
                    })
                    .map(|value| Self {
                        name,
                        name_node,
                        source: HeaderSource::Value(value),
                        source_node: value_node,
                    })
            }
            (None, None) => {
                Err(HeaderParseError::Other {
                    message: format!("either `{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}` or `{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}` must be set"),
                    node,
                })
            },
            (Some((from_name, _)), Some((value_name, _))) => {
                Err(HeaderParseError::ConflictingArguments {
                    message: format!("`{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}` and `{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}` can't be set at the same time"),
                    from_location: from_name.location(),
                    value_location: value_name.location(),
                })
            }
        }
    }

    /// These headers are not allowed to be defined by connect directives at all.
    /// Copied from Router's plugins::headers
    /// Headers from https://datatracker.ietf.org/doc/html/rfc2616#section-13.5.1
    /// These are not propagated by default using a regex match as they will not make sense for the
    /// second hop.
    /// In addition, because our requests are not regular proxy requests content-type, content-length
    /// and host are also in the exclude list.
    fn is_reserved(header_name: &HeaderName) -> bool {
        static KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");
        matches!(
            *header_name,
            header::CONNECTION
                | header::PROXY_AUTHENTICATE
                | header::PROXY_AUTHORIZATION
                | header::TE
                | header::TRAILER
                | header::TRANSFER_ENCODING
                | header::UPGRADE
                | header::CONTENT_LENGTH
                | header::CONTENT_ENCODING
                | header::HOST
                | header::ACCEPT_ENCODING
        ) || header_name == KEEP_ALIVE
    }

    /// These headers can be defined as static values in connect directives, but can't be
    /// forwarded by the user.
    fn is_static(header_name: &HeaderName) -> bool {
        matches!(*header_name, header::CONTENT_TYPE | header::ACCEPT,)
    }
}

#[derive(Debug)]
pub(crate) enum HeaderParseError<'a> {
    ValueError {
        err: string_template::Error,
        node: &'a Node<ast::Value>,
    },
    /// Both `value` and `from` are set
    ConflictingArguments {
        message: String,
        from_location: Option<SourceSpan>,
        value_location: Option<SourceSpan>,
    },
    Other {
        message: String,
        node: &'a Node<ast::Value>,
    },
}

impl Display for HeaderParseError<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConflictingArguments { message, .. } | Self::Other { message, .. } => {
                write!(f, "{}", message)
            }
            Self::ValueError { err, .. } => write!(f, "{err}"),
        }
    }
}

impl Error for HeaderParseError<'_> {}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::ValidFederationSubgraphs;
    use crate::schema::FederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

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
        let connectors =
            Connector::from_schema(subgraph.schema.schema(), "connectors", ConnectSpec::V0_1)
                .unwrap();
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
                            StringTemplate {
                                parts: [
                                    Constant(
                                        Constant {
                                            value: "users",
                                            location: 1..6,
                                        },
                                    ),
                                ],
                            },
                        ],
                        query: [],
                    },
                    method: Get,
                    headers: {
                        "authtoken": From(
                            "x-auth-token",
                        ),
                        "user-agent": Value(
                            HeaderValue(
                                StringTemplate {
                                    parts: [
                                        Constant(
                                            Constant {
                                                value: "Firefox",
                                                location: 0..7,
                                            },
                                        ),
                                    ],
                                },
                            ),
                        ),
                    },
                    body: None,
                },
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "id",
                                    ),
                                    range: Some(
                                        0..2,
                                    ),
                                },
                                None,
                            ),
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "name",
                                    ),
                                    range: Some(
                                        3..7,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..7,
                        ),
                    },
                ),
                config: None,
                max_requests: None,
                entity_resolver: None,
                spec: V0_1,
                request_variables: {},
                response_variables: {},
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
                            StringTemplate {
                                parts: [
                                    Constant(
                                        Constant {
                                            value: "posts",
                                            location: 1..6,
                                        },
                                    ),
                                ],
                            },
                        ],
                        query: [],
                    },
                    method: Get,
                    headers: {
                        "authtoken": From(
                            "x-auth-token",
                        ),
                        "user-agent": Value(
                            HeaderValue(
                                StringTemplate {
                                    parts: [
                                        Constant(
                                            Constant {
                                                value: "Firefox",
                                                location: 0..7,
                                            },
                                        ),
                                    ],
                                },
                            ),
                        ),
                    },
                    body: None,
                },
                selection: Named(
                    SubSelection {
                        selections: [
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "id",
                                    ),
                                    range: Some(
                                        0..2,
                                    ),
                                },
                                None,
                            ),
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "title",
                                    ),
                                    range: Some(
                                        3..8,
                                    ),
                                },
                                None,
                            ),
                            Field(
                                None,
                                WithRange {
                                    node: Field(
                                        "body",
                                    ),
                                    range: Some(
                                        9..13,
                                    ),
                                },
                                None,
                            ),
                        ],
                        range: Some(
                            0..13,
                        ),
                    },
                ),
                config: None,
                max_requests: None,
                entity_resolver: None,
                spec: V0_1,
                request_variables: {},
                response_variables: {},
            },
        }
        "###);
    }
}
