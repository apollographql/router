#![deny(clippy::pedantic)]

use std::error::Error;
use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::SourceSpan;
use either::Either;
use http::HeaderName;
use http::header;

use crate::connectors::ConnectSpec;
use crate::connectors::JSONSelection;
use crate::connectors::header::HeaderValue;
use crate::connectors::spec::http::HEADERS_ARGUMENT_NAME;
use crate::connectors::spec::http::HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME;
use crate::connectors::spec::http::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME;
use crate::connectors::spec::http::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME;
use crate::connectors::string_template;

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub enum OriginatingDirective {
    Source,
    Connect,
}

#[derive(Clone)]
pub struct Header {
    pub name: HeaderName,
    pub(crate) name_node: Option<Node<Value>>,
    pub source: HeaderSource,
    pub(crate) source_node: Option<Node<Value>>,
    pub originating_directive: OriginatingDirective,
}

impl Header {
    /// Get a list of headers from the `headers` argument in a `@connect` or `@source` directive.
    pub(crate) fn from_http_arg(
        http_arg: &[(Name, Node<Value>)],
        originating_directive: OriginatingDirective,
        spec: ConnectSpec,
    ) -> Vec<Result<Self, HeaderParseError>> {
        let Some(headers_arg) = http_arg
            .iter()
            .find_map(|(key, value)| (*key == HEADERS_ARGUMENT_NAME).then_some(value))
        else {
            return Vec::new();
        };
        if let Some(values) = headers_arg.as_list() {
            values
                .iter()
                .map(|n| Self::from_single(n, originating_directive, spec))
                .collect()
        } else if headers_arg.as_object().is_some() {
            vec![Self::from_single(headers_arg, originating_directive, spec)]
        } else {
            vec![Err(HeaderParseError::Other {
                message: format!("`{HEADERS_ARGUMENT_NAME}` must be an object or list of objects"),
                node: headers_arg.clone(),
            })]
        }
    }

    /// Create a single `Header` directly, not from schema. Mostly useful for testing.
    pub fn from_values(
        name: HeaderName,
        source: HeaderSource,
        originating_directive: OriginatingDirective,
    ) -> Self {
        Self {
            name,
            name_node: None,
            source,
            source_node: None,
            originating_directive,
        }
    }

    /// Build a single [`Self`] from a single entry in the `headers` arg.
    fn from_single(
        node: &Node<Value>,
        originating_directive: OriginatingDirective,
        spec: ConnectSpec,
    ) -> Result<Self, HeaderParseError> {
        let mappings = node.as_object().ok_or_else(|| HeaderParseError::Other {
            message: "the HTTP header mapping is not an object".to_string(),
            node: node.clone(),
        })?;
        let name_node = mappings
            .iter()
            .find_map(|(name, value)| {
                (*name == HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME).then_some(value)
            })
            .ok_or_else(|| HeaderParseError::Other {
                message: format!("missing `{HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME}` field"),
                node: node.clone(),
            })?;
        let name = name_node
            .as_str()
            .ok_or_else(|| format!("`{HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME}` is not a string"))
            .and_then(|name_str| {
                HeaderName::try_from(name_str)
                    .map_err(|_| format!("the value `{name_str}` is an invalid HTTP header name"))
            })
            .map_err(|message| HeaderParseError::Other {
                message,
                node: name_node.clone(),
            })?;

        if RESERVED_HEADERS.contains(&name) {
            return Err(HeaderParseError::Other {
                message: format!("header '{name}' is reserved and cannot be set by a connector"),
                node: name_node.clone(),
            });
        }

        let from = mappings
            .iter()
            .find(|(name, _value)| *name == HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME);
        let value = mappings
            .iter()
            .find(|(name, _value)| *name == HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME);

        match (from, value) {
            (Some(_), None) if STATIC_HEADERS.contains(&name) => {
                Err(HeaderParseError::Other{
                    message: format!(
                        "header '{name}' can't be set with `{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}`, only with `{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}`"
                    ),
                    node: name_node.clone()
                })
            }
            (Some((_, from_node)), None) => {
                from_node.as_str()
                    .ok_or_else(|| format!("`{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}` is not a string"))
                    .and_then(|from_str| {
                        HeaderName::try_from(from_str).map_err(|_| {
                            format!("the value `{from_str}` is an invalid HTTP header name")
                        })
                    })
                    .map(|from| Self {
                        name,
                        name_node: Some(name_node.clone()),
                        source: HeaderSource::From(from),
                        source_node: Some(from_node.clone()),
                        originating_directive
                    })
                    .map_err(|message| HeaderParseError::Other{ message, node: from_node.clone()})
            }
            (None, Some((_, value_node))) => {
                value_node
                    .as_str()
                    .ok_or_else(|| HeaderParseError::Other{
                        message: format!("`{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}` field in HTTP header mapping must be a string"),
                        node: value_node.clone()
                    })
                    .and_then(|value_str| {
                        HeaderValue::parse_with_spec(
                            value_str,
                            spec,
                        )
                        .map_err(|err| HeaderParseError::ValueError {err, node: value_node.clone()})
                    })
                    .map(|value| Self {
                        name,
                        name_node: Some(name_node.clone()),
                        source: HeaderSource::Value(value),
                        source_node: Some(value_node.clone()),
                        originating_directive
                    })
            }
            (None, None) => {
                Err(HeaderParseError::Other {
                    message: format!("either `{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}` or `{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}` must be set"),
                    node: node.clone(),
                })
            },
            (Some((from_name, _)), Some((value_name, _))) => {
                Err(HeaderParseError::ConflictingArguments {
                    message: format!("`{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}` and `{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}` can't be set at the same time"),
                    from_location: from_name.location(),
                    value_location: value_name.location(),
                })
            }
        }
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl Debug for Header {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Header")
            .field("name", &self.name)
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub enum HeaderSource {
    From(HeaderName),
    Value(HeaderValue),
}

impl HeaderSource {
    pub(crate) fn expressions(&self) -> impl Iterator<Item = &JSONSelection> {
        match self {
            HeaderSource::From(_) => Either::Left(std::iter::empty()),
            HeaderSource::Value(value) => Either::Right(value.expressions().map(|e| &e.expression)),
        }
    }
}

#[derive(Debug)]
pub(crate) enum HeaderParseError {
    ValueError {
        err: string_template::Error,
        node: Node<Value>,
    },
    /// Both `value` and `from` are set
    ConflictingArguments {
        message: String,
        from_location: Option<SourceSpan>,
        value_location: Option<SourceSpan>,
    },
    Other {
        message: String,
        node: Node<Value>,
    },
}

impl Display for HeaderParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConflictingArguments { message, .. } | Self::Other { message, .. } => {
                write!(f, "{message}")
            }
            Self::ValueError { err, .. } => write!(f, "{err}"),
        }
    }
}

impl Error for HeaderParseError {}

const RESERVED_HEADERS: [HeaderName; 11] = [
    header::CONNECTION,
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
    header::CONTENT_LENGTH,
    header::CONTENT_ENCODING,
    header::ACCEPT_ENCODING,
    HeaderName::from_static("keep-alive"),
];

const STATIC_HEADERS: [HeaderName; 3] = [header::CONTENT_TYPE, header::ACCEPT, header::HOST];
