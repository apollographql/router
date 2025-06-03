mod http_json_transport;
mod keys;
mod problem_location;

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use keys::make_key_field_set_from_variables;
use serde_json::Value;

pub use self::http_json_transport::HTTPMethod;
pub(crate) use self::http_json_transport::Header;
pub(crate) use self::http_json_transport::HeaderParseError;
pub use self::http_json_transport::HeaderSource;
pub use self::http_json_transport::HttpJsonTransport;
pub use self::http_json_transport::MakeUriError;
pub use self::http_json_transport::OriginatingDirective;
pub use self::problem_location::ProblemLocation;
use super::ConnectId;
use super::JSONSelection;
use super::PathSelection;
use super::id::ConnectorPosition;
use super::json_selection::ExternalVarPaths;
use super::spec::schema::ConnectDirectiveArguments;
use super::spec::schema::ErrorsArguments;
use super::spec::schema::SourceDirectiveArguments;
use super::variable::Namespace;
use super::variable::VariableReference;
use crate::connectors::ConnectSpec;
use crate::connectors::spec::extract_connect_directive_arguments;
use crate::connectors::spec::extract_source_directive_arguments;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::Link;

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

    /// The request headers referenced in the connectors request mapping
    pub request_headers: HashSet<String>,
    /// The request or response headers referenced in the connectors response mapping
    pub response_headers: HashSet<String>,

    pub batch_settings: Option<ConnectorBatchSettings>,

    pub error_settings: ConnectorErrorsSettings,
}

#[derive(Debug, Clone)]
pub struct ConnectorBatchSettings {
    pub max_size: Option<usize>,
}

impl ConnectorBatchSettings {
    fn from_directive(connect: &ConnectDirectiveArguments) -> Option<Self> {
        Some(Self {
            max_size: connect.batch.as_ref().and_then(|b| b.max_size),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConnectorErrorsSettings {
    pub message: Option<JSONSelection>,
    pub source_extensions: Option<JSONSelection>,
    pub connect_extensions: Option<JSONSelection>,
}

impl ConnectorErrorsSettings {
    fn from_directive(
        connect_errors: Option<&ErrorsArguments>,
        source_errors: Option<&ErrorsArguments>,
    ) -> Self {
        let message = connect_errors
            .and_then(|e| e.message.as_ref())
            .or_else(|| source_errors.and_then(|e| e.message.as_ref()))
            .cloned();
        let source_extensions = source_errors.and_then(|e| e.extensions.as_ref()).cloned();
        let connect_extensions = connect_errors.and_then(|e| e.extensions.as_ref()).cloned();

        Self {
            message,
            source_extensions,
            connect_extensions,
        }
    }

    pub fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> + '_ {
        self.message
            .as_ref()
            .into_iter()
            .flat_map(|m| m.variable_references())
            .chain(
                self.source_extensions
                    .as_ref()
                    .into_iter()
                    .flat_map(|m| m.variable_references()),
            )
            .chain(
                self.connect_extensions
                    .as_ref()
                    .into_iter()
                    .flat_map(|m| m.variable_references()),
            )
    }
}

pub type CustomConfiguration = Arc<HashMap<String, Value>>;

/// Entity resolver type
///
/// A connector can be used as a potential entity resolver for a type, with
/// extra validation rules based on the transport args and field position within
/// a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityResolver {
    /// The user defined a connector on a field that acts as an entity resolver
    Explicit,

    /// The user defined a connector on a field of a type, so we need an entity resolver for that type
    Implicit,

    /// The user defined a connector on the type directly and uses the $batch variable
    TypeBatch,

    /// The user defined a connector on the type directly and uses the $this variable
    TypeSingle,
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

        // Create our transport
        let connect_http = connect
            .http
            .as_ref()
            .ok_or_else(|| internal_error!("@connect(http:) missing"))?;
        let source_http = source.map(|s| &s.http);
        let transport = HttpJsonTransport::from_directive(connect_http, source_http)?;

        // Get our batch and error settings
        let batch_settings = ConnectorBatchSettings::from_directive(&connect);
        let connect_errors = connect.errors.as_ref();
        let source_errors = source.and_then(|s| s.errors.as_ref());
        let error_settings = ConnectorErrorsSettings::from_directive(connect_errors, source_errors);

        // Calculate which variables and headers are in use in the request
        let request_references: HashSet<VariableReference<Namespace>> =
            transport.variable_references().collect();
        let request_variables: HashSet<Namespace> = request_references
            .iter()
            .map(|var_ref| var_ref.namespace.namespace)
            .collect();
        let request_headers = extract_header_references(request_references);

        // Calculate which variables and headers are in use in the response (including errors.message and errors.extensions)
        let response_references: HashSet<VariableReference<Namespace>> = connect
            .selection
            .variable_references()
            .chain(error_settings.variable_references())
            .collect();
        let response_variables: HashSet<Namespace> = response_references
            .iter()
            .map(|var_ref| var_ref.namespace.namespace)
            .collect();
        let response_headers = extract_header_references(response_references);

        // Last couple of items here!
        let entity_resolver = determine_entity_resolver(&connect, schema, &request_variables);
        let id = ConnectId {
            label: make_label(subgraph_name, &source_name, &transport, &entity_resolver),
            subgraph_name: subgraph_name.to_string(),
            source_name: source_name.clone(),
            directive: connect.position,
        };

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
            request_headers,
            response_headers,
            batch_settings,
            error_settings,
        };

        Ok((id, connector))
    }

