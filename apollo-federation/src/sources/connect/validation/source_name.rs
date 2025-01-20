use std::fmt::Display;

use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::Name;
use apollo_compiler::Node;

use super::coordinates::connect_directive_name_coordinate;
use super::coordinates::source_name_argument_coordinate;
use super::coordinates::source_name_value_coordinate;
use super::Code;
use super::DirectiveName;
use super::Message;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::sources::connect::validation::graphql::SchemaInfo;

// Adding a module to allow changing clippy lints for the regex
#[allow(clippy::expect_used)]
mod patterns {
    use once_cell::sync::Lazy;
    use regex::Regex;

    /// This is the same regular expression used for subgraph names
    pub(super) static SOURCE_NAME_REGEX: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"^[a-zA-Z][a-zA-Z0-9_-]{0,63}$")
            .expect("this regex to check source names is valid")
    });
}

pub(super) fn validate_source_name_arg(
    field_name: &Name,
    object_name: &Name,
    source_name: &Node<Argument>,
    source_names: &[SourceName],
    schema: &SchemaInfo,
) -> Vec<Message> {
    let mut messages = vec![];

    if source_names.iter().all(|name| name != &source_name.value) {
        // TODO: Pick a suggestion that's not just the first defined source
        let qualified_directive = connect_directive_name_coordinate(
            schema.connect_directive_name,
            &source_name.value,
            object_name,
            field_name,
        );
        if let Some(first_source_name) = source_names.first() {
            messages.push(Message {
                    code: Code::SourceNameMismatch,
                    message: format!(
                        "{qualified_directive} does not match any defined sources. Did you mean {first_source_name}?",
                    ),
                    locations: source_name.line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
        } else {
            messages.push(Message {
                    code: Code::NoSourcesDefined,
                    message: format!(
                        "{qualified_directive} specifies a source, but none are defined. Try adding {coordinate} to the schema.",
                        coordinate = source_name_value_coordinate(schema.source_directive_name, &source_name.value),
                    ),
                    locations: source_name.line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
        }
    }

    messages
}

/// The `name` argument of a `@source` directive.
#[derive(Clone, Debug)]
pub(super) enum SourceName {
    /// A perfectly reasonable source name.
    Valid {
        value: Node<Value>,
        directive_name: DirectiveName,
    },
    /// Contains invalid characters, so it will have to be renamed. This means certain checks
    /// (like uniqueness) should be skipped. However, we have _a_ name, so _other_ checks on the
    /// `@source` directive can continue.
    Invalid {
        value: Node<Value>,
        directive_name: DirectiveName,
    },
    /// The name was an empty string
    Empty {
        directive_name: DirectiveName,
        value: Node<Value>,
    },
    /// No `name` argument was defined
    Missing {
        directive_name: DirectiveName,
        ast_node: Node<Directive>,
    },
}

impl SourceName {
    pub(crate) fn from_directive(directive: &Component<Directive>) -> Self {
        let directive_name = directive.name.clone();
        let Some(arg) = directive
            .arguments
            .iter()
            .find(|arg| arg.name == SOURCE_NAME_ARGUMENT_NAME)
        else {
            return Self::Missing {
                directive_name,
                ast_node: directive.node.clone(),
            };
        };
        let Some(str_value) = arg.value.as_str() else {
            return Self::Invalid {
                value: arg.value.clone(),
                directive_name,
            };
        };
        if str_value.is_empty() {
            Self::Empty {
                directive_name,
                value: arg.value.clone(),
            }
        } else if patterns::SOURCE_NAME_REGEX.is_match(str_value) {
            Self::Valid {
                value: arg.value.clone(),
                directive_name,
            }
        } else {
            Self::Invalid {
                value: arg.value.clone(),
                directive_name,
            }
        }
    }

    pub(crate) fn into_value_or_error(self, sources: &SourceMap) -> Result<Node<Value>, Message> {
        match self {
            Self::Valid { value, ..} => Ok(value),
            Self::Invalid {
                value,
                directive_name,
            } => Err(Message {
                // This message is the same as Studio when trying to publish a subgraph with an invalid name
                message: format!("{coordinate} is invalid; all source names must follow pattern '^[a-zA-Z][a-zA-Z0-9_-]{{0,63}}$", coordinate = source_name_value_coordinate(&directive_name, &value)),
                code: Code::InvalidSourceName,
                locations: value.line_column_range(sources).into_iter().collect(),
            }),
            Self::Empty { directive_name, value } => {
                Err(Message {
                    code: Code::EmptySourceName,
                    message: format!("The value for {coordinate} can't be empty.", coordinate = source_name_argument_coordinate(&directive_name))   ,
                    locations: value.line_column_range(sources).into_iter().collect(),
                })
            }
            Self::Missing { directive_name, ast_node } => Err(Message {
                code: Code::GraphQLError,
                message: format!("The {coordinate} argument is required.", coordinate = source_name_argument_coordinate(&directive_name)),
                locations: ast_node.line_column_range(sources).into_iter().collect()
            }),
        }
    }
}

impl Display for SourceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid {
                value,
                directive_name,
            }
            | Self::Invalid {
                value,
                directive_name,
            } => write!(
                f,
                "`@{directive_name}({SOURCE_NAME_ARGUMENT_NAME}: {value})`"
            ),
            Self::Empty { directive_name, .. } | Self::Missing { directive_name, .. } => {
                write!(f, "unnamed `@{directive_name}`")
            }
        }
    }
}

impl PartialEq<Node<Value>> for SourceName {
    fn eq(&self, other: &Node<Value>) -> bool {
        match self {
            Self::Valid { value, .. } | Self::Invalid { value, .. } => value == other,
            Self::Empty { .. } | Self::Missing { .. } => {
                other.as_str().unwrap_or_default().is_empty()
            }
        }
    }
}
