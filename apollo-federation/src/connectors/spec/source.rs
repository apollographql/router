use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use http::Uri;
use itertools::Itertools;

use super::errors::ERRORS_ARGUMENT_NAME;
use super::errors::ErrorsArguments;
use crate::connectors::Header;
use crate::connectors::JSONSelection;
use crate::connectors::OriginatingDirective;
use crate::connectors::SourceName;
use crate::connectors::spec::http::HTTP_ARGUMENT_NAME;
use crate::connectors::spec::http::PATH_ARGUMENT_NAME;
use crate::connectors::spec::http::QUERY_PARAMS_ARGUMENT_NAME;
use crate::error::FederationError;

pub(crate) const SOURCE_DIRECTIVE_NAME_IN_SPEC: Name = name!("source");
pub(crate) const SOURCE_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const SOURCE_HTTP_NAME_IN_SPEC: Name = name!("SourceHTTP");
pub(crate) const SOURCE_BASE_URL_ARGUMENT_NAME: Name = name!("baseURL");

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
                let http_value = SourceHTTPArguments::try_from((http_value, directive_name))?;

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
    pub(crate) base_url: Uri,

    /// HTTP headers used when requesting resources from the upstream source.
    /// Can be overridden by name with headers in a @connect directive.
    pub(crate) headers: Vec<Header>,
    pub(crate) path: Option<JSONSelection>,
    pub(crate) query_params: Option<JSONSelection>,
}

impl TryFrom<(&[(Name, Node<Value>)], &Name)> for SourceHTTPArguments {
    type Error = FederationError;

    fn try_from(
        (values, directive_name): (&[(Name, Node<Value>)], &Name),
    ) -> Result<Self, FederationError> {
        let mut base_url = None;
        let headers: Vec<Header> = Header::from_http_arg(values, OriginatingDirective::Source)
            .into_iter()
            .try_collect()
            .map_err(|err| FederationError::internal(err.to_string()))?;
        let mut path = None;
        let mut query = None;
        for (name, value) in values {
            let name = name.as_str();

            if name == SOURCE_BASE_URL_ARGUMENT_NAME.as_str() {
                let base_url_value = value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "`baseURL` field in `@{directive_name}` directive's `http.baseURL` field is not a string"
                )))?;

                base_url = Some(base_url_value.parse().map_err(|err| {
                    FederationError::internal(format!("Invalid base URL: {}", err))
                })?);
            } else if name == PATH_ARGUMENT_NAME.as_str() {
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
            base_url: base_url.ok_or_else(|| {
                FederationError::internal(format!(
                    "missing `base_url` field in `@{directive_name}` directive's `http` argument"
                ))
            })?,
            headers,
            path,
            query_params: query,
        })
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

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
        let sources: Result<Vec<_>, _> = schema_directive_refs
            .get(schema.schema())
            .directives
            .iter()
            .filter(|directive| directive.name == SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .map(|directive| {
                SourceDirectiveArguments::from_directive(directive, &schema.schema().sources)
            })
            .collect();

        insta::assert_debug_snapshot!(
            sources.unwrap(),
            @r#"
        [
            SourceDirectiveArguments {
                name: "json",
                http: SourceHTTPArguments {
                    base_url: https://jsonplaceholder.typicode.com/,
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
                    path: None,
                    query_params: None,
                },
                errors: None,
            },
        ]
        "#
        );
    }
}
