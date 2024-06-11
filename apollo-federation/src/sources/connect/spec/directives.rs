use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Name;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::Node;
use indexmap::map::Entry::Occupied;
use indexmap::map::Entry::Vacant;
use indexmap::IndexMap;

use super::schema::ConnectDirectiveArguments;
use super::schema::ConnectHTTPArguments;
use super::schema::HTTPHeaderMappings;
use super::schema::HTTPHeaderOption;
use super::schema::SourceDirectiveArguments;
use super::schema::SourceHTTPArguments;
use super::schema::CONNECT_BODY_ARGUMENT_NAME;
use super::schema::CONNECT_ENTITY_ARGUMENT_NAME;
use super::schema::CONNECT_HEADERS_ARGUMENT_NAME;
use super::schema::CONNECT_SELECTION_ARGUMENT_NAME;
use super::schema::HTTP_HEADER_MAPPING_AS_ARGUMENT_NAME;
use super::schema::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME;
use super::schema::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME;
use super::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use super::schema::SOURCE_HEADERS_ARGUMENT_NAME;
use super::schema::SOURCE_HTTP_ARGUMENT_NAME;
use super::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::error::FederationError;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
use crate::schema::FederationSchema;
use crate::sources::connect::json_selection::JSONSelection;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;

pub(crate) fn extract_source_directive_arguments(
    schema: &FederationSchema,
    name: &Name,
) -> Result<Vec<SourceDirectiveArguments>, FederationError> {
    let Ok(directive_refs) = schema.referencers().get_directive(name) else {
        return Ok(vec![]);
    };

    let schema_directive_pos = directive_refs.schema.as_ref().unwrap();

    let schema_def = schema_directive_pos.get(schema.schema());

    schema_def
        .directives
        .iter()
        .filter(|directive| directive.name == *name)
        .map(SourceDirectiveArguments::try_from)
        .collect()
}

pub(crate) fn extract_connect_directive_arguments(
    schema: &FederationSchema,
    name: &Name,
) -> Result<Vec<ConnectDirectiveArguments>, FederationError> {
    let Ok(directive_refs) = schema.referencers().get_directive(name) else {
        return Ok(vec![]);
    };

    directive_refs
        .object_fields
        .iter()
        .flat_map(|pos| {
            let object_field = pos.get(schema.schema()).unwrap();
            object_field
                .directives
                .iter()
                .enumerate()
                .filter(|(_, directive)| directive.name == *name)
                .map(move |(i, directive)| {
                    let directive_pos = ObjectOrInterfaceFieldDirectivePosition {
                        field: ObjectOrInterfaceFieldDefinitionPosition::Object(pos.clone()),
                        directive_name: directive.name.clone(),
                        directive_index: i,
                    };
                    ConnectDirectiveArguments::from_position_and_directive(directive_pos, directive)
                })
        })
        .collect()
}

/// Internal representation of the object type pairs
type ObjectNode = [(Name, Node<Value>)];

impl TryFrom<&Component<Directive>> for SourceDirectiveArguments {
    type Error = FederationError;

    // TODO: This currently does not handle validation
    fn try_from(value: &Component<Directive>) -> Result<Self, Self::Error> {
        let args = &value.arguments;

        // We'll have to iterate over the arg list and keep the properties by their name
        let mut name = None;
        let mut http = None;
        for arg in args {
            let arg_name = arg.name.as_str();

            if arg_name == SOURCE_NAME_ARGUMENT_NAME.as_str() {
                name = Some(
                    arg.value
                        .as_node_str()
                        .expect("`name` field in `@source` directive is not a string")
                        .clone(),
                );
            } else if arg_name == SOURCE_HTTP_ARGUMENT_NAME.as_str() {
                let http_value = arg
                    .value
                    .as_object()
                    .expect("`http` field in `@source` directive is not an object");
                let http_value = SourceHTTPArguments::try_from(http_value)?;

                http = Some(http_value);
            } else {
                unreachable!("unknown argument in `@source` directive: {arg_name}");
            }
        }

        // TODO: The compiler should catch missing fields here, right?
        Ok(Self {
            name: name.expect("missing `name` field in `@source` directive"),
            http: http.expect("missing `http` field in `@source` directive"),
        })
    }
}

