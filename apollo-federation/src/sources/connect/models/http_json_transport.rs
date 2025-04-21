use std::error::Error;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Write;
use std::iter::once;
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
use http::uri::Parts;
use http::uri::PathAndQuery;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use thiserror::Error;

use crate::error::FederationError;
use crate::sources::connect::ApplyToError;
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
use crate::sources::connect::string_template::UriString;
use crate::sources::connect::string_template::write_value;
use crate::sources::connect::variable::VariableReference;

#[derive(Clone, Debug, Default)]
pub struct HttpJsonTransport {
    pub source_url: Option<Uri>,
    pub connect_template: StringTemplate,
    pub method: HTTPMethod,
    pub headers: IndexMap<HeaderName, HeaderSource>,
    pub body: Option<JSONSelection>,
    pub source_path: Option<JSONSelection>,
    pub source_query_params: Option<JSONSelection>,
    pub connect_path: Option<JSONSelection>,
    pub connect_query_params: Option<JSONSelection>,
}

impl HttpJsonTransport {
    pub(super) fn from_directive(
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
            source_path: source.and_then(|s| s.path.clone()),
            source_query_params: source.and_then(|s| s.query_params.clone()),
            connect_path: http.path.clone(),
            connect_query_params: http.query_params.clone(),
        })
    }

    pub(super) fn label(&self) -> String {
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
            .chain(self.source_path.iter())
            .chain(self.source_query_params.iter())
            .chain(self.connect_path.iter())
            .chain(self.connect_query_params.iter())
            .flat_map(|b| {
                b.external_var_paths()
                    .into_iter()
                    .flat_map(PathSelection::variable_reference)
            })
    }

    pub fn make_uri(&self, inputs: &IndexMap<String, Value>) -> Result<Uri, MakeUriError> {
        let mut uri_parts = Parts::default();
        // TODO: Return these warnings for both Sandbox debugging and mapping playground
        let mut warnings = Vec::new();

        let connect_uri = self.connect_template.interpolate_uri(inputs)?;

        if let Some(source_uri) = &self.source_url {
            uri_parts.scheme = source_uri.scheme().cloned();
            uri_parts.authority = source_uri.authority().cloned();
        } else {
            uri_parts.scheme = connect_uri.scheme().cloned();
            uri_parts.authority = connect_uri.authority().cloned();
        }

        let mut path = UriString::new();
        if let Some(source_uri_path) = self.source_url.as_ref().map(|source_uri| source_uri.path())
        {
            path.write_without_encoding(source_uri_path)?;
        }
        if let Some(source_path) = self.source_path.as_ref() {
            warnings.extend(extend_path_from_expression(&mut path, source_path, inputs)?);
        }
        let connect_path = connect_uri.path();
        if !connect_path.is_empty() && connect_path != "/" {
            if path.ends_with('/') {
                path.write_without_encoding(connect_path.trim_start_matches('/'))?;
            } else if connect_path.starts_with('/') {
                path.write_without_encoding(connect_path)?;
            } else {
                path.write_without_encoding("/")?;
                path.write_without_encoding(connect_path)?;
            };
        }
        if let Some(connect_path) = self.connect_path.as_ref() {
            warnings.extend(extend_path_from_expression(
                &mut path,
                connect_path,
                inputs,
            )?);
        }

        let mut query = UriString::new();

        if let Some(source_uri_query) = self
            .source_url
            .as_ref()
            .and_then(|source_uri| source_uri.query())
        {
            query.write_without_encoding(source_uri_query)?;
        }
        if let Some(source_query) = self.source_query_params.as_ref() {
            warnings.extend(extend_query_from_expression(
                &mut query,
                source_query,
                inputs,
            )?);
        }
        let connect_query = connect_uri.query().unwrap_or_default();
        if !connect_query.is_empty() {
            if !query.is_empty() && !query.ends_with('&') {
                query.write_without_encoding("&")?;
            }
            query.write_without_encoding(connect_query)?;
        }
        if let Some(connect_query) = self.connect_query_params.as_ref() {
            warnings.extend(extend_query_from_expression(
                &mut query,
                connect_query,
                inputs,
            )?);
        }

        let path = path.into_string();
        let query = query.into_string();

        uri_parts.path_and_query = match (path.is_empty(), query.is_empty()) {
            (true, true) => None,
            (true, false) => Some(PathAndQuery::try_from(format!("?{query}"))?),
            (false, true) => Some(PathAndQuery::try_from(path)?),
            (false, false) => Some(PathAndQuery::try_from(format!("{path}?{query}"))?),
        };

        Uri::from_parts(uri_parts).map_err(MakeUriError::BuildMergedUri)
    }
}

