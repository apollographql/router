//! Validates `@source` directives

use apollo_compiler::ast::Directive;
use apollo_compiler::schema::Component;
use hashbrown::HashMap;

use super::coordinates::SourceDirectiveCoordinate;
use super::errors::ErrorsCoordinate;
use super::http::UrlProperties;
use crate::connectors::SourceName;
use crate::connectors::spec::schema::HTTP_ARGUMENT_NAME;
use crate::connectors::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::connectors::spec::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::coordinates::BaseUrlCoordinate;
use crate::connectors::validation::coordinates::HttpHeadersCoordinate;
use crate::connectors::validation::coordinates::source_http_argument_coordinate;
use crate::connectors::validation::errors::Errors;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::connectors::validation::http::headers::Headers;
use crate::connectors::validation::parse_url;

/// A `@source` directive along with any errors related to it.
pub(super) struct SourceDirective<'schema> {
    pub(crate) name: SourceName,
    directive: &'schema Component<Directive>,
    url: Option<UrlProperties<'schema>>,
    headers: Option<Headers<'schema>>,
    errors: Option<Errors<'schema>>,
    schema: &'schema SchemaInfo<'schema>,
}

impl<'schema> SourceDirective<'schema> {
    pub(super) fn find(schema: &'schema SchemaInfo) -> (Vec<Self>, Vec<Message>) {
        let source_directive_name = schema.source_directive_name();
        let mut directives = Vec::new();
        let mut messages = Vec::new();
        for directive in &schema.schema_definition.directives {
            if directive.name != *source_directive_name {
                continue;
            }
            let (directive, new_messages) = Self::from_directive(directive, schema);
            directives.extend(directive);
            messages.extend(new_messages);
        }
        let mut valid_source_names = HashMap::new();
        for directive in &directives {
            valid_source_names
                .entry(directive.name.as_str())
                .or_insert_with(Vec::new)
                .extend(directive.directive.node.line_column_range(&schema.sources))
        }
        for (name, locations) in valid_source_names {
            if locations.len() > 1 {
                messages.push(Message {
                    message: format!("Every `@{source_directive_name}({SOURCE_NAME_ARGUMENT_NAME}:)` must be unique. Found duplicate name \"{name}\"."),
                    code: Code::DuplicateSourceName,
                    locations,
                });
            }
        }
        (directives, messages)
    }

    fn from_directive(
        directive: &'schema Component<Directive>,
        schema: &'schema SchemaInfo<'schema>,
    ) -> (Option<SourceDirective<'schema>>, Vec<Message>) {
        let mut messages = Vec::new();
        let (name, name_messages) = SourceName::from_directive(directive, &schema.sources);
        messages.extend(name_messages);
        let Some(name) = name else {
            return (None, messages);
        };

        let coordinate = SourceDirectiveCoordinate {
            directive,
            name: name.clone(),
        };

        let errors = match Errors::parse(
            ErrorsCoordinate::Source {
                source: coordinate.clone(),
            },
            schema,
        ) {
            Ok(errors) => Some(errors),
            Err(errs) => {
                messages.extend(errs);
                None
            }
        };

        let Some(http_arg) = directive
            .specified_argument_by_name(&HTTP_ARGUMENT_NAME)
            .and_then(|arg| arg.as_object())
        else {
            messages.push(Message {
                code: Code::GraphQLError,
                message: format!(
                    "{coordinate} must have a `{HTTP_ARGUMENT_NAME}` argument.",
                    coordinate = source_http_argument_coordinate(&directive.name),
                ),
                locations: directive
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            });
            return (
                Some(SourceDirective {
                    name,
                    schema,
                    directive,
                    url: None,
                    headers: None,
                    errors: None,
                }),
                messages,
            );
        };

        if let Some(url_value) = http_arg
            .iter()
            .find_map(|(key, value)| (key == &SOURCE_BASE_URL_ARGUMENT_NAME).then_some(value))
        {
            if let Some(url_error) = parse_url(
                url_value,
                BaseUrlCoordinate {
                    source_directive_name: &directive.name,
                },
                schema,
            )
            .err()
            {
                messages.push(url_error);
            }
        }

        let url = match UrlProperties::parse_for_source(coordinate, schema, http_arg) {
            Ok(url_properties) => Some(url_properties),
            Err(errs) => {
                messages.extend(errs);
                None
            }
        };

        let headers = match Headers::parse(
            http_arg,
            HttpHeadersCoordinate::Source {
                directive_name: &directive.name,
            },
            schema,
        ) {
            Ok(headers) => Some(headers),
            Err(header_messages) => {
                messages.extend(header_messages);
                None
            }
        };

        (
            Some(SourceDirective {
                name,
                directive,
                url,
                headers,
                errors,
                schema,
            }),
            messages,
        )
    }

    /// Type-check each source, doing compile-time-only validations.
    ///
    /// Do not call this at runtime, as it may be prohibitively expensive.
    pub(crate) fn type_check(self) -> Vec<Message> {
        let mut messages = Vec::new();
        if let Some(errors) = self.errors {
            messages.extend(errors.type_check(self.schema).err().into_iter().flatten());
        }
        if let Some(url) = self.url {
            messages.extend(url.type_check(self.schema));
        }
        if let Some(headers) = self.headers {
            messages.extend(headers.type_check(self.schema).err().into_iter().flatten());
        }
        messages
    }
}
