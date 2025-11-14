use std::collections::BTreeMap;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use itertools::Itertools;

use super::errors::ERRORS_ARGUMENT_NAME;
use super::errors::ErrorsArguments;
use crate::connectors::ConnectSpec;
use crate::connectors::Header;
use crate::connectors::JSONSelection;
use crate::connectors::OriginatingDirective;
use crate::connectors::SourceName;
use crate::connectors::StringTemplate;
use crate::connectors::models::FRAGMENT_INLINE;
use crate::connectors::spec::connect::DEFAULT_CONNECT_SPEC;
use crate::connectors::spec::connect::IS_SUCCESS_ARGUMENT_NAME;
use crate::connectors::spec::connect_spec_from_schema;
use crate::connectors::spec::http::HTTP_ARGUMENT_NAME;
use crate::connectors::spec::http::PATH_ARGUMENT_NAME;
use crate::connectors::spec::http::QUERY_PARAMS_ARGUMENT_NAME;
use crate::connectors::string_template;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::error::FederationError;

pub(crate) const SOURCE_DIRECTIVE_NAME_IN_SPEC: Name = name!("source");
pub(crate) const SOURCE_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const SOURCE_HTTP_NAME_IN_SPEC: Name = name!("SourceHTTP");
pub(crate) const FRAGMENTS_NAME_ARGUMENT: Name = name!("fragments");

pub(crate) fn extract_source_directive_arguments(
    schema: &Schema,
    name: &Name,
) -> Result<Vec<SourceDirectiveArguments>, FederationError> {
    let connect_spec = connect_spec_from_schema(schema).unwrap_or(DEFAULT_CONNECT_SPEC);
    schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == *name)
        .map(|directive| {
            SourceDirectiveArguments::from_directive(directive, &schema.sources, connect_spec)
        })
        .collect()
}

/// Arguments to the `@source` directive
#[cfg_attr(test, derive(Debug))]
pub(crate) struct SourceDirectiveArguments {
    /// The friendly name of this source for use in `@connect` directives
    pub(crate) name: SourceName,

    /// Common HTTP options
    pub(crate) http: SourceHTTPArguments,

    /// Configure the error mapping functionality for this source
    pub(crate) errors: Option<ErrorsArguments>,

    /// Conditional statement to override the default success criteria for responses
    pub(crate) is_success: Option<JSONSelection>,

    /// Possible json selections fragments
    pub(crate) fragments: BTreeMap<Name, JSONSelection>,
}

impl SourceDirectiveArguments {
    fn from_directive(
        value: &Component<Directive>,
        sources: &SourceMap,
        spec: ConnectSpec,
    ) -> Result<Self, FederationError> {
        let args = &value.arguments;
        let directive_name = &value.name;

        // We'll have to iterate over the arg list and keep the properties by their name
        let name = SourceName::from_directive_permissive(value, sources).map_err(|message| {
            crate::error::SingleFederationError::InvalidGraphQL {
                message: message.message,
            }
        })?;
        let mut http = None;
        let mut errors = None;
        let mut is_success = None;
        let mut fragments = BTreeMap::new();
        for arg in args {
            let arg_name = arg.name.as_str();

            if arg_name == HTTP_ARGUMENT_NAME.as_str() {
                let http_value = arg.value.as_object().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`http` field in `@{directive_name}` directive is not an object"
                    ))
                })?;
                let http_value =
                    SourceHTTPArguments::from_directive(http_value, directive_name, sources, spec)?;

                http = Some(http_value);
            } else if arg_name == ERRORS_ARGUMENT_NAME.as_str() {
                let http_value = arg.value.as_object().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`errors` field in `@{directive_name}` directive is not an object"
                    ))
                })?;
                let errors_value = ErrorsArguments::try_from((http_value, directive_name, spec))?;

                errors = Some(errors_value);
            } else if arg_name == IS_SUCCESS_ARGUMENT_NAME.as_str() {
                let selection_value = arg.value.as_str().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`is_success` field in `@{directive_name}` directive is not a string"
                    ))
                })?;
                is_success = Some(
                    JSONSelection::parse_with_spec(selection_value, spec)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            } else if arg_name == FRAGMENTS_NAME_ARGUMENT.as_str() {
                let frag = arg.value.as_object().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`fragments` field in `@{directive_name}` directive is not an object"
                    ))
                })?;

                for (fragment, node) in frag {
                    let selection_value = node.as_str().ok_or_else(|| {
                        FederationError::internal(format!(
                            "`fragment` subfield in `@{directive_name}` directive is not a string"
                        ))
                    })?;
                    if selection_value.contains(FRAGMENT_INLINE) {
                        return Err(FederationError::internal(format!(
                            "recursive `fragments` are not allowed. `{fragment}` in `@{directive_name}` directive is recursive"
                        )));
                    }
                    let value = JSONSelection::parse_with_spec(selection_value, spec)
                        .map_err(|e| FederationError::internal(e.message))?;
                    fragments.insert(fragment.clone(), value);
                }
            }
        }

        Ok(Self {
            name,
            fragments,
            http: http.ok_or_else(|| {
                FederationError::internal(format!(
                    "missing `http` field in `@{directive_name}` directive"
                ))
            })?,
            errors,
            is_success,
        })
    }
}

