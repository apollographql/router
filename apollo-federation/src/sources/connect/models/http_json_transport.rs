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
use itertools::Itertools;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use url::Url;

use super::super::JSONSelection;
use super::super::PathSelection;
use super::super::URLTemplate;
use super::super::json_selection::ExternalVarPaths;
use super::super::spec::ConnectHTTPArguments;
use super::super::spec::SourceHTTPArguments;
use super::super::spec::versions::AllowedHeaders;
use super::super::string_template;
use super::super::variable::Namespace;
use super::super::variable::VariableReference;
use crate::error::FederationError;
use crate::sources::connect::ApplyToError;
use crate::sources::connect::header::HeaderValue;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME;

#[derive(Clone, Debug, Default)]
pub struct HttpJsonTransport {
    pub source_url: Option<Url>,
    pub connect_template: Option<URLTemplate>,
    pub method: Option<HTTPMethod>,
    pub headers: IndexMap<HeaderName, HeaderSource>,
    pub body: Option<JSONSelection>,

    pub method_expression: Option<JSONSelection>,
    pub scheme: Option<JSONSelection>,
    pub host: Option<JSONSelection>,
    pub port: Option<JSONSelection>,
    pub user: Option<JSONSelection>,
    pub password: Option<JSONSelection>,
    pub source_path: Option<JSONSelection>,
    pub source_query: Option<JSONSelection>,
    pub connect_path: Option<JSONSelection>,
    pub connect_query: Option<JSONSelection>,
}

