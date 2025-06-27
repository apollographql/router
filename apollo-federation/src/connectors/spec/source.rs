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
use crate::connectors::Header;
use crate::connectors::JSONSelection;
use crate::connectors::OriginatingDirective;
use crate::connectors::SourceName;
use crate::connectors::StringTemplate;
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

pub(crate) fn extract_source_directive_arguments(
    schema: &Schema,
    name: &Name,
) -> Result<Vec<SourceDirectiveArguments>, FederationError> {
    schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == *name)
        .map(|directive| SourceDirectiveArguments::from_directive(directive, &schema.sources))
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
}

impl SourceDirectiveArguments {
    fn from_directive(
        value: &Component<Directive>,
        sources: &SourceMap,
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
        for arg in args {
            let arg_name = arg.name.as_str();

            if arg_name == HTTP_ARGUMENT_NAME.as_str() {
                let http_value = arg.value.as_object().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`http` field in `@{directive_name}` directive is not an object"
                    ))
                })?;
                let http_value =
                    SourceHTTPArguments::from_directive(http_value, directive_name, sources)?;

                http = Some(http_value);
            } else if arg_name == ERRORS_ARGUMENT_NAME.as_str() {
                let http_value = arg.value.as_object().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`errors` field in `@{directive_name}` directive is not an object"
                    ))
                })?;
                let errors_value = ErrorsArguments::try_from((http_value, directive_name))?;

                errors = Some(errors_value);
            }
        }

        Ok(Self {
            name,
            http: http.ok_or_else(|| {
                FederationError::internal(format!(
                    "missing `http` field in `@{directive_name}` directive"
                ))
            })?,
            errors,
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
    fn from_directive(
        values: &[(Name, Node<Value>)],
        directive_name: &Name,
        sources: &SourceMap,
    ) -> Result<Self, FederationError> {
        let base_url = BaseUrl::parse(values, directive_name, sources)
            .map_err(|err| FederationError::internal(err.message))?;
        let headers: Vec<Header> = Header::from_http_arg(values, OriginatingDirective::Source)
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
                        "`{}` field in `@{directive_name}` directive's `http.path` field is not a string",
                        PATH_ARGUMENT_NAME
                    ))
                })?;
                path = Some(
                    JSONSelection::parse(value)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            } else if name == QUERY_PARAMS_ARGUMENT_NAME.as_str() {
                let value = value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "`{}` field in `@{directive_name}` directive's `http.queryParams` field is not a string",
                    QUERY_PARAMS_ARGUMENT_NAME
                )))?;
                query = Some(
                    JSONSelection::parse(value)
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
        let template: StringTemplate = str_value.parse().map_err(|inner: string_template::Error| {
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

        insta::assert_snapshot!(actual_definition.to_string(), @"directive @source(name: String!, http: connect__SourceHTTP, errors: connect__ConnectorErrors) repeatable on SCHEMA");

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
        let sources: Vec<_> = schema_directive_refs
            .get(schema.schema())
            .directives
            .iter()
            .filter(|directive| directive.name == SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .map(|directive| {
                SourceDirectiveArguments::from_directive(directive, &schema.schema().sources)
                    .unwrap()
            })
            .collect();

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
        let subgraphs = get_subgraphs(TEMPLATED_SOURCE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();
        let schema = &subgraph.schema;

        // Extract the sources from the schema definition and map them to their `Source` equivalent
        let sources = schema
            .referencers()
            .get_directive(&SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .unwrap();

        let schema_directive_refs = sources.schema.as_ref().unwrap();
        let sources: Result<Vec<_>, _> = schema_directive_refs
            .get(schema.schema())
            .directives
            .iter()
            .filter(|directive| directive.name == SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .map(|directive| {
                SourceDirectiveArguments::from_directive(directive, &schema.schema().sources)
            })
            .collect();
        let directive_args = sources.unwrap();

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
}
