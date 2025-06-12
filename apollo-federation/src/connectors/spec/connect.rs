use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use itertools::Itertools;

use super::errors::ERRORS_ARGUMENT_NAME;
use super::errors::ErrorsArguments;
use super::http::HTTP_ARGUMENT_NAME;
use super::http::PATH_ARGUMENT_NAME;
use super::http::QUERY_PARAMS_ARGUMENT_NAME;
use crate::connectors::ConnectorPosition;
use crate::connectors::ObjectFieldDefinitionPosition;
use crate::connectors::OriginatingDirective;
use crate::connectors::SourceName;
use crate::connectors::id::ObjectTypeDefinitionDirectivePosition;
use crate::connectors::json_selection::JSONSelection;
use crate::connectors::models::Header;
use crate::error::FederationError;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

pub(crate) const CONNECT_DIRECTIVE_NAME_IN_SPEC: Name = name!("connect");
pub(crate) const CONNECT_SOURCE_ARGUMENT_NAME: Name = name!("source");
pub(crate) const CONNECT_SELECTION_ARGUMENT_NAME: Name = name!("selection");
pub(crate) const CONNECT_ENTITY_ARGUMENT_NAME: Name = name!("entity");
pub(crate) const CONNECT_HTTP_NAME_IN_SPEC: Name = name!("ConnectHTTP");
pub(crate) const CONNECT_BATCH_NAME_IN_SPEC: Name = name!("ConnectBatch");
pub(crate) const CONNECT_BODY_ARGUMENT_NAME: Name = name!("body");
pub(crate) const BATCH_ARGUMENT_NAME: Name = name!("batch");

pub(crate) fn extract_connect_directive_arguments(
    schema: &Schema,
    name: &Name,
) -> Result<Vec<ConnectDirectiveArguments>, FederationError> {
    // connect on fields
    schema
        .types
        .iter()
        .filter_map(|(name, ty)| match ty {
            apollo_compiler::schema::ExtendedType::Object(node) => {
                Some((name, &node.fields, /* is_interface */ false))
            }
            apollo_compiler::schema::ExtendedType::Interface(node) => {
                Some((name, &node.fields, /* is_interface */ true))
            }
            _ => None,
        })
        .flat_map(|(type_name, fields, is_interface)| {
            fields.iter().flat_map(move |(field_name, field_def)| {
                field_def
                    .directives
                    .iter()
                    .enumerate()
                    .filter(|(_, directive)| directive.name == *name)
                    .map(move |(i, directive)| {
                        let field_pos = if is_interface {
                            ObjectOrInterfaceFieldDefinitionPosition::Interface(
                                InterfaceFieldDefinitionPosition {
                                    type_name: type_name.clone(),
                                    field_name: field_name.clone(),
                                },
                            )
                        } else {
                            ObjectOrInterfaceFieldDefinitionPosition::Object(
                                ObjectFieldDefinitionPosition {
                                    type_name: type_name.clone(),
                                    field_name: field_name.clone(),
                                },
                            )
                        };

                        let position =
                            ConnectorPosition::Field(ObjectOrInterfaceFieldDirectivePosition {
                                field: field_pos,
                                directive_name: directive.name.clone(),
                                directive_index: i,
                            });
                        ConnectDirectiveArguments::from_position_and_directive(position, directive)
                    })
            })
        })
        .chain(
            // connect on types
            schema
                .types
                .iter()
                .filter_map(|(_, ty)| ty.as_object())
                .flat_map(|ty| {
                    ty.directives
                        .iter()
                        .enumerate()
                        .filter(|(_, directive)| directive.name == *name)
                        .map(move |(i, directive)| {
                            let position =
                                ConnectorPosition::Type(ObjectTypeDefinitionDirectivePosition {
                                    type_name: ty.name.clone(),
                                    directive_name: directive.name.clone(),
                                    directive_index: i,
                                });
                            ConnectDirectiveArguments::from_position_and_directive(
                                position, directive,
                            )
                        })
                }),
        )
        .collect()
}