impl TryFrom<&ObjectNode> for SourceHTTPArguments {
    type Error = FederationError;

    // TODO: This does not currently do validation
    fn try_from(values: &ObjectNode) -> Result<Self, Self::Error> {
        // Iterate over all of the argument pairs and fill in what we need
        let mut base_url = None;
        let mut headers = None;
        for (name, value) in values {
            let name = name.as_str();

            if name == SOURCE_BASE_URL_ARGUMENT_NAME.as_str() {
                let base_url_value = value.as_node_str().expect(
                    "`baseURL` field in `@source` directive's `http` field is not a string",
                );

                base_url = Some(base_url_value.clone());
            } else if name == SOURCE_HEADERS_ARGUMENT_NAME.as_str() {
                // TODO: handle a single object since the language spec allows it
                headers = value
                    .as_list()
                    .map(HTTPHeaderMappings::try_from)
                    .transpose()?;
            } else {
                unreachable!("unknown argument in `@source` directive's `http` field: {name}");
            }
        }

        Ok(Self {
            base_url: base_url
                .expect("missing `base_url` field in `@source` directive's `http` argument"),
            headers: headers.unwrap_or_default(),
        })
    }
}

/// Converts a list of (name, value) pairs into a map of HTTP headers. Using
/// the same name twice is an error.
// TODO: The following does not do any formal validation
impl TryFrom<&[Node<Value>]> for HTTPHeaderMappings {
    type Error = FederationError;

    fn try_from(values: &[Node<Value>]) -> Result<Self, Self::Error> {
        let mut map = IndexMap::new();

        for value in values {
            // The mapping should be an object
            let mappings = value.as_object().unwrap();

            // Extract the known configuration options
            let mut name = None;
            let mut option = None;
            for (field, mapping) in mappings {
                let field = field.as_str();

                if field == HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME.as_str() {
                    let name_value = mapping
                        .as_node_str()
                        .expect("`name` field in HTTP header mapping is not a string");

                    name = Some(name_value.clone());
                } else if field == HTTP_HEADER_MAPPING_AS_ARGUMENT_NAME.as_str() {
                    let as_value = mapping
                        .as_node_str()
                        .expect("`as` field in HTTP header mapping is not a string");

                    option = Some(HTTPHeaderOption::As(as_value.clone()));
                } else if field == HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME.as_str() {
                    let value_values = if let Some(list) = mapping.as_list() {
                        list.iter()
                            .map(|item| {
                                item.as_node_str()
                                    .expect("`value` field in HTTP header mapping is not a string")
                                    .clone()
                            })
                            .collect()
                    } else if let Some(item) = mapping.as_node_str() {
                        vec![item.clone()]
                    } else {
                        unreachable!(
                            "`value` field in HTTP header mapping is not a string or list of strings"
                        );
                    };

                    option = Some(HTTPHeaderOption::Value(value_values));
                } else {
                    unreachable!("unknown argument for HTTP header mapping: {field}")
                }
            }

            let name = name.expect("missing `name` field in HTTP header mapping");
            match map.entry(name.clone()) {
                Occupied(_) => {
                    return Err(FederationError::internal(format!(
                        "duplicate HTTP header mapping for `{}`",
                        &name
                    )));
                }
                Vacant(entry) => {
                    entry.insert(option);
                }
            }
        }

        Ok(Self(map))
    }
}

