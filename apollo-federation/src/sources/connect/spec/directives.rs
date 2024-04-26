use apollo_compiler::{
    ast::{Directive, Name, Value},
    schema::Component,
    Node,
};
use indexmap::{
    map::Entry::{Occupied, Vacant},
    IndexMap,
};

use crate::{
    error::FederationError,
    sources::connect::{
        selection_parser::Selection,
        spec::schema::{CONNECT_HTTP_ARGUMENT_NAME, CONNECT_SOURCE_ARGUMENT_NAME},
        url_path_template::URLPathTemplate,
    },
};

use super::schema::{
    ConnectDirectiveArguments, ConnectHTTPArguments, Connector, HTTPArguments, HTTPHeaderMappings,
    HTTPHeaderOption, HTTPMethod, SourceDirectiveArguments, CONNECT_BODY_ARGUMENT_NAME,
    CONNECT_ENTITY_ARGUMENT_NAME, CONNECT_HEADERS_ARGUMENT_NAME, CONNECT_SELECTION_ARGUMENT_NAME,
    HTTP_HEADER_MAPPING_AS_ARGUMENT_NAME, HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME,
    HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME, SOURCE_BASE_URL_ARGUMENT_NAME,
    SOURCE_HEADERS_ARGUMENT_NAME, SOURCE_HTTP_ARGUMENT_NAME, SOURCE_NAME_ARGUMENT_NAME,
};

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
                let http_value = HTTPArguments::try_from(http_value)?;

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

impl TryFrom<&ObjectNode> for HTTPArguments {
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

impl TryFrom<&Node<Directive>> for ConnectDirectiveArguments {
    type Error = FederationError;

    // TODO: This does not currently do validation
    fn try_from(value: &Node<Directive>) -> Result<Self, Self::Error> {
        let args = &value.arguments;

        // We'll have to iterate over the arg list and keep the properties by their name
        let mut source = None;
        let mut connector = None;
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
                // Make sure that we haven't seen a connector already
                if connector.is_some() {
                    panic!("`@source` directive has multiple connectors specified");
                }

                let http_value = arg
                    .value
                    .as_object()
                    .expect("`http` field in `@connect` directive is not an object");
                let http_value = ConnectHTTPArguments::try_from(http_value)?;

                connector = Some(Connector::Http(http_value));
            } else if arg_name == CONNECT_SELECTION_ARGUMENT_NAME.as_str() {
                let selection_value = arg
                    .value
                    .as_node_str()
                    .expect("`selection` field in `@connect` directive is not a string");
                let (remainder, selection_value) =
                    Selection::parse(selection_value.as_str()).expect("invalid JSON selection");
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
            source,
            connector: connector.expect("`@connect` directive is missing a connector"),
            selection,
            entity: entity.unwrap_or_default(),
        })
    }
}

impl TryFrom<&ObjectNode> for ConnectHTTPArguments {
    type Error = FederationError;

    fn try_from(values: &ObjectNode) -> Result<Self, Self::Error> {
        // Iterate over all of the argument pairs and fill in what we need
        let mut method_and_url = None;
        let mut body = None;
        let mut headers = None;
        for (name, value) in values {
            let name = name.as_str();

            if name == CONNECT_BODY_ARGUMENT_NAME.as_str() {
                let body_value = value
                    .as_node_str()
                    .expect("`body` field in `@connect` directive's `http` field is not a string");
                let (remainder, body_value) =
                    Selection::parse(body_value.as_str()).expect("invalid JSON selection");
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
            } else {
                // We need to (potentially) map any arbitrary keys to an HTTP verb
                let method = match name {
                    "GET" => HTTPMethod::Get,
                    "POST" => HTTPMethod::Post,
                    "PATCH" => HTTPMethod::Patch,
                    "PUT" => HTTPMethod::Put,
                    "DELETE" => HTTPMethod::Delete,
                    _ => unreachable!(
                        "unknown argument in `@source` directive's `http` field: {name}"
                    ),
                };

                // If we have a valid verb, then we need to grab (and parse) the URL template for it
                let url = value.as_str().expect("supplied HTTP template URL in `@connect` directive's `http` field is not a string");
                let url = URLPathTemplate::parse(url).expect("supplied HTTP template URL in `@connect` directive's `http` field is not valid");

                method_and_url = Some((method, url));
            }
        }

        let (method, url) = method_and_url
            .expect("missing an HTTP verb and endpoint in `@connect` directive's `http` field");
        Ok(Self {
            method,
            url,
            body,
            headers: headers.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

    use crate::{
        query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph,
        schema::FederationSchema,
        sources::connect::spec::schema::{
            ConnectDirectiveArguments, SourceDirectiveArguments, CONNECT_DIRECTIVE_NAME_IN_SPEC,
            SOURCE_DIRECTIVE_NAME_IN_SPEC,
        },
        ValidFederationSubgraphs,
    };

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
                    http: HTTPArguments {
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

        // Try to extract the source information from the valid schema
        // TODO: This should probably be handled by the rest of the stack
        let connects = schema
            .referencers()
            .get_directive(&CONNECT_DIRECTIVE_NAME_IN_SPEC)
            .unwrap();

        // Extract the connects from the schema definition and map them to their `Connect` equivalent
        // TODO: We can safely assume that a connect can only be on object fields, right?
        let connects: Result<Vec<_>, _> = connects
            .object_fields
            .iter()
            .flat_map(|field| field.get(schema.schema()).unwrap().directives.iter())
            .map(ConnectDirectiveArguments::try_from)
            .collect();

        insta::assert_debug_snapshot!(
            connects.unwrap(),
            @r###"
        [
            ConnectDirectiveArguments {
                source: Some(
                    "json",
                ),
                connector: Http(
                    ConnectHTTPArguments {
                        method: Get,
                        url: URLPathTemplate {
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
                        body: None,
                        headers: HTTPHeaderMappings(
                            {},
                        ),
                    },
                ),
                selection: Some(
                    Named(
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
                ),
                entity: false,
            },
            ConnectDirectiveArguments {
                source: Some(
                    "json",
                ),
                connector: Http(
                    ConnectHTTPArguments {
                        method: Get,
                        url: URLPathTemplate {
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
                        body: None,
                        headers: HTTPHeaderMappings(
                            {},
                        ),
                    },
                ),
                selection: Some(
                    Named(
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
                ),
                entity: false,
            },
        ]
        "###
        );
    }
}