/// Path segments can optionally be appended from the `http.path` inputs, each of which are a
/// [`JSONSelection`] expression expected to evaluate to an array.
fn extend_path_from_expression(
    path: &mut UriString,
    expression: &JSONSelection,
    inputs: &IndexMap<String, Value>,
) -> Result<Vec<ApplyToError>, MakeUriError> {
    let (value, warnings) = expression.apply_with_vars(&json!({}), inputs);
    let Some(value) = value else {
        return Ok(warnings);
    };
    let Value::Array(values) = value else {
        return Err(MakeUriError::PathComponents(
            "Expression did not evaluate to an array".into(),
        ));
    };
    for value in &values {
        if !path.ends_with('/') {
            path.write_trusted("/")?;
        }
        write_value(&mut *path, value)
            .map_err(|err| MakeUriError::PathComponents(err.to_string()))?;
    }
    Ok(warnings)
}

fn extend_query_from_expression(
    query: &mut UriString,
    expression: &JSONSelection,
    inputs: &IndexMap<String, Value>,
) -> Result<Vec<ApplyToError>, MakeUriError> {
    let (value, warnings) = expression.apply_with_vars(&json!({}), inputs);
    let Some(value) = value else {
        return Ok(warnings);
    };
    let Value::Object(map) = value else {
        return Err(MakeUriError::QueryParams(
            "Expression did not evaluate to an object".into(),
        ));
    };

    let all_params = map.iter().flat_map(|(key, value)| {
        if let Value::Array(values) = value {
            // If the top-level value is an array, we're going to turn that into repeated params
            Either::Left(values.iter().map(|value| (key.as_str(), value)))
        } else {
            Either::Right(once((key.as_str(), value)))
        }
    });

    for (key, value) in all_params {
        if !query.is_empty() && !query.ends_with('&') {
            query.write_trusted("&")?;
        }
        query.write_str(key)?;
        query.write_trusted("=")?;
        write_value(&mut *query, value)
            .map_err(|err| MakeUriError::QueryParams(err.to_string()))?;
    }
    Ok(warnings)
}

#[derive(Debug, Error)]
pub enum MakeUriError {
    #[error("Error building URI: {0}")]
    ParsePathAndQuery(#[from] InvalidUri),
    #[error("Error building URI: {0}")]
    BuildMergedUri(InvalidUriParts),
    #[error("Error rendering URI template: {0}")]
    TemplateGenerationError(#[from] string_template::Error),
    #[error("Internal error building URI")]
    WriteError(#[from] std::fmt::Error),
    #[error("Error building path components from expression: {0}")]
    PathComponents(String),
    #[error("Error building query parameters from queryParams: {0}")]
    QueryParams(String),
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

    use apollo_compiler::collections::IndexMap;
    use http::Uri;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use serde_json_bytes::json;

    use super::HttpJsonTransport;
    use super::*;
    use crate::sources::connect::JSONSelection;
    use crate::sources::connect::string_template::StringTemplate;

    #[test]
    fn combine_new_and_old_apis() {
        let transport = HttpJsonTransport {
            source_url: Uri::from_str("http://example.com/a?z=1").ok(),
            connect_template: "/{$args.c}?x={$args.x}".parse().unwrap(),
            source_path: JSONSelection::parse("$(['b', $args.b2])").ok(),
            connect_path: JSONSelection::parse("$(['d', $args.d2])").ok(),
            source_query_params: JSONSelection::parse("y: $args.y").ok(),
            connect_query_params: JSONSelection::parse("w: $args.w").ok(),
            ..Default::default()
        };
        let inputs = IndexMap::from_iter([(
            "$args".to_string(),
            json!({"b2": "b2", "c": "c", "d2": "d2", "y": "y", "x": "x", "w": "w"}),
        )]);
        let url = transport.make_uri(&inputs).unwrap();
        assert_eq!(warnings, vec![]);
        assert_eq!(
            url.to_string(),
            "http://example.com/a/b/b2/c/d/d2?z=1&y=y&x=x&w=w"
        );
    }

    #[test]
    fn only_new_api() {
        let transport = HttpJsonTransport {
            connect_path: JSONSelection::parse("$(['a', $args.a, ''])").ok(),
            connect_query_params: JSONSelection::parse("$args { b }").ok(),
            ..Default::default()
        };
        let inputs = IndexMap::from_iter([("$args".to_string(), json!({"a": "1", "b": "2"}))]);
        let url = transport.make_uri(&inputs).unwrap();
        assert_eq!(warnings, vec![]);
        assert_eq!(url.to_string(), "/a/1/?b=2");
    }

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

    /// When merging source and connect pieces, we sometimes have to apply encoding as we go.
    /// This double-checks that we never _double_ encode pieces.
    #[test]
    fn pieces_are_not_double_encoded() {
        let transport = HttpJsonTransport {
            source_url: Uri::from_str("http://localhost/source%20path?param=source%20param").ok(),
            connect_template: "/connect%20path?param=connect%20param".parse().unwrap(),
            ..Default::default()
        };
        assert_eq!(
            transport.make_uri(&Default::default()).unwrap(),
            "http://localhost/source%20path/connect%20path?param=source%20param&param=connect%20param"
        )
    }
}