    pub(crate) fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> {
        self.transport.variable_references().chain(
            self.selection
                .external_var_paths()
                .into_iter()
                .flat_map(PathSelection::variable_reference),
        )
    }

    /// Create a field set for a `@key` using `$args`, `$this`, or `$batch` variables.
    pub fn resolvable_key(&self, schema: &Schema) -> Result<Option<Valid<FieldSet>>, String> {
        match &self.entity_resolver {
            None => Ok(None),
            Some(EntityResolver::Explicit) => {
                make_key_field_set_from_variables(
                    schema,
                    &self.id.directive.base_type_name(schema).ok_or_else(|| {
                        format!("Missing field {}", self.id.directive.coordinate())
                    })?,
                    self.variable_references(),
                    Namespace::Args,
                )
            }
            Some(EntityResolver::Implicit) => {
                make_key_field_set_from_variables(
                    schema,
                    &self.id.directive.parent_type_name().ok_or_else(|| {
                        format!("Missing type {}", self.id.directive.coordinate())
                    })?,
                    self.variable_references(),
                    Namespace::This,
                )
            }
            Some(EntityResolver::TypeBatch) => {
                make_key_field_set_from_variables(
                    schema,
                    &self.id.directive.base_type_name(schema).ok_or_else(|| {
                        format!("Missing type {}", self.id.directive.coordinate())
                    })?,
                    self.variable_references(),
                    Namespace::Batch,
                )
            }
            Some(EntityResolver::TypeSingle) => {
                make_key_field_set_from_variables(
                    schema,
                    &self.id.directive.base_type_name(schema).ok_or_else(|| {
                        format!("Missing type {}", self.id.directive.coordinate())
                    })?,
                    self.variable_references(),
                    Namespace::This,
                )
            }
        }
        .map_err(|_| {
            format!(
                "Failed to create key for connector {}",
                self.id.coordinate()
            )
        })
    }

    /// Create an identifier for this connector that can be used for configuration and service identification
    /// source_name will be "none" here when we are using a "sourceless" connector. In this situation, we'll use
    /// the synthetic_name instead so that we have some kind of a unique identifier for this source.
    pub fn source_config_key(&self) -> String {
        let source_name = self
            .id
            .source_name
            .clone()
            .unwrap_or_else(|| self.id.synthetic_name());
        format!("{}.{}", self.id.subgraph_name, source_name)
    }

    /// Get the name of the `@connect` directive associated with this [`Connector`] instance.
    ///
    /// The [`Name`] can be used to help locate the connector within a source file.
    pub fn name(&self) -> Name {
        match &self.id.directive {
            ConnectorPosition::Field(field_position) => field_position.directive_name.clone(),
            ConnectorPosition::Type(type_position) => type_position.directive_name.clone(),
        }
    }
}

fn make_label(
    subgraph_name: &str,
    source: &Option<String>,
    transport: &HttpJsonTransport,
    entity_resolver: &Option<EntityResolver>,
) -> String {
    let source = format!(".{}", source.as_deref().unwrap_or(""));
    let batch = match entity_resolver {
        Some(EntityResolver::TypeBatch) => "[BATCH] ",
        _ => "",
    };
    format!("{}{}{} {}", batch, subgraph_name, source, transport.label())
}

fn determine_entity_resolver(
    connect: &ConnectDirectiveArguments,
    schema: &Schema,
    request_variables: &HashSet<Namespace>,
) -> Option<EntityResolver> {
    match connect.position {
        ConnectorPosition::Field(_) => {
            match (connect.entity, connect.position.on_root_type(schema)) {
                (true, _) => Some(EntityResolver::Explicit), // Query.foo @connect(entity: true)
                (_, false) => Some(EntityResolver::Implicit), // Foo.bar @connect
                _ => None,
            }
        }
        ConnectorPosition::Type(_) => {
            if request_variables.contains(&Namespace::Batch) {
                Some(EntityResolver::TypeBatch) // Foo @connect($batch)
            } else {
                Some(EntityResolver::TypeSingle) // Foo @connect($this)
            }
        }
    }
}