/// Arguments to the `@connect` directive
///
/// Refer to [ConnectSpecDefinition] for more info.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct ConnectDirectiveArguments {
    pub(crate) position: ConnectorPosition,

    /// The upstream source for shared connector configuration.
    ///
    /// Must match the `name` argument of a @source directive in this schema.
    pub(crate) source: Option<SourceName>,

    /// HTTP options for this connector
    ///
    /// Marked as optional in the GraphQL schema to allow for future transports,
    /// but is currently required.
    pub(crate) http: Option<ConnectHTTPArguments>,

    /// Fields to extract from the upstream JSON response.
    ///
    /// Uses the JSONSelection syntax to define a mapping of connector response to
    /// GraphQL schema.
    pub(crate) selection: JSONSelection,

    /// Entity resolver marker
    ///
    /// Marks this connector as a canonical resolver for an entity (uniquely
    /// identified domain model.) If true, the connector must be defined on a field
    /// of the Query type.
    pub(crate) entity: bool,

    /// Settings for the connector when it is doing a $batch entity resolver
    pub(crate) batch: Option<ConnectBatchArguments>,

    /// Configure the error mapping functionality for this connect
    pub(crate) errors: Option<ErrorsArguments>,
}

impl ConnectDirectiveArguments {
    fn from_position_and_directive(
        position: ConnectorPosition,
        value: &Node<Directive>,
    ) -> Result<Self, FederationError> {
        let args = &value.arguments;
        let directive_name = &value.name;

        // We'll have to iterate over the arg list and keep the properties by their name
        let source = SourceName::from_connect(value);
        let mut http = None;
        let mut selection = None;
        let mut entity = None;
        let mut batch = None;
        let mut errors = None;
        for arg in args {
            let arg_name = arg.name.as_str();

            if arg_name == HTTP_ARGUMENT_NAME.as_str() {
                let http_value = arg.value.as_object().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`http` field in `@{directive_name}` directive is not an object"
                    ))
                })?;

                http = Some(ConnectHTTPArguments::try_from((
                    http_value,
                    directive_name,
                ))?);
            } else if arg_name == BATCH_ARGUMENT_NAME.as_str() {
                let http_value = arg.value.as_object().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`http` field in `@{directive_name}` directive is not an object"
                    ))
                })?;

                batch = Some(ConnectBatchArguments::try_from((
                    http_value,
                    directive_name,
                ))?);
            } else if arg_name == ERRORS_ARGUMENT_NAME.as_str() {
                let http_value = arg.value.as_object().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`errors` field in `@{directive_name}` directive is not an object"
                    ))
                })?;

                let errors_value = ErrorsArguments::try_from((http_value, directive_name))?;

                errors = Some(errors_value);
            } else if arg_name == CONNECT_SELECTION_ARGUMENT_NAME.as_str() {
                let selection_value = arg.value.as_str().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`selection` field in `@{directive_name}` directive is not a string"
                    ))
                })?;
                selection = Some(
                    JSONSelection::parse(selection_value)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            } else if arg_name == CONNECT_ENTITY_ARGUMENT_NAME.as_str() {
                let entity_value = arg.value.to_bool().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`entity` field in `@{directive_name}` directive is not a boolean"
                    ))
                })?;

                entity = Some(entity_value);
            }
        }

        Ok(Self {
            position,
            source,
            http,
            selection: selection.ok_or_else(|| {
                FederationError::internal(format!(
                    "`@{directive_name}` directive is missing a selection"
                ))
            })?,
            entity: entity.unwrap_or_default(),
            batch,
            errors,
        })
    }
}

/// The HTTP arguments needed for a connect request
#[cfg_attr(test, derive(Debug))]
pub struct ConnectHTTPArguments {
    pub(crate) get: Option<String>,
    pub(crate) post: Option<String>,
    pub(crate) patch: Option<String>,
    pub(crate) put: Option<String>,
    pub(crate) delete: Option<String>,