impl HttpJsonTransport {
    pub(super) fn from_directive(
        http: &ConnectHTTPArguments,
        source: Option<&SourceHTTPArguments>,
    ) -> Result<Self, FederationError> {
        let (method, connect_url) = if let Some(url) = &http.get {
            (Some(HTTPMethod::Get), Some(url))
        } else if let Some(url) = &http.post {
            (Some(HTTPMethod::Post), Some(url))
        } else if let Some(url) = &http.patch {
            (Some(HTTPMethod::Patch), Some(url))
        } else if let Some(url) = &http.put {
            (Some(HTTPMethod::Put), Some(url))
        } else if let Some(url) = &http.delete {
            (Some(HTTPMethod::Delete), Some(url))
        } else {
            (None, None)
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
            source_url: source.and_then(|s| s.base_url.clone()),
            connect_template: connect_url
                .map(|c| {
                    c.parse().map_err(|e: string_template::Error| {
                        FederationError::internal(format!(
                            "could not parse URL template: {message}",
                            message = e.message
                        ))
                    })
                })
                .transpose()?,
            method,
            headers,
            body: http.body.clone(),

            method_expression: http
                .method
                .clone()
                .or(source.and_then(|s| s.method.clone())),
            scheme: http
                .scheme
                .clone()
                .or(source.and_then(|s| s.scheme.clone())),
            host: http.host.clone().or(source.and_then(|s| s.host.clone())),
            port: http.port.clone().or(source.and_then(|s| s.port.clone())),
            user: http.user.clone().or(source.and_then(|s| s.user.clone())),
            password: http
                .password
                .clone()
                .or(source.and_then(|s| s.password.clone())),
            source_path: source.and_then(|s| s.path.clone()),
            source_query: source.and_then(|s| s.query.clone()),
            connect_path: http.path.clone(),
            connect_query: http.query.clone(),
        })
    }

    pub(super) fn label(&self) -> String {
        format!("http: {} {}", self.method_attr(), self.url_attr())
    }

    pub fn method_attr(&self) -> String {
        self.method
            .as_ref()
            .map_or("dynamic", |m| m.as_str())
            .to_string()
    }

    pub fn url_attr(&self) -> String {
        self.connect_template
            .as_ref()
            .map_or("dynamic".to_string(), |u| u.to_string())
    }

    pub fn method(&self, inputs: &IndexMap<String, Value>) -> (HTTPMethod, Vec<ApplyToError>) {
        self.method_expression
            .as_ref()
            .map(|m| {
                let (data, apply_to_errors) = m.apply_with_vars(&json!({}), inputs);
                let Some(method) = data
                    .as_ref()
                    .and_then(|s| s.as_str())
                    .and_then(|s| HTTPMethod::from_str(s).ok())
                else {
                    return (
                        HTTPMethod::default(),
                        vec![ApplyToError::new(
                            "Invalid HTTP method".to_string(),
                            vec![],
                            None,
                        )],
                    );
                };
                (method, apply_to_errors)
            })
            .or_else(|| Some((self.method.unwrap_or_default(), vec![])))
            .unwrap_or_default()
    }

    pub(super) fn variables(&self) -> impl Iterator<Item = Namespace> {
        self.variable_references()
            .map(|var_ref| var_ref.namespace.namespace)
    }

    pub(super) fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> {
        let header_selections = self
            .headers
            .iter()
            .flat_map(|(_, source)| source.expressions());

        let url_selections = self
            .connect_template
            .iter()
            .flat_map(|url| url.expressions())
            .map(|e| &e.expression);

        header_selections
            .chain(url_selections)
            .chain(self.body.iter())
            .chain(self.method_expression.iter())
            .chain(self.scheme.iter())
            .chain(self.host.iter())
            .chain(self.port.iter())
            .chain(self.user.iter())
            .chain(self.password.iter())
            .chain(self.source_path.iter())
            .chain(self.source_query.iter())
            .chain(self.connect_path.iter())
            .chain(self.connect_query.iter())
            .flat_map(|b| {
                b.external_var_paths()
                    .into_iter()
                    .flat_map(PathSelection::variable_reference)
            })
    }

    fn resolved_scheme(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> (Option<String>, Vec<ApplyToError>) {
        let mut warnings = vec![];

        let scheme = self.scheme.as_ref().and_then(|s| {
            let (s, w) = s.apply_with_vars(&json!({}), inputs);
            warnings.extend(w);
            s.as_ref().and_then(|s| s.as_str()).map(|s| s.to_string())
        });

        (scheme, warnings)
    }

    fn resolved_host(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Option<String>, Vec<ApplyToError>), FederationError> {
        let mut warnings = vec![];

        let host = self.host.as_ref().and_then(|a| {
            let (a, w) = a.apply_with_vars(&json!({}), inputs);
            warnings.extend(w);
            a.as_ref().and_then(|s| s.as_str()).map(|s| s.to_string())
        });

        Ok((host, warnings))
    }

    fn resolved_port(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Option<u16>, Vec<ApplyToError>), FederationError> {
        let mut warnings = vec![];

        let port = self.port.as_ref().and_then(|a| {
            let (a, w) = a.apply_with_vars(&json!({}), inputs);
            warnings.extend(w);
            a.as_ref().and_then(|s| s.as_u64()).map(|s| s as u16)
        });

        Ok((port, warnings))
    }

    fn resolved_user(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Option<String>, Vec<ApplyToError>), FederationError> {
        let mut warnings = vec![];

        let user = self.user.as_ref().and_then(|a| {
            let (a, w) = a.apply_with_vars(&json!({}), inputs);
            warnings.extend(w);
            a.as_ref().and_then(|s| s.as_str()).map(|s| s.to_string())
        });

        Ok((user, warnings))
    }

    fn resolved_password(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Option<String>, Vec<ApplyToError>), FederationError> {
        let mut warnings = vec![];

        let password = self.password.as_ref().and_then(|a| {
            let (a, w) = a.apply_with_vars(&json!({}), inputs);
            warnings.extend(w);
            a.as_ref().and_then(|s| s.as_str()).map(|s| s.to_string())
        });

        Ok((password, warnings))
    }

    fn base_url(&self) -> Url {
        self.source_url
            .as_ref()
            .or_else(|| self.connect_template.as_ref().and_then(|u| u.base.as_ref()))
            .map(|u| u.clone())
            .unwrap_or_else(|| Url::parse("https://localhost").expect("always parses"))
    }

    pub fn make_uri(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Url, Vec<ApplyToError>), FederationError> {
        let mut warnings = vec![];

        let mut base_url = self.base_url();

        let (scheme, ws) = self.resolved_scheme(inputs);
        warnings.extend(ws);
        if let Some(scheme) = scheme {
            base_url
                .set_scheme(&scheme)
                .map_err(|_| FederationError::internal(format!("Invalid URL scheme: {scheme}")))?;
        }

        let (host, ws) = self.resolved_host(inputs)?;
        warnings.extend(ws);
        if let Some(host) = host {
            base_url
                .set_host(Some(&host))
                .map_err(|_| FederationError::internal(format!("Invalid URL host: {host}")))?;
        }

        let (port, ws) = self.resolved_port(inputs)?;
        warnings.extend(ws);
        if let Some(port) = port {
            base_url
                .set_port(Some(port))
                .map_err(|_| FederationError::internal(format!("Invalid URL port: {port}")))?;
        }

        let (user, ws) = self.resolved_user(inputs)?;
        warnings.extend(ws);
        if let Some(user) = user {
            base_url
                .set_username(&user)
                .map_err(|_| FederationError::internal(format!("Invalid URL user: {user}")))?;
        }

        let (password, ws) = self.resolved_password(inputs)?;
        warnings.extend(ws);
        if let Some(password) = password {
            base_url.set_password(Some(&password)).map_err(|_| {
                FederationError::internal(format!("Invalid URL password: {password}"))
            })?;
        }

        let source_path = self
            .source_path
            .as_ref()
            .and_then(|p| {
                let (p, w) = p.apply_with_vars(&json!({}), inputs);
                warnings.extend(w);
                p.as_ref().and_then(|s| s.as_array().clone()).map(|s| {
                    s.iter()
                        .map(|s| s.as_str().unwrap_or_default().to_string())
                        .collect_vec()
                })
            })
            .unwrap_or_default();
        let connect_template_path = self
            .connect_template
            .as_ref()
            .map(|u| u.interpolate_path(inputs))
            .transpose()
            .map_err(|e| FederationError::internal(format!("Invalid URL template: {e}")))?
            .unwrap_or_default();
        let connect_path = self
            .connect_path
            .as_ref()
            .and_then(|p| {
                let (p, w) = p.apply_with_vars(&json!({}), inputs);
                warnings.extend(w);
                p.as_ref().and_then(|s| s.as_array().clone()).map(|s| {
                    s.iter()
                        .map(|s| s.as_str().unwrap_or_default().to_string())
                        .collect_vec()
                })
            })
            .unwrap_or_default();

        let source_query = self
            .source_query
            .as_ref()
            .and_then(|q| {
                let (q, w) = q.apply_with_vars(&json!({}), inputs);
                warnings.extend(w);
                q.as_ref().and_then(|s| s.as_object().clone()).map(|o| {
                    o.iter()
                        .map(|(k, v)| {
                            (
                                k.as_str().to_string(),
                                v.as_str().unwrap_or_default().to_string(),
                            )
                        })
                        .collect_vec()
                })
            })
            .unwrap_or_default();
        let connect_template_query = self
            .connect_template
            .as_ref()
            .map(|u| u.interpolate_query(inputs))
            .transpose()
            .map_err(|e| FederationError::internal(format!("Invalid URL template: {e}")))?
            .unwrap_or_default();
        let connect_query = self
            .connect_query
            .as_ref()
            .and_then(|q| {
                let (q, w) = q.apply_with_vars(&json!({}), inputs);
                warnings.extend(w);
                q.as_ref().and_then(|s| s.as_object().clone()).map(|o| {
                    o.iter()
                        .map(|(k, v)| {
                            (
                                k.as_str().to_string(),
                                v.as_str().unwrap_or_default().to_string(),
                            )
                        })
                        .collect_vec()
                })
            })
            .unwrap_or_default();

        base_url
            .path_segments_mut()
            .map_err(|_| FederationError::internal(format!("Invalid URL")))?
            .pop_if_empty()
            .extend(source_path)
            .extend(connect_template_path)
            .extend(connect_path);

        let qps = source_query
            .into_iter()
            .chain(connect_template_query.into_iter())
            .chain(connect_query.into_iter())
            .collect_vec();

        if !qps.is_empty() {
            base_url.query_pairs_mut().extend_pairs(qps);
        }

        Ok((base_url, warnings))
    }
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
mod tests {
    use apollo_compiler::collections::IndexMap;
    use insta::assert_snapshot;
    use serde_json_bytes::json;
    use url::Url;

    use crate::sources::connect::JSONSelection;

    use super::HttpJsonTransport;

    #[test]
    fn make_request() {
        let transport = HttpJsonTransport {
            source_url: Url::parse("http://example.com/a?z=1").ok(),
            connect_template: "/{$args.c}?x={$args.x}".parse().ok(),
            source_path: JSONSelection::parse("$(['b', $args.b2])").ok(),
            connect_path: JSONSelection::parse("$(['d', $args.d2])").ok(),
            source_query: JSONSelection::parse("y: $args.y").ok(),
            connect_query: JSONSelection::parse("w: $args.w").ok(),
            ..Default::default()
        };
        let inputs = IndexMap::from_iter([(
            "$args".to_string(),
            json!({"b2": "b2", "c": "c", "d2": "d2", "y": "y", "x": "x", "w": "w"}),
        )]);
        let (url, warnings) = transport.make_uri(&inputs).unwrap();
        assert_eq!(warnings, vec![]);
        assert_eq!(
            url.to_string(),
            "http://example.com/a/b/b2/c/d/d2?z=1&y=y&x=x&w=w"
        );
    }

    #[test]
    fn make_request_2() {
        let transport = HttpJsonTransport {
            scheme: JSONSelection::parse("$('http')").ok(),
            host: JSONSelection::parse("$('example.com')").ok(),
            connect_path: JSONSelection::parse("$(['a', $args.a, ''])").ok(),
            connect_query: JSONSelection::parse("$args { b }").ok(),
            ..Default::default()
        };
        let inputs = IndexMap::from_iter([("$args".to_string(), json!({"a": "1", "b": "2"}))]);
        let (url, warnings) = transport.make_uri(&inputs).unwrap();
        assert_eq!(warnings, vec![]);
        assert_eq!(url.to_string(), "http://example.com/a/1/?b=2");
    }

    // Previous make_uri() tests

    macro_rules! this {
        ($($value:tt)*) => {{
            let mut map = IndexMap::with_capacity_and_hasher(1, Default::default());
            map.insert("$this".to_string(), json!({ $($value)* }));
            map
        }};
    }

    #[test]
    fn append_path() {
        assert_eq!(
            HttpJsonTransport {
                source_url: Url::parse("https://localhost:8080/v1").ok(),
                connect_template: "/hello/42".parse().ok(),
                ..Default::default()
            }
            .make_uri(&Default::default())
            .unwrap()
            .0
            .as_str(),
            "https://localhost:8080/v1/hello/42"
        );
    }

    #[test]
    fn append_path_with_trailing_slash() {
        assert_eq!(
            HttpJsonTransport {
                source_url: Url::parse("https://localhost:8080/").ok(),
                connect_template: "/hello/42".parse().ok(),
                ..Default::default()
            }
            .make_uri(&Default::default())
            .unwrap()
            .0
            .as_str(),
            "https://localhost:8080/hello/42"
        );
    }

    #[test]
    fn append_path_test_with_trailing_slash_and_base_path() {
        assert_eq!(
            HttpJsonTransport {
                source_url: Url::parse("https://localhost:8080/v1/").ok(),
                connect_template: "/hello/{$this.id}?id={$this.id}".parse().ok(),
                ..Default::default()
            }
            .make_uri(&this! { "id": 42 })
            .unwrap()
            .0
            .as_str(),
            "https://localhost:8080/v1/hello/42?id=42"
        );
    }

    #[test]
    fn append_path_test_with_and_base_path_and_params() {
        assert_eq!(
            HttpJsonTransport {
                source_url: Url::parse("https://localhost:8080/v1?foo=bar").ok(),
                connect_template: "/hello/{$this.id}?id={$this.id}".parse().ok(),
                ..Default::default()
            }
            .make_uri(&this! { "id": 42 })
            .unwrap()
            .0
            .as_str(),
            "https://localhost:8080/v1/hello/42?foo=bar&id=42"
        );
    }

    #[test]
    fn append_path_test_with_and_base_path_and_trailing_slash_and_params() {
        assert_eq!(
            HttpJsonTransport {
                source_url: Url::parse("https://localhost:8080/v1/?foo=bar").ok(),
                connect_template: "/hello/{$this.id}?id={$this.id}".parse().ok(),
                ..Default::default()
            }
            .make_uri(&this! { "id": 42 })
            .unwrap()
            .0
            .as_str(),
            "https://localhost:8080/v1/hello/42?foo=bar&id=42"
        );
    }

    #[test]
    fn path_cases() {
        let template = "http://localhost/users/{$this.user_id}?a={$this.b}&e={$this.f.g}"
            .parse()
            .ok();

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }.make_uri(&Default::default())
                .unwrap().0
                .as_str(),
            @"http://localhost/users/?a=&e="
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }.make_uri(&this! {
                "user_id": 123,
                "b": "456",
                "f": {"g": "abc"}
            })
            .unwrap().0
            .as_str(),
            @"http://localhost/users/123?a=456&e=abc"
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }.make_uri(&this! {
                "user_id": 123,
                "f": "not an object"
            })
            .unwrap().0
            .as_str(),
            @"http://localhost/users/123?a=&e="
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }.make_uri(&this! {
                // The order of the variables should not matter.
                "b": "456",
                "user_id": "123"
            })
            .unwrap().0
            .as_str(),
            @"http://localhost/users/123?a=456&e="
        );

        assert_eq!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "user_id": "123",
                "b": "a",
                "f": {"g": "e"},
                // Extra variables should be ignored.
                "extra": "ignored"
            })
            .unwrap()
            .0
            .as_str(),
            "http://localhost/users/123?a=a&e=e",
        );
    }

    #[test]
    fn multi_variable_parameter_values() {
        let template =
            "http://localhost/locations/xyz({$this.x},{$this.y},{$this.z})?required={$this.b},{$this.c};{$this.d}&optional=[{$this.e},{$this.f}]"
                .parse()
                .ok();

        assert_eq!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "x": 1,
                "y": 2,
                "z": 3,
                "b": 4,
                "c": 5,
                "d": 6,
                "e": 7,
                "f": 8,
            })
            .unwrap()
            .0
            .as_str(),
            "http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B7%2C8%5D"
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "x": 1,
                "y": 2,
                "z": 3,
                "b": 4,
                "c": 5,
                "d": 6,
                "e": 7
                // "f": 8,
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B7%2C%5D",
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "x": 1,
                "y": 2,
                "z": 3,
                "b": 4,
                "c": 5,
                "d": 6,
                // "e": 7,
                "f": 8
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B%2C8%5D",
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "x": 1,
                "y": 2,
                "z": 3,
                "b": 4,
                "c": 5,
                "d": 6
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/locations/xyz(1,2,3)?required=4%2C5%3B6&optional=%5B%2C%5D",
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                // "x": 1,
                "y": 2,
                "z": 3
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/locations/xyz(,2,3)?required=%2C%3B&optional=%5B%2C%5D",
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "x": 1,
                "y": 2
                // "z": 3,
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/locations/xyz(1,2,)?required=%2C%3B&optional=%5B%2C%5D"
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "b": 4,
                // "c": 5,
                "d": 6,
                "x": 1,
                "y": 2,
                "z": 3
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/locations/xyz(1,2,3)?required=4%2C%3B6&optional=%5B%2C%5D"
        );

        let line_template = "http://localhost/line/{$this.p1.x},{$this.p1.y},{$this.p1.z}/{$this.p2.x},{$this.p2.y},{$this.p2.z}"
            .parse()
            .ok();

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: line_template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "p1": {
                    "x": 1,
                    "y": 2,
                    "z": 3,
                },
                "p2": {
                    "x": 4,
                    "y": 5,
                    "z": 6,
                }
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/line/1,2,3/4,5,6"
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: line_template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "p1": {
                    "x": 1,
                    "y": 2,
                    "z": 3,
                },
                "p2": {
                    "x": 4,
                    "y": 5,
                    // "z": 6,
                }
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/line/1,2,3/4,5,"
        );

        assert_snapshot!(
            HttpJsonTransport {
                connect_template: line_template.clone(),
                ..Default::default()
            }
            .make_uri(&this! {
                "p1": {
                    "x": 1,
                    // "y": 2,
                    "z": 3,
                },
                "p2": {
                    "x": 4,
                    "y": 5,
                    "z": 6,
                }
            })
            .unwrap()
            .0.as_str(),
            @"http://localhost/line/1,,3/4,5,6"
        );
    }

    /// Values are all strings, they can't have semantic value for HTTP. That means no dynamic paths,
    /// no nested query params, etc. When we expand values, we have to make sure they're safe.
    #[test]
    fn parameter_encoding() {
        let vars = &this! {
            "path": "/some/path",
            "question_mark": "a?b",
            "ampersand": "a&b=b",
            "hash": "a#b",
        };

        let template = "http://localhost/{$this.path}/{$this.question_mark}?a={$this.ampersand}&c={$this.hash}"
            .parse()
            .expect("Failed to parse URL template");

        let url = HttpJsonTransport {
            connect_template: Some(template),
            ..Default::default()
        }
        .make_uri(&vars)
        .unwrap()
        .0;

        assert_eq!(
            url.as_str(),
            "http://localhost/%2Fsome%2Fpath/a%3Fb?a=a%26b%3Db&c=a%23b"
        );
    }
}
