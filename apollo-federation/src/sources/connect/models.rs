pub(super) mod http_json_transport;
mod keys;

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Schema;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use http_json_transport::HttpJsonTransport;
use keys::make_key_field_set_from_variables;
use serde_json::Value;

use super::ConnectId;
use super::JSONSelection;
use super::PathSelection;
use super::id::ConnectorPosition;
use super::json_selection::ExternalVarPaths;
use super::spec::schema::ConnectDirectiveArguments;
use super::spec::schema::SourceDirectiveArguments;
use super::spec::versions::VersionInfo;
use super::variable::Namespace;
use super::variable::VariableReference;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::Link;
use crate::sources::connect::ConnectSpec;
use crate::sources::connect::spec::extract_connect_directive_arguments;
use crate::sources::connect::spec::extract_source_directive_arguments;

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

        let version: VersionInfo = spec.into();

        let source_name = ConnectSpec::source_directive_name(&link);
        let source_arguments = extract_source_directive_arguments(schema, &source_name, &version)?;

        let connect_name = ConnectSpec::connect_directive_name(&link);
        let connect_arguments =
            extract_connect_directive_arguments(schema, &connect_name, &version)?;

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
            .as_ref()
            .ok_or_else(|| internal_error!("@connect(http:) missing"))?;
        let source_http = source.map(|s| &s.http);

        let transport = HttpJsonTransport::from_directive(connect_http, source_http)?;
        let request_variables = transport.variables().collect();
        let response_variables = connect.selection.external_variables().collect();
        let entity_resolver = determine_entity_resolver(&connect, schema, &request_variables);

        let id = ConnectId {
            label: make_label(subgraph_name, &source_name, &transport),
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

    /// Create a field set for a `@key` using $args and $this variables.
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
            .unwrap_or(self.id.synthetic_name());
        format!("{}.{}", self.id.subgraph_name, source_name)
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
                    origin: None,
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
                    origin: None,
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
            },
        }
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