/// Parsed `@source(http:)`
#[cfg_attr(test, derive(Debug))]
pub struct SourceHTTPArguments {
    /// The base URL containing all sub API endpoints
    pub(crate) base_url: BaseUrl,

    /// HTTP headers used when requesting resources from the upstream source.
    /// Can be overridden by name with headers in a @connect directive.
    pub(crate) headers: Vec<Header>,
    pub(crate) path: Option<JSONSelection>,
    pub(crate) query_params: Option<JSONSelection>,
}

impl SourceHTTPArguments {
    pub fn from_directive(
        values: &[(Name, Node<Value>)],
        directive_name: &Name,
        sources: &SourceMap,
        spec: ConnectSpec,
    ) -> Result<Self, FederationError> {
        let base_url = BaseUrl::parse(values, directive_name, sources, spec)
            .map_err(|err| FederationError::internal(err.message))?;
        let headers: Vec<Header> =
            Header::from_http_arg(values, OriginatingDirective::Source, spec)
                .into_iter()
                .try_collect()
                .map_err(|err| FederationError::internal(err.to_string()))?;
        let mut path = None;
        let mut query = None;
        for (name, value) in values {
            let name = name.as_str();

            if name == PATH_ARGUMENT_NAME.as_str() {
                let value = value.as_str().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`{PATH_ARGUMENT_NAME}` field in `@{directive_name}` directive's `http.path` field is not a string"
                    ))
                })?;
                path = Some(
                    JSONSelection::parse_with_spec(value, spec)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            } else if name == QUERY_PARAMS_ARGUMENT_NAME.as_str() {
                let value = value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "`{QUERY_PARAMS_ARGUMENT_NAME}` field in `@{directive_name}` directive's `http.queryParams` field is not a string"
                )))?;
                query = Some(
                    JSONSelection::parse_with_spec(value, spec)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            }
        }

        Ok(Self {
            base_url,
            headers,
            path,
            query_params: query,
        })
    }
}

/// The `baseURL` argument to the `@source` directive
#[derive(Debug, Clone)]
pub(crate) struct BaseUrl {
    pub(crate) template: StringTemplate,
    pub(crate) node: Node<Value>,
}

impl BaseUrl {
    pub(crate) const ARGUMENT: Name = name!("baseURL");