    /// Request body
    ///
    /// Define a request body using JSONSelection. Selections can include values from
    /// field arguments using `$args.argName` and from fields on the parent type using
    /// `$this.fieldName`.
    pub(crate) body: Option<JSONSelection>,

    /// Configuration for headers to attach to the request.
    ///
    /// Overrides headers from the associated @source by name.
    pub(crate) headers: Vec<Header>,

    /// A [`JSONSelection`] that should resolve to an array of strings to append to the path.
    pub(crate) path: Option<JSONSelection>,
    /// A [`JSONSelection`] that should resolve to an object to convert to query params.
    pub(crate) query_params: Option<JSONSelection>,
}

impl TryFrom<(&ObjectNode, &Name)> for ConnectHTTPArguments {
    type Error = FederationError;

    fn try_from((values, directive_name): (&ObjectNode, &Name)) -> Result<Self, FederationError> {
        let mut get = None;
        let mut post = None;
        let mut patch = None;
        let mut put = None;
        let mut delete = None;
        let mut body = None;
        let headers: Vec<Header> = Header::from_http_arg(values, OriginatingDirective::Connect)
            .into_iter()
            .try_collect()
            .map_err(|err| FederationError::internal(err.to_string()))?;
        let mut path = None;
        let mut query_params = None;
        for (name, value) in values {
            let name = name.as_str();

            if name == CONNECT_BODY_ARGUMENT_NAME.as_str() {
                let body_value = value.as_str().ok_or_else(|| {
                    FederationError::internal(format!("`body` field in `@{directive_name}` directive's `http` field is not a string"))
                })?;
                body = Some(
                    JSONSelection::parse(body_value)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            } else if name == "GET" {
                get = Some(value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "supplied HTTP template URL in `@{directive_name}` directive's `http` field is not a string"
                )))?.to_string());
            } else if name == "POST" {
                post = Some(value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "supplied HTTP template URL in `@{directive_name}` directive's `http` field is not a string"
                )))?.to_string());
            } else if name == "PATCH" {
                patch = Some(value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "supplied HTTP template URL in `@{directive_name}` directive's `http` field is not a string"
                )))?.to_string());
            } else if name == "PUT" {
                put = Some(value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "supplied HTTP template URL in `@{directive_name}` directive's `http` field is not a string"
                )))?.to_string());
            } else if name == "DELETE" {
                delete = Some(value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "supplied HTTP template URL in `@{directive_name}` directive's `http` field is not a string"
                )))?.to_string());
            } else if name == PATH_ARGUMENT_NAME.as_str() {
                let value = value.as_str().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`{}` field in `@{directive_name}` directive's `http` field is not a string",
                        PATH_ARGUMENT_NAME
                    ))
                })?;
                path = Some(
                    JSONSelection::parse(value)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            } else if name == QUERY_PARAMS_ARGUMENT_NAME.as_str() {
                let value = value.as_str().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`{}` field in `@{directive_name}` directive's `http` field is not a string",
                        QUERY_PARAMS_ARGUMENT_NAME
                    ))
                })?;
                query_params = Some(
                    JSONSelection::parse(value)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            }
        }

        Ok(Self {
            get,
            post,
            patch,
            put,
            delete,
            body,
            headers,
            path,
            query_params,
        })
    }
}

/// Settings for the connector when it is doing a $batch entity resolver
#[derive(Clone, Copy, Debug)]
pub struct ConnectBatchArguments {
    /// Set a maximum number of requests to be batched together.
    ///
    /// Over this maximum, will be split into multiple batch requests of `max_size`.
    pub max_size: Option<usize>,
}

/// Internal representation of the object type pairs
type ObjectNode = [(Name, Node<Value>)];

impl TryFrom<(&ObjectNode, &Name)> for ConnectBatchArguments {
    type Error = FederationError;

