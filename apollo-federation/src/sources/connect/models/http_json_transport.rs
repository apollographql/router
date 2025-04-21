use std::error::Error;
use std::fmt::Display;
use std::fmt::Formatter;
use std::str::FromStr;

use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::parser::SourceSpan;
use either::Either;
use http::HeaderName;
use http::Uri;
use http::uri::InvalidUri;
use http::uri::InvalidUriParts;
use http::uri::PathAndQuery;
use serde_json_bytes::Value;
use thiserror::Error;

use crate::error::FederationError;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Namespace;
use crate::sources::connect::PathSelection;
use crate::sources::connect::StringTemplate;
use crate::sources::connect::header::HeaderValue;
use crate::sources::connect::json_selection::ExternalVarPaths;
use crate::sources::connect::spec::ConnectHTTPArguments;
use crate::sources::connect::spec::SourceHTTPArguments;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME;
use crate::sources::connect::spec::versions::AllowedHeaders;
use crate::sources::connect::string_template;
use crate::sources::connect::variable::VariableReference;

#[derive(Clone, Debug, Default)]
pub struct HttpJsonTransport {
    pub source_url: Option<Uri>,
    pub connect_template: StringTemplate,
    pub method: HTTPMethod,
    pub headers: IndexMap<HeaderName, HeaderSource>,
    pub body: Option<JSONSelection>,
}

impl HttpJsonTransport {
    pub(crate) fn from_directive(
        http: &ConnectHTTPArguments,
        source: Option<&SourceHTTPArguments>,
    ) -> Result<Self, FederationError> {
        let (method, connect_url) = if let Some(url) = &http.get {
            (HTTPMethod::Get, url)
        } else if let Some(url) = &http.post {
            (HTTPMethod::Post, url)
        } else if let Some(url) = &http.patch {
            (HTTPMethod::Patch, url)
        } else if let Some(url) = &http.put {
            (HTTPMethod::Put, url)
        } else if let Some(url) = &http.delete {
            (HTTPMethod::Delete, url)
        } else {
            return Err(FederationError::internal("missing http method"));
        };

        #[allow(clippy::mutable_key_type)]
        // HeaderName is internally mutable, but we don't mutate it
        let mut headers = http.headers.clone();
        for (header_name, header_source) in
            source.map(|source| &source.headers).into_iter().flatten()
        {
            if !headers.contains_key(header_name) {
                headers.insert(header_name.clone(), header_source.clone());
            }
        }

        Ok(Self {
            source_url: source.map(|s| s.base_url.clone()),
            connect_template: connect_url.parse().map_err(|e: string_template::Error| {
                FederationError::internal(format!(
                    "could not parse URL template: {message}",
                    message = e.message
                ))
            })?,
            method,
            headers,
            body: http.body.clone(),
        })
    }

    pub(crate) fn label(&self) -> String {
        format!("http: {} {}", self.method, self.connect_template)
    }

    pub(crate) fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> {
        let url_selections = self.connect_template.expressions().map(|e| &e.expression);
        let header_selections = self
            .headers
            .iter()
            .flat_map(|(_, source)| source.expressions());
        url_selections
            .chain(header_selections)
            .chain(self.body.iter())
            .flat_map(|b| {
                b.external_var_paths()
                    .into_iter()
                    .flat_map(PathSelection::variable_reference)
            })
    }

    pub fn make_uri(&self, inputs: &IndexMap<String, Value>) -> Result<Uri, MakeUriError> {
        let connect_uri = self.connect_template.interpolate_uri(inputs)?;

        let Some(source_uri) = &self.source_url else {
            return Ok(connect_uri);
        };

        let Some(connect_path_and_query) = connect_uri.path_and_query() else {
            return Ok(source_uri.clone());
        };

        // Extract source path and query
        let source_path = source_uri.path();
        let source_query = source_uri.query().unwrap_or("");

        // Extract connect path and query
        let connect_path = connect_path_and_query.path();
        let connect_query = connect_path_and_query.query().unwrap_or("");

        // Merge paths (ensuring proper slash handling)
        let merged_path = if connect_path.is_empty() || connect_path == "/" {
            source_path.to_string()
        } else if source_path.ends_with('/') {
            format!("{}{}", source_path, connect_path.trim_start_matches('/'))
        } else if connect_path.starts_with('/') {
            format!("{}{}", source_path, connect_path)
        } else {
            format!("{}/{}", source_path, connect_path)
        };

        // Merge query parameters
        let merged_query = if source_query.is_empty() {
            connect_query.to_string()
        } else if connect_query.is_empty() {
            source_query.to_string()
        } else {
            format!("{}&{}", source_query, connect_query)
        };

        // Build the merged URI
        let mut uri_parts = source_uri.clone().into_parts();
        let merged_path_and_query = if merged_query.is_empty() {
            merged_path
        } else {
            format!("{}?{}", merged_path, merged_query)
        };

        uri_parts.path_and_query = Some(PathAndQuery::from_str(&merged_path_and_query)?);

        // Reconstruct the URI and convert to string
        Uri::from_parts(uri_parts).map_err(MakeUriError::BuildMergedUri)
    }
}

