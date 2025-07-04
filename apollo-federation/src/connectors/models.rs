mod headers;
mod http_json_transport;
mod keys;
mod problem_location;
mod source;

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use keys::make_key_field_set_from_variables;
use serde_json::Value;

pub use self::headers::Header;
pub(crate) use self::headers::HeaderParseError;
pub use self::headers::HeaderSource;
pub use self::headers::OriginatingDirective;
pub use self::http_json_transport::HTTPMethod;
pub use self::http_json_transport::HttpJsonTransport;
pub use self::http_json_transport::MakeUriError;
pub use self::problem_location::ProblemLocation;
pub use self::source::SourceName;
use super::ConnectId;
use super::JSONSelection;
use super::PathSelection;
use super::id::ConnectorPosition;
use super::json_selection::ExternalVarPaths;
use super::spec::connect::ConnectBatchArguments;
use super::spec::connect::ConnectDirectiveArguments;
use super::spec::errors::ErrorsArguments;
use super::spec::source::SourceDirectiveArguments;
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

    /// The request headers referenced in the connectors request mapping
    pub request_headers: HashSet<String>,
    /// The request or response headers referenced in the connectors response mapping
    pub response_headers: HashSet<String>,
    /// Environment and context variable keys referenced in the connector
    pub request_variable_keys: IndexMap<Namespace, IndexSet<String>>,
    pub response_variable_keys: IndexMap<Namespace, IndexSet<String>>,

    pub batch_settings: Option<ConnectBatchArguments>,

    pub error_settings: ConnectorErrorsSettings,
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
    /// before calling this function. We can't take a `Valid<Schema>` or `ValidFederationSchema`
    /// because we use this code in validation, which occurs before we've augmented
    /// the schema with types from `@link` directives.
    pub fn from_schema(
        schema: &Schema,
        subgraph_name: &str,
        spec: ConnectSpec,
    ) -> Result<Vec<Self>, FederationError> {
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
    ) -> Result<Self, FederationError> {
        let source = connect
            .source
            .and_then(|name| source_arguments.iter().find(|s| s.name == name));
        let source_name = source.map(|s| s.name.clone());

        // Create our transport
        let connect_http = connect
            .http
            .ok_or_else(|| internal_error!("@connect(http:) missing"))?;
        let source_http = source.map(|s| &s.http);
        let transport = HttpJsonTransport::from_directive(connect_http, source_http)?;

        // Get our batch and error settings
        let batch_settings = connect.batch;
        let connect_errors = connect.errors.as_ref();
        let source_errors = source.and_then(|s| s.errors.as_ref());
        let error_settings = ConnectorErrorsSettings::from_directive(connect_errors, source_errors);

        // Collect all variables and subselections used in the request mappings
        let request_references: IndexSet<VariableReference<Namespace>> =
            transport.variable_references().collect();

        // Collect all variables and subselections used in response mappings (including errors.message and errors.extensions)
        let response_references: IndexSet<VariableReference<Namespace>> = connect
            .selection
            .variable_references()
            .chain(error_settings.variable_references())
            .collect();

        // Store a map of variable names and the set of first-level of keys so we can
        // more efficiently clone values for mappings (especially for $context and $env)
        let request_variable_keys = extract_variable_key_references(request_references.iter());
        let response_variable_keys = extract_variable_key_references(response_references.iter());

        // Store a set of header names referenced in mappings (these are second-level keys)
        let request_headers = extract_header_references(&request_references); // $request in request mappings
        let response_headers = extract_header_references(&response_references); // $request or $response in response mappings

        // Last couple of items here!
        let entity_resolver = determine_entity_resolver(
            &connect.position,
            connect.entity,
            schema,
            &request_variable_keys,
        );
        let id = ConnectId {
            label: make_label(
                subgraph_name,
                source_name.as_ref(),
                &transport,
                entity_resolver.as_ref(),
            ),
            subgraph_name: subgraph_name.to_string(),
            source_name,
            directive: connect.position,
        };

        Ok(Connector {
            id,
            transport,
            selection: connect.selection,
            entity_resolver,
            config: None,
            max_requests: None,
            spec,
            request_headers,
            response_headers,
            request_variable_keys,
            response_variable_keys,
            batch_settings,
            error_settings,
        })
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
    /// `source_name` will be `None` here when we are using a "sourceless" connector. In this situation, we'll use
    /// the `synthetic_name` instead so that we have some kind of a unique identifier for this source.
    pub fn source_config_key(&self) -> String {
        if let Some(source_name) = &self.id.source_name {
            format!("{}.{}", self.id.subgraph_name, source_name)
        } else {
            format!("{}.{}", self.id.subgraph_name, self.id.synthetic_name())
        }
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
    source: Option<&SourceName>,
    transport: &HttpJsonTransport,
    entity_resolver: Option<&EntityResolver>,
) -> String {
    let source = source.map(SourceName::as_str).unwrap_or_default();
    let batch = match entity_resolver {
        Some(EntityResolver::TypeBatch) => "[BATCH] ",
        _ => "",
    };
    format!("{batch}{subgraph_name}.{source} {}", transport.label())
}

fn determine_entity_resolver(
    position: &ConnectorPosition,
    entity: bool,
    schema: &Schema,
    request_variables: &IndexMap<Namespace, IndexSet<String>>,
) -> Option<EntityResolver> {
    match position {
        ConnectorPosition::Field(_) => {
            match (entity, position.on_root_type(schema)) {
                (true, _) => Some(EntityResolver::Explicit), // Query.foo @connect(entity: true)
                (_, false) => Some(EntityResolver::Implicit), // Foo.bar @connect
                _ => None,
            }
        }
        ConnectorPosition::Type(_) => {
            if request_variables.contains_key(&Namespace::Batch) {
                Some(EntityResolver::TypeBatch) // Foo @connect($batch)
            } else {
                Some(EntityResolver::TypeSingle) // Foo @connect($this)
            }
        }
    }
}

/// Get any headers referenced in the variable references by looking at both Request and Response namespaces.
fn extract_header_references(
    variable_references: &IndexSet<VariableReference<Namespace>>,
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

/// Create a map of variable namespaces like env and context to a set of the
/// root keys referenced in the connector
fn extract_variable_key_references<'a>(
    references: impl Iterator<Item = &'a VariableReference<Namespace>>,
) -> IndexMap<Namespace, IndexSet<String>> {
    let mut variable_keys: IndexMap<Namespace, IndexSet<String>> = IndexMap::default();

    for var_ref in references {
        // make there there's a key for each namespace
        let set = variable_keys
            .entry(var_ref.namespace.namespace)
            .or_default();

        for key in var_ref.selection.keys() {
            set.insert(key.to_string());
        }
    }

    variable_keys
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
        assert_debug_snapshot!(&connectors, @r###"
        [
            Connector {
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
                    source_template: Some(
                        StringTemplate {
                            parts: [
                                Constant(
                                    Constant {
                                        value: "https://jsonplaceholder.typicode.com/",
                                        location: 0..37,
                                    },
                                ),
                            ],
                        },
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
                    headers: [
                        Header {
                            name: "authtoken",
                            source: From(
                                "x-auth-token",
                            ),
                        },
                        Header {
                            name: "user-agent",
                            source: Value(
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
                    ],
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
                request_headers: {},
                response_headers: {},
                request_variable_keys: {},
                response_variable_keys: {},
                batch_settings: None,
                error_settings: ConnectorErrorsSettings {
                    message: None,
                    source_extensions: None,
                    connect_extensions: None,
                },
            },
            Connector {
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
                    source_template: Some(
                        StringTemplate {
                            parts: [
                                Constant(
                                    Constant {
                                        value: "https://jsonplaceholder.typicode.com/",
                                        location: 0..37,
                                    },
                                ),
                            ],
                        },
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
                    headers: [
                        Header {
                            name: "authtoken",
                            source: From(
                                "x-auth-token",
                            ),
                        },
                        Header {
                            name: "user-agent",
                            source: Value(
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
                    ],
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
                request_headers: {},
                response_headers: {},
                request_variable_keys: {},
                response_variable_keys: {},
                batch_settings: None,
                error_settings: ConnectorErrorsSettings {
                    message: None,
                    source_extensions: None,
                    connect_extensions: None,
                },
            },
        ]
        "###);
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