    fn try_from((values, directive_name): (&ObjectNode, &Name)) -> Result<Self, FederationError> {
        let mut max_size = None;
        for (name, value) in values {
            let name = name.as_str();

            if name == "maxSize" {
                let max_size_int = Some(value.to_i32().ok_or_else(|| FederationError::internal(format!(
                    "supplied 'max_size' field in `@{directive_name}` directive's `batch` field is not a positive integer"
                )))?);
                // Convert the int to a usize since it is used for chunking an array later.
                // Much better to fail here than during the request lifecycle.
                max_size = max_size_int.map(|i| usize::try_from(i).map_err(|_| FederationError::internal(format!(
                    "supplied 'max_size' field in `@{directive_name}` directive's `batch` field is not a positive integer"
                )))).transpose()?;
            }
        }

        Ok(Self { max_size })
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::name;

    use super::*;
    use crate::ValidFederationSubgraphs;
    use crate::schema::FederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    static SIMPLE_SUPERGRAPH: &str = include_str!("../tests/schemas/simple.graphql");

    fn get_subgraphs(supergraph_sdl: &str) -> ValidFederationSubgraphs {
        let schema = Schema::parse(supergraph_sdl, "supergraph.graphql").unwrap();
        let supergraph_schema = FederationSchema::new(schema).unwrap();
        extract_subgraphs_from_supergraph(&supergraph_schema, Some(true)).unwrap()
    }

    #[test]
    fn it_parses_at_connect() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();
        let schema = &subgraph.schema;

        let actual_definition = schema
            .get_directive_definition(&CONNECT_DIRECTIVE_NAME_IN_SPEC)
            .unwrap()
            .get(schema.schema())
            .unwrap();

        insta::assert_snapshot!(
            actual_definition.to_string(),
            @"directive @connect(source: String, http: connect__ConnectHTTP, batch: connect__ConnectBatch, errors: connect__ConnectorErrors, selection: connect__JSONSelection!, entity: Boolean = false) repeatable on FIELD_DEFINITION | OBJECT"
        );

        let fields = schema
            .referencers()
            .get_directive(CONNECT_DIRECTIVE_NAME_IN_SPEC.as_str())
            .unwrap()
            .object_fields
            .iter()
            .map(|f| f.get(schema.schema()).unwrap().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        insta::assert_snapshot!(
            fields,
            @r###"
                users: [User] @connect(source: "json", http: {GET: "/users"}, selection: "id name")
                posts: [Post] @connect(source: "json", http: {GET: "/posts"}, selection: "id title body")
            "###
        );
    }

    #[test]
    fn it_extracts_at_connect() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();
        let schema = &subgraph.schema;

        // Extract the connects from the schema definition and map them to their `Connect` equivalent
        let connects = extract_connect_directive_arguments(schema.schema(), &name!(connect));

        insta::assert_debug_snapshot!(
            connects.unwrap(),
            @r#"
        [
            ConnectDirectiveArguments {
                position: Field(
                    ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(Query.users),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                ),
                source: Some(
                    "json",
                ),
                http: Some(
                    ConnectHTTPArguments {
                        get: Some(
                            "/users",
                        ),
                        post: None,
                        patch: None,
                        put: None,
                        delete: None,
                        body: None,
                        headers: [],
                        path: None,
                        query_params: None,
                    },
                ),
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
                entity: false,
                batch: None,
                errors: None,
            },
            ConnectDirectiveArguments {
                position: Field(
                    ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(Query.posts),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                ),
                source: Some(
                    "json",
                ),
                http: Some(
                    ConnectHTTPArguments {
                        get: Some(
                            "/posts",
                        ),
                        post: None,
                        patch: None,
                        put: None,
                        delete: None,
                        body: None,
                        headers: [],
                        path: None,
                        query_params: None,
                    },
                ),
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
                entity: false,
                batch: None,
                errors: None,
            },
        ]
        "#
        );
    }
}