impl ConnectDirectiveArguments {
    fn from_position_and_directive(
        position: ObjectOrInterfaceFieldDirectivePosition,
        value: &Node<Directive>,
    ) -> Result<Self, FederationError> {
        let args = &value.arguments;

        // We'll have to iterate over the arg list and keep the properties by their name
        let mut source = None;
        let mut http = None;
        let mut selection = None;
        let mut entity = None;
        for arg in args {
            let arg_name = arg.name.as_str();

            if arg_name == CONNECT_SOURCE_ARGUMENT_NAME.as_str() {
                let source_value = arg
                    .value
                    .as_node_str()
                    .expect("`source` field in `@source` directive is not a string");

                source = Some(source_value.clone());
            } else if arg_name == CONNECT_HTTP_ARGUMENT_NAME.as_str() {
                let http_value = arg
                    .value
                    .as_object()
                    .expect("`http` field in `@connect` directive is not an object");
                http = Some(ConnectHTTPArguments::try_from(http_value)?);
            } else if arg_name == CONNECT_SELECTION_ARGUMENT_NAME.as_str() {
                let selection_value = arg
                    .value
                    .as_node_str()
                    .expect("`selection` field in `@connect` directive is not a string");
                let (remainder, selection_value) =
                    JSONSelection::parse(selection_value.as_str()).expect("invalid JSON selection");
                if !remainder.is_empty() {
                    panic!("`selection` field in `@connect` directive could not be fully parsed: the following was left over: {remainder}");
                }

                selection = Some(selection_value);
            } else if arg_name == CONNECT_ENTITY_ARGUMENT_NAME.as_str() {
                let entity_value = arg
                    .value
                    .to_bool()
                    .expect("`entity` field in `@connect` directive is not a boolean");

                entity = Some(entity_value);
            } else {
                unreachable!("unknown argument in `@connect` directive: {arg_name}");
            }
        }

        Ok(Self {
            position,
            source,
            http,
            selection: selection.expect("`@connect` directive is missing a selection"),
            entity: entity.unwrap_or_default(),
        })
    }
}

impl TryFrom<&ObjectNode> for ConnectHTTPArguments {
    type Error = FederationError;

