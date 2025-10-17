use std::fmt;
use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::ops::Range;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

use crate::connectors::spec::connect::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::connectors::spec::source::SOURCE_NAME_ARGUMENT_NAME;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;

/// The `name` argument of a `@source` directive.
#[derive(Clone, Eq)]
pub struct SourceName {
    pub value: Arc<str>,
    node: Option<Arc<Node<Value>>>,
}

impl SourceName {
    /// Create a `SourceName`, but without checking most validations.
    ///
    /// Useful for speeding up parsing at runtime & tests.
    ///
    /// For enhanced validity checks, use [`SourceName::from_directive`]
    pub(crate) fn from_directive_permissive(
        directive: &Component<Directive>,
        sources: &SourceMap,
    ) -> Result<Self, Message> {
        Self::parse_basics(directive, sources)
    }

    /// Cast a string into a `SourceName` for when they don't come from directives
    #[must_use]
    pub fn cast(name: &str) -> Self {
        Self {
            value: Arc::from(name),
            node: None,
        }
    }
    fn parse_basics(
        directive: &Component<Directive>,
        sources: &SourceMap,
    ) -> Result<Self, Message> {
        let coordinate = NameCoordinate {
            directive_name: &directive.name,
            value: None,
        };
        let Some(arg) = directive
            .arguments
            .iter()
            .find(|arg| arg.name == SOURCE_NAME_ARGUMENT_NAME)
        else {
            return Err(Message {
                code: Code::GraphQLError,
                message: format!("The {coordinate} argument is required.",),
                locations: directive.line_column_range(sources).into_iter().collect(),
            });
        };
        let node = &arg.value;
        let Some(str_value) = node.as_str() else {
            return Err(Message {
                message: format!("{coordinate} is invalid; source names must be strings.",),
                code: Code::InvalidSourceName,
                locations: node.line_column_range(sources).into_iter().collect(),
            });
        };
        Ok(Self {
            value: Arc::from(str_value),
            node: Some(Arc::new(node.clone())),
        })
    }

    pub(crate) fn from_connect(directive: &Node<Directive>) -> Option<Self> {
        let arg = directive
            .arguments
            .iter()
            .find(|arg| arg.name == CONNECT_SOURCE_ARGUMENT_NAME)?;
        let node = &arg.value;
        let str_value = node.as_str()?;
        Some(Self {
            value: Arc::from(str_value),
            node: Some(Arc::new(node.clone())),
        })
    }
    pub(crate) fn from_directive(
        directive: &Component<Directive>,
        sources: &SourceMap,
    ) -> (Option<Self>, Option<Message>) {
        let name = match Self::parse_basics(directive, sources) {
            Ok(name) => name,
            Err(message) => return (None, Some(message)),
        };

        let coordinate = NameCoordinate {
            directive_name: &directive.name,
            value: Some(name.value.clone()),
        };

        let Some(first_char) = name.value.chars().next() else {
            let locations = name.locations(sources);
            return (
                Some(name),
                Some(Message {
                    code: Code::EmptySourceName,
                    message: format!("The value for {coordinate} can't be empty.",),
                    locations,
                }),
            );
        };
        let message = if !first_char.is_ascii_alphabetic() {
            Some(Message {
                message: format!(
                    "{coordinate} is invalid; source names must start with an ASCII letter (a-z or A-Z)",
                ),
                code: Code::InvalidSourceName,
                locations: name.locations(sources),
            })
        } else if name.value.len() > 64 {
            Some(Message {
                message: format!(
                    "{coordinate} is invalid; source names must be 64 characters or fewer",
                ),
                code: Code::InvalidSourceName,
                locations: name.locations(sources),
            })
        } else {
            name.value
                .chars()
                .find(|c| !c.is_ascii_alphanumeric() && *c != '_' && *c != '-').map(|unacceptable| Message {
                message: format!(
                    "{coordinate} can't contain `{unacceptable}`; only ASCII letters, numbers, underscores, or hyphens are allowed",
                ),
                code: Code::InvalidSourceName,
                locations: name.locations(sources),
            })
        };
        (Some(name), message)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    pub(crate) fn locations(&self, sources: &SourceMap) -> Vec<Range<LineColumn>> {
        self.node
            .as_ref()
            .map(|node| node.line_column_range(sources))
            .into_iter()
            .flatten()
            .collect()
    }
}

impl Display for SourceName {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Debug for SourceName {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Debug::fmt(&self.as_str(), f)
    }
}

impl PartialEq<Node<Value>> for SourceName {
    fn eq(&self, other: &Node<Value>) -> bool {
        other
            .as_str()
            .is_some_and(|value| value == self.value.as_ref())
    }
}

impl PartialEq<SourceName> for SourceName {
    fn eq(&self, other: &SourceName) -> bool {
        self.value == other.value
    }
}

impl Hash for SourceName {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl Serialize for SourceName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for SourceName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Arc::deserialize(deserializer)?;
        Ok(Self { value, node: None })
    }
}

struct NameCoordinate<'schema> {
    directive_name: &'schema Name,
    value: Option<Arc<str>>,
}

impl Display for NameCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        if let Some(value) = &self.value {
            write!(
                f,
                "`@{}({SOURCE_NAME_ARGUMENT_NAME}: \"{value}\")`",
                self.directive_name,
            )
        } else {
            write!(
                f,
                "`@{}({SOURCE_NAME_ARGUMENT_NAME}:)`",
                self.directive_name
            )
        }
    }
}