/// Get any headers referenced in the variable references by looking at both Request and Response namespaces.
fn extract_header_references(
    variable_references: HashSet<VariableReference<Namespace>>,
) -> HashSet<String> {
    variable_references
        .iter()
        .flat_map(|var_ref| {
            if var_ref.namespace.namespace != Namespace::Request
                && var_ref.namespace.namespace != Namespace::Response
            {
                Vec::new()
            } else {
                var_ref
                    .selection
                    .get("headers")
                    .map(|headers_subtrie| headers_subtrie.keys().cloned().collect())
                    .unwrap_or_default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::ValidFederationSubgraphs;
    use crate::schema::FederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    static SIMPLE_SUPERGRAPH: &str = include_str!("./tests/schemas/simple.graphql");
    static SIMPLE_SUPERGRAPH_V0_2: &str = include_str!("./tests/schemas/simple_v0_2.graphql");

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
        assert_debug_snapshot!(&connectors, @r#"
        {
            ConnectId {
                label: "connectors.json http: GET /users",
                subgraph_name: "connectors",
                source_name: Some(
                    "json",
                ),
                directive: Field(
                    ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(Query.users),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                ),
            }: Connector {
                id: ConnectId {
                    label: "connectors.json http: GET /users",
                    subgraph_name: "connectors",
                    source_name: Some(
                        "json",
                    ),
                    directive: Field(
                        ObjectOrInterfaceFieldDirectivePosition {
                            field: Object(Query.users),
                            directive_name: "connect",
                            directive_index: 0,
                        },
                    ),
                },
                transport: HttpJsonTransport {
                    source_url: Some(
                        https://jsonplaceholder.typicode.com/,
                    ),
                    connect_template: StringTemplate {
                        parts: [
                            Constant(
                                Constant {
                                    value: "/users",
                                    location: 0..6,
                                },
                            ),
                        ],
                    },
                    method: Get,
                    headers: {
                        "authtoken": (
                            From(
                                "x-auth-token",
                            ),
                            Source,
                        ),
                        "user-agent": (
                            Value(
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
                            Source,
                        ),
                    },
                    body: None,
                    source_path: None,
                    source_query_params: None,
                    connect_path: None,
                    connect_query_params: None,
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
                request_headers: {},
                response_headers: {},
                batch_settings: Some(
                    ConnectorBatchSettings {
                        max_size: None,
                    },
                ),
                error_settings: ConnectorErrorsSettings {
                    message: None,
                    source_extensions: None,
                    connect_extensions: None,
                },
            },
            ConnectId {
                label: "connectors.json http: GET /posts",
                subgraph_name: "connectors",
                source_name: Some(
                    "json",
                ),
                directive: Field(
                    ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(Query.posts),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                ),
            }: Connector {
                id: ConnectId {
                    label: "connectors.json http: GET /posts",
                    subgraph_name: "connectors",
                    source_name: Some(
                        "json",
                    ),
                    directive: Field(
                        ObjectOrInterfaceFieldDirectivePosition {
                            field: Object(Query.posts),
                            directive_name: "connect",
                            directive_index: 0,
                        },
                    ),
                },
                transport: HttpJsonTransport {
                    source_url: Some(
                        https://jsonplaceholder.typicode.com/,
                    ),
                    connect_template: StringTemplate {
                        parts: [
                            Constant(
                                Constant {
                                    value: "/posts",
                                    location: 0..6,
                                },
                            ),
                        ],
                    },
                    method: Get,
                    headers: {
                        "authtoken": (
                            From(
                                "x-auth-token",
                            ),
                            Source,
                        ),
                        "user-agent": (
                            Value(
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
                            Source,
                        ),
                    },
                    body: None,
                    source_path: None,
                    source_query_params: None,
                    connect_path: None,
                    connect_query_params: None,
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
                request_headers: {},
                response_headers: {},
                batch_settings: Some(
                    ConnectorBatchSettings {
                        max_size: None,
                    },
                ),
                error_settings: ConnectorErrorsSettings {
                    message: None,
                    source_extensions: None,
                    connect_extensions: None,
                },
            },
        }
        "#);
    }

    #[test]
    fn test_from_schema_v0_2() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH_V0_2);
        let subgraph = subgraphs.get("connectors").unwrap();
        let connectors =
            Connector::from_schema(subgraph.schema.schema(), "connectors", ConnectSpec::V0_2)
                .unwrap();
        assert_debug_snapshot!(&connectors);
    }
}