    fn try_from(values: &ObjectNode) -> Result<Self, Self::Error> {
        let mut get = None;
        let mut post = None;
        let mut patch = None;
        let mut put = None;
        let mut delete = None;
        let mut body = None;
        let mut headers = None;
        for (name, value) in values {
            let name = name.as_str();

            if name == CONNECT_BODY_ARGUMENT_NAME.as_str() {
                let body_value = value
                    .as_node_str()
                    .expect("`body` field in `@connect` directive's `http` field is not a string");
                let (remainder, body_value) =
                    JSONSelection::parse(body_value.as_str()).expect("invalid JSON selection");
                if !remainder.is_empty() {
                    panic!("`body` field in `@connect` directive could not be fully parsed: the following was left over: {remainder}");
                }

                body = Some(body_value);
            } else if name == CONNECT_HEADERS_ARGUMENT_NAME.as_str() {
                // TODO: handle a single object since the language spec allows it
                headers = value
                    .as_list()
                    .map(HTTPHeaderMappings::try_from)
                    .transpose()?;
            } else if name == "GET" {
                get = Some(value.as_str().expect(
                    "supplied HTTP template URL in `@connect` directive's `http` field is not a string",
                ).to_string());
            } else if name == "POST" {
                post = Some(value.as_str().expect(
                    "supplied HTTP template URL in `@connect` directive's `http` field is not a string",
                ).to_string());
            } else if name == "PATCH" {
                patch = Some(value.as_str().expect(
                    "supplied HTTP template URL in `@connect` directive's `http` field is not a string",
                ).to_string());
            } else if name == "PUT" {
                put = Some(value.as_str().expect(
                    "supplied HTTP template URL in `@connect` directive's `http` field is not a string",
                ).to_string());
            } else if name == "DELETE" {
                delete = Some(value.as_str().expect(
                    "supplied HTTP template URL in `@connect` directive's `http` field is not a string",
                ).to_string());
            }
        }

        Ok(Self {
            get,
            post,
            patch,
            put,
            delete,
            body,
            headers: headers.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::Schema;

    use crate::query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph;
    use crate::schema::FederationSchema;
    use crate::sources::connect::spec::schema::SourceDirectiveArguments;
    use crate::sources::connect::spec::schema::CONNECT_DIRECTIVE_NAME_IN_SPEC;
    use crate::sources::connect::spec::schema::SOURCE_DIRECTIVE_NAME_IN_SPEC;
    use crate::ValidFederationSubgraphs;

    static SIMPLE_SUPERGRAPH: &str = include_str!("../tests/schemas/simple.graphql");

    fn get_subgraphs(supergraph_sdl: &str) -> ValidFederationSubgraphs {
        let schema = Schema::parse(supergraph_sdl, "supergraph.graphql").unwrap();
        let supergraph_schema = FederationSchema::new(schema).unwrap();
        extract_subgraphs_from_supergraph(&supergraph_schema, Some(true)).unwrap()
    }

    #[test]
    fn it_parses_at_source() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();

        let actual_definition = subgraph
            .schema
            .get_directive_definition(&SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .unwrap()
            .get(subgraph.schema.schema())
            .unwrap();

        insta::assert_snapshot!(actual_definition.to_string(), @"directive @source(name: String!, http: connect__SourceHTTP) repeatable on SCHEMA");

        insta::assert_debug_snapshot!(
            subgraph.schema
                .referencers()
                .get_directive(SOURCE_DIRECTIVE_NAME_IN_SPEC.as_str())
                .unwrap(),
            @r###"
                DirectiveReferencers {
                    schema: Some(
                        SchemaDefinitionPosition,
                    ),
                    scalar_types: {},
                    object_types: {},
                    object_fields: {},
                    object_field_arguments: {},
                    interface_types: {},
                    interface_fields: {},
                    interface_field_arguments: {},
                    union_types: {},
                    enum_types: {},
                    enum_values: {},
                    input_object_types: {},
                    input_object_fields: {},
                    directive_arguments: {},
                }
            "###
        );
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
            @"directive @connect(source: String, http: connect__ConnectHTTP, selection: connect__JSONSelection!, entity: Boolean = false) repeatable on FIELD_DEFINITION"
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
    fn it_extracts_at_source() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();
        let schema = &subgraph.schema;

        // Try to extract the source information from the valid schema
        // TODO: This should probably be handled by the rest of the stack
        let sources = schema
            .referencers()
            .get_directive(&SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .unwrap();

        // Extract the sources from the schema definition and map them to their `Source` equivalent
        let schema_directive_refs = sources.schema.as_ref().unwrap();
        let sources: Result<Vec<_>, _> = schema_directive_refs
            .get(schema.schema())
            .directives
            .iter()
            .filter(|directive| directive.name == SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .map(SourceDirectiveArguments::try_from)
            .collect();

        insta::assert_debug_snapshot!(
            sources.unwrap(),
            @r###"
        [
            SourceDirectiveArguments {
                name: "json",
                http: SourceHTTPArguments {
                    base_url: "https://jsonplaceholder.typicode.com/",
                    headers: HTTPHeaderMappings(
                        {
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
                    ),
                },
            },
        ]
        "###
        );
    }

    #[test]
    fn it_extracts_at_connect() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();
        let schema = &subgraph.schema;

        // Extract the connects from the schema definition and map them to their `Connect` equivalent
        let connects = super::extract_connect_directive_arguments(schema, &name!(connect));

        insta::assert_debug_snapshot!(
            connects.unwrap(),
            @r###"
        [
            ConnectDirectiveArguments {
                position: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(Query.users),
                    directive_name: "connect",
                    directive_index: 0,
                },
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
                        headers: HTTPHeaderMappings(
                            {},
                        ),
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
            },
            ConnectDirectiveArguments {
                position: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(Query.posts),
                    directive_name: "connect",
                    directive_index: 0,
                },
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
                        headers: HTTPHeaderMappings(
                            {},
                        ),
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
            },
        ]
        "###
        );
    }
}