    pub(crate) fn parse(
        values: &[(Name, Node<Value>)],
        directive_name: &Name,
        sources: &SourceMap,
        spec: ConnectSpec,
    ) -> Result<Self, Message> {
        const BASE_URL: Name = BaseUrl::ARGUMENT;

        let value = values
            .iter()
            .find_map(|(key, value)| (key == &Self::ARGUMENT).then_some(value))
            .ok_or_else(|| Message {
                code: Code::GraphQLError,
                message: format!("`@{directive_name}` must have a `baseURL` argument."),
                locations: directive_name
                    .line_column_range(sources)
                    .into_iter()
                    .collect(),
            })?;
        let str_value = value.as_str().ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!("`@{directive_name}({BASE_URL}:)` must be a string."),
            locations: value.line_column_range(sources).into_iter().collect(),
        })?;
        let template: StringTemplate = StringTemplate::parse_with_spec(
            str_value,
            spec,
        ).map_err(|inner: string_template::Error| {
            Message {
                code: Code::InvalidUrl,
                message: format!(
                    "`@{directive_name}({BASE_URL})` value {str_value} is not a valid URL Template: {inner}."
                ),
                locations: value.line_column_range(sources).into_iter().collect(),
            }
        })?;

        Ok(Self {
            template,
            node: value.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use http::Uri;

    use super::*;
    use crate::ValidFederationSubgraphs;
    use crate::connectors::Namespace;
    use crate::schema::FederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    static SIMPLE_SUPERGRAPH: &str = include_str!("../tests/schemas/simple.graphql");
    static TEMPLATED_SOURCE_SUPERGRAPH: &str =
        include_str!("../tests/schemas/source-template.graphql");
    static IS_SUCCESS_SOURCE_SUPERGRAPH: &str =
        include_str!("../tests/schemas/is-success-source.graphql");
    static SINGLE_FRAGMENT_SOURCE_SUPERGRAPH: &str =
        include_str!("../tests/schemas/single-fragment-source.graphql");
    static RECURSIVE_FRAGMENT: &str =
        include_str!("../tests/schemas/recursive-fragment-source.graphql");

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

        insta::assert_snapshot!(actual_definition.to_string(), @"directive @source(name: String!, http: connect__SourceHTTP, errors: connect__ConnectorErrors, fragments: connect__JSONSelectionMap, isSuccess: connect__JSONSelection) repeatable on SCHEMA");

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
    fn it_extracts_at_source() {
        let sources = extract_source_directive_args(SIMPLE_SUPERGRAPH).unwrap();

        let source = sources.first().unwrap();
        assert_eq!(source.name, SourceName::cast("json"));
        assert_eq!(
            source
                .http
                .base_url
                .template
                .interpolate_uri(&Default::default())
                .unwrap()
                .0,
            Uri::from_static("https://jsonplaceholder.typicode.com/")
        );
        assert_eq!(source.http.path, None);
        assert_eq!(source.http.query_params, None);

        insta::assert_debug_snapshot!(
            source.http.headers,
            @r#"
        [
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
        ]
        "#
        );
    }

    #[test]
    fn it_parses_as_template_at_source() {
        let directive_args = extract_source_directive_args(TEMPLATED_SOURCE_SUPERGRAPH).unwrap();

        // Extract the matching templated URL from the matching source or panic if no match
        let templated_base_url = directive_args
            .iter()
            .find(|arg| arg.name == SourceName::cast("json"))
            .map(|arg| arg.http.base_url.clone())
            .unwrap()
            .template;
        assert_eq!(
            templated_base_url.to_string(),
            "https://${$config.subdomain}.typicode.com/"
        );

        // Ensure config variable exists as expected.
        templated_base_url
            .expressions()
            .flat_map(|exp| exp.expression.variable_references())
            .find(|var_ref| var_ref.namespace.namespace == Namespace::Config)
            .unwrap();
    }

    #[test]
    fn it_supports_is_success_in_source() {
        let spec_from_success_source_subgraph = ConnectSpec::V0_1;
        let sources = extract_source_directive_args(IS_SUCCESS_SOURCE_SUPERGRAPH).unwrap();
        let source = sources.first().unwrap();
        assert_eq!(source.name, SourceName::cast("json"));
        assert!(source.is_success.is_some());
        let expected =
            JSONSelection::parse_with_spec("$status->eq(202)", spec_from_success_source_subgraph)
                .unwrap();
        assert_eq!(source.is_success.as_ref().unwrap(), &expected);
    }

    #[test]
    fn it_supports_single_fragment_in_source() {
        let spec_from_success_source_subgraph = ConnectSpec::V0_4;
        let sources = extract_source_directive_args(SINGLE_FRAGMENT_SOURCE_SUPERGRAPH).unwrap();
        let source = sources.first().unwrap();
        assert_eq!(source.name, SourceName::cast("json"));
        assert_eq!(source.fragments.len(), 1);
        let expected =
            JSONSelection::parse_with_spec("id name", spec_from_success_source_subgraph).unwrap();
        assert_eq!(source.fragments["hello"], expected);
    }

    #[test]
    fn errors_on_recursive_fragment() {
        let error = extract_source_directive_args(RECURSIVE_FRAGMENT).unwrap_err();
        assert_eq!(
            format!("{:?}", error.errors().first().unwrap()),
            "Internal { message: \"recursive `fragments` are not allowed. `recur` in `@source` directive is recursive\" }"
        );
    }

    fn extract_source_directive_args(
        graph: &str,
    ) -> Result<Vec<SourceDirectiveArguments>, FederationError> {
        let subgraphs = get_subgraphs(graph);
        let subgraph = subgraphs.get("connectors").unwrap();
        let schema = &subgraph.schema;

        // Extract the sources from the schema definition and map them to their `Source` equivalent
        let sources = schema
            .referencers()
            .get_directive(&SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .unwrap();

        let schema_directive_refs = sources.schema.as_ref().unwrap();
        schema_directive_refs
            .get(schema.schema())
            .directives
            .iter()
            .filter(|directive| directive.name == SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .map(|directive| {
                let connect_spec =
                    connect_spec_from_schema(schema.schema()).unwrap_or(DEFAULT_CONNECT_SPEC);
                SourceDirectiveArguments::from_directive(
                    directive,
                    &schema.schema().sources,
                    connect_spec,
                )
            })
            .collect()
    }
}