#[derive(Debug, Error)]
pub enum MakeUriError {
    #[error("Error building URI: {0}")]
    ParsePathAndQuery(#[from] InvalidUri),
    #[error("Error building URI: {0}")]
    BuildMergedUri(InvalidUriParts),
    #[error("Error rendering URI template: {0}")]
    TemplateGenerationError(#[from] string_template::Error),
}

/// The HTTP arguments needed for a connect request
#[derive(Debug, Clone, Copy, Default)]
pub enum HTTPMethod {
    #[default]
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl HTTPMethod {
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            HTTPMethod::Get => "GET",
            HTTPMethod::Post => "POST",
            HTTPMethod::Patch => "PATCH",
            HTTPMethod::Put => "PUT",
            HTTPMethod::Delete => "DELETE",
        }
    }
}

impl FromStr for HTTPMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "GET" => Ok(HTTPMethod::Get),
            "POST" => Ok(HTTPMethod::Post),
            "PATCH" => Ok(HTTPMethod::Patch),
            "PUT" => Ok(HTTPMethod::Put),
            "DELETE" => Ok(HTTPMethod::Delete),
            _ => Err(format!("Invalid HTTP method: {s}")),
        }
    }
}

impl Display for HTTPMethod {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
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

#[derive(Clone, Debug)]
pub(crate) struct Header<'a> {
    pub(crate) name: HeaderName,
    pub(crate) name_node: &'a Node<ast::Value>,
    pub(crate) source: HeaderSource,
    pub(crate) source_node: &'a Node<ast::Value>,
}

impl<'a> Header<'a> {
    /// Get a list of headers from the `headers` argument in a `@connect` or `@source` directive.
    pub(crate) fn from_headers_arg(
        node: &'a Node<ast::Value>,
        allowed_headers: &AllowedHeaders,
    ) -> Vec<Result<Self, HeaderParseError<'a>>> {
        if let Some(values) = node.as_list() {
            values
                .iter()
                .map(|v| Self::from_single(v, allowed_headers))
                .collect()
        } else if node.as_object().is_some() {
            vec![Self::from_single(node, allowed_headers)]
        } else {
            vec![Err(HeaderParseError::Other {
                message: format!("`{HEADERS_ARGUMENT_NAME}` must be an object or list of objects"),
                node,
            })]
        }
    }

    /// Build a single [`Self`] from a single entry in the `headers` arg.
    fn from_single(
        node: &'a Node<ast::Value>,
        allowed_headers: &AllowedHeaders,
    ) -> Result<Self, HeaderParseError<'a>> {
        let mappings = node.as_object().ok_or_else(|| HeaderParseError::Other {
            message: "the HTTP header mapping is not an object".to_string(),
            node,
        })?;
        let name_node = mappings
            .iter()
            .find_map(|(name, value)| {
                (*name == HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME).then_some(value)
            })
            .ok_or_else(|| HeaderParseError::Other {
                message: format!("missing `{HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME}` field"),
                node,
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
                node: name_node,
            })?;

        if allowed_headers.header_name_is_reserved(&name) {
            return Err(HeaderParseError::Other {
                message: format!("header '{name}' is reserved and cannot be set by a connector"),
                node: name_node,
            });
        }

        let from = mappings
            .iter()
            .find(|(name, _value)| *name == HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME);
        let value = mappings
            .iter()
            .find(|(name, _value)| *name == HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME);

        match (from, value) {
            (Some(_), None) if allowed_headers.header_name_allowed_static(&name) => {
                Err(HeaderParseError::Other{ message: format!(
                    "header '{name}' can't be set with `{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}`, only with `{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}`"
                ), node: name_node})
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
                        name_node,
                        source: HeaderSource::From(from),
                        source_node: from_node,
                    })
                    .map_err(|message| HeaderParseError::Other{ message, node: from_node})
            }
            (None, Some((_, value_node))) => {
                value_node
                    .as_str()
                    .ok_or_else(|| HeaderParseError::Other{ message: format!("`{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}` field in HTTP header mapping must be a string"), node: value_node})
                    .and_then(|value_str| {
                        value_str
                            .parse::<HeaderValue>()
                            .map_err(|err| HeaderParseError::ValueError {err, node: value_node})
                    })
                    .map(|value| Self {
                        name,
                        name_node,
                        source: HeaderSource::Value(value),
                        source_node: value_node,
                    })
            }
            (None, None) => {
                Err(HeaderParseError::Other {
                    message: format!("either `{HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME}` or `{HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME}` must be set"),
                    node,
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

#[derive(Debug)]
pub(crate) enum HeaderParseError<'a> {
    ValueError {
        err: string_template::Error,
        node: &'a Node<ast::Value>,
    },
    /// Both `value` and `from` are set
    ConflictingArguments {
        message: String,
        from_location: Option<SourceSpan>,
        value_location: Option<SourceSpan>,
    },
    Other {
        message: String,
        node: &'a Node<ast::Value>,
    },
}

impl Display for HeaderParseError<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConflictingArguments { message, .. } | Self::Other { message, .. } => {
                write!(f, "{}", message)
            }
            Self::ValueError { err, .. } => write!(f, "{err}"),
        }
    }
}

