//! Validates `@source` directives

use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;

use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Value;
use hashbrown::HashMap;

use super::coordinates::SourceDirectiveCoordinate;
use super::coordinates::source_name_argument_coordinate;
use super::coordinates::source_name_value_coordinate;
use super::errors::ErrorsCoordinate;
use super::http::UrlProperties;
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
    pub(crate) name: SourceName<'schema>,
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
                .entry(&directive.name.0)
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

        let coordinate = SourceDirectiveCoordinate {
            directive,
            name: name.unwrap_or_default(),
        };

        let errors = match Errors::parse(ErrorsCoordinate::Source { source: coordinate }, schema) {
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
                name.map(|name| SourceDirective {
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
            name.map(|name| SourceDirective {
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

/// The `name` argument of a `@source` directive.
#[derive(Clone, Debug, Copy, Default)]
pub(super) struct SourceName<'schema>(&'schema str);

impl<'schema> SourceName<'schema> {
    pub(crate) fn from_directive(
        directive: &'schema Component<Directive>,
        sources: &SourceMap,
    ) -> (Option<Self>, Option<Message>) {
        let directive_name = directive.name.clone();
        let Some(arg) = directive
            .arguments
            .iter()
            .find(|arg| arg.name == SOURCE_NAME_ARGUMENT_NAME)
        else {
            return (
                None,
                Some(Message {
                    code: Code::GraphQLError,
                    message: format!(
                        "The {coordinate} argument is required.",
                        coordinate = source_name_argument_coordinate(&directive_name)
                    ),
                    locations: directive.line_column_range(sources).into_iter().collect(),
                }),
            );
        };
        let value = &arg.value;
        let Some(str_value) = value.as_str() else {
            return (
                None,
                Some(Message {
                    message: format!(
                        "{coordinate} is invalid; source names must be strings.",
                        coordinate = source_name_value_coordinate(&directive_name, value)
                    ),
                    code: Code::InvalidSourceName,
                    locations: value.line_column_range(sources).into_iter().collect(),
                }),
            );
        };
        let name = Some(Self(str_value));
        let Some(first_char) = str_value.chars().next() else {
            return (
                name,
                Some(Message {
                    code: Code::EmptySourceName,
                    message: format!(
                        "The value for {coordinate} can't be empty.",
                        coordinate = source_name_argument_coordinate(&directive_name)
                    ),
                    locations: arg.value.line_column_range(sources).into_iter().collect(),
                }),
            );
        };
        let message = if !first_char.is_ascii_alphabetic() {
            Some(Message {
                message: format!(
                    "{coordinate} is invalid; source names must start with an ASCII letter (a-z or A-Z)",
                    coordinate = source_name_value_coordinate(&directive_name, value)
                ),
                code: Code::InvalidSourceName,
                locations: value.line_column_range(sources).into_iter().collect(),
            })
        } else if str_value.len() > 64 {
            Some(Message {
                message: format!(
                    "{coordinate} is invalid; source names must be 64 characters or fewer",
                    coordinate = source_name_value_coordinate(&directive_name, value)
                ),
                code: Code::InvalidSourceName,
                locations: value.line_column_range(sources).into_iter().collect(),
            })
        } else {
            str_value
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && *c != '_' && *c != '-').map(|unacceptable| Message {
                message: format!(
                    "{coordinate} can't contain `{unacceptable}`; only ASCII letters, numbers, underscores, or hyphens are allowed",
                    coordinate = source_name_value_coordinate(&directive_name, value)
                ),
                code: Code::InvalidSourceName,
                locations: value.line_column_range(sources).into_iter().collect(),
            })
        };
        (name, message)
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0
    }
}

impl Display for SourceName<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl PartialEq<Node<Value>> for SourceName<'_> {
    fn eq(&self, other: &Node<Value>) -> bool {
        other.as_str().is_some_and(|value| value == self.0)
    }
}