impl Error for HeaderParseError<'_> {}

#[cfg(test)]
mod test_make_uri {
    use std::str::FromStr;

    use pretty_assertions::assert_eq;

    use super::*;

    macro_rules! this {
        ($($value:tt)*) => {{
            let mut map = IndexMap::with_capacity_and_hasher(1, Default::default());
            map.insert("$this".to_string(), serde_json_bytes::json!({ $($value)* }));
            map
        }};
    }

    mod combining_paths {
        use pretty_assertions::assert_eq;
        use rstest::rstest;

        use super::*;
        #[rstest]
        #[case::connect_only("https://localhost:8080/v1", "/hello")]
        #[case::source_only("https://localhost:8080/v1/", "hello")]
        #[case::neither("https://localhost:8080/v1", "hello")]
        #[case::both("https://localhost:8080/v1/", "/hello")]
        fn slashes_between_source_and_connect(
            #[case] source_uri: &str,
            #[case] connect_path: &str,
        ) {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str(source_uri).ok(),
                connect_template: connect_path.parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap().to_string(),
                "https://localhost:8080/v1/hello"
            );
        }

        #[test]
        fn preserve_trailing_slash_from_connect() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("https://localhost:8080/v1").ok(),
                connect_template: "/hello/".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap().to_string(),
                "https://localhost:8080/v1/hello/"
            );
        }

        #[test]
        fn preserve_trailing_slash_from_source() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("https://localhost:8080/v1/").ok(),
                connect_template: "/".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap().to_string(),
                "https://localhost:8080/v1/"
            );
        }

        #[test]
        fn preserve_no_trailing_slash_from_source() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("https://localhost:8080/v1").ok(),
                connect_template: "/".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap().to_string(),
                "https://localhost:8080/v1"
            );
        }

        #[test]
        fn add_path_before_query_params() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("https://localhost:8080/v1?something").ok(),
                connect_template: "/hello".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&this! { "id": 42 }).unwrap().to_string(),
                "https://localhost:8080/v1/hello?something"
            );
        }

        #[test]
        fn trailing_slash_plus_query_params() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("https://localhost:8080/v1/?something").ok(),
                connect_template: "/hello/".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&this! { "id": 42 }).unwrap().to_string(),
                "https://localhost:8080/v1/hello/?something"
            );
        }

        #[test]
        fn with_merged_query_params() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("https://localhost:8080/v1?foo=bar").ok(),
                connect_template: "/hello/{$this.id}?id={$this.id}".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&this! {"id": 42 }).unwrap().to_string(),
                "https://localhost:8080/v1/hello/42?foo=bar&id=42"
            );
        }
        #[test]
        fn with_trailing_slash_in_base_plus_query_params() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("https://localhost:8080/v1/?foo=bar").ok(),
                connect_template: "/hello/{$this.id}?id={$this.id}".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&this! {"id": 42 }).unwrap().to_string(),
                "https://localhost:8080/v1/hello/42?foo=bar&id=42"
            );
        }
    }

    mod merge_query {
        use pretty_assertions::assert_eq;

        use super::*;
        #[test]
        fn source_only() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("http://localhost/users?a=b").ok(),
                connect_template: "/123".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap(),
                "http://localhost/users/123?a=b"
            );
        }

        #[test]
        fn connect_only() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("http://localhost/users").ok(),
                connect_template: "?a=b&c=d".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap(),
                "http://localhost/users?a=b&c=d"
            )
        }

        #[test]
        fn combine_from_both() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("http://localhost/users?a=b").ok(),
                connect_template: "?c=d".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap(),
                "http://localhost/users?a=b&c=d"
            )
        }

        #[test]
        fn source_and_connect_have_same_param() {
            let transport = HttpJsonTransport {
                source_url: Uri::from_str("http://localhost/users?a=b").ok(),
                connect_template: "?a=d".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap(),
                "http://localhost/users?a=b&a=d"
            )
        }
    }

    #[test]
    fn fragments_are_dropped() {
        let transport = HttpJsonTransport {
            source_url: Uri::from_str("http://localhost/source?a=b#SourceFragment").ok(),
            connect_template: "/connect?c=d#connectFragment".parse().unwrap(),
            ..Default::default()
        };
        assert_eq!(
            transport.make_uri(&Default::default()).unwrap(),
            "http://localhost/source/connect?a=b&c=d"
        )
    }
}
