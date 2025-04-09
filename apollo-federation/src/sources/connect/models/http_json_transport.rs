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
    pub authority: Option<JSONSelection>,
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
            authority: http
                .authority
                .clone()
                .or(source.and_then(|s| s.authority.clone())),
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
            .chain(self.authority.iter())
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

    pub fn make_uri(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Url, Vec<ApplyToError>), FederationError> {
        // scheme: self.schema or connect_template.base.scheme or baseURL.schema
        // authority: self.authority or connect_template.base.authority or baseURL.authority
        // path: (baseURL path + source path eval + connect_template path + connect path eval).join('/')
        // query: (baseURL query + source query eval + connect_template query + connect query eval).join('&')

        let mut warnings = vec![];

        let scheme = self
            .scheme
            .as_ref()
            .and_then(|s| {
                let (s, w) = s.apply_with_vars(&json!({}), inputs);
                warnings.extend(w);
                s.as_ref().and_then(|s| s.as_str()).map(|s| s.to_string())
            })
            .or_else(|| {
                self.connect_template
                    .as_ref()
                    .and_then(|u| u.base.as_ref())
                    .map(|u| u.scheme().to_string())
            })
            .or_else(|| self.source_url.as_ref().map(|u| u.scheme().to_string()))
            .unwrap_or("GET".to_string());

        let authority = self
            .authority
            .as_ref()
            .and_then(|a| {
                let (a, w) = a.apply_with_vars(&json!({}), inputs);
                warnings.extend(w);
                a.as_ref().and_then(|s| s.as_str()).map(|s| s.to_string())
            })
            .or_else(|| {
                self.connect_template
                    .as_ref()
                    .and_then(|u| u.base.as_ref())
                    .map(|u| u.authority().to_string())
            })
            .or_else(|| self.source_url.as_ref().map(|u| u.authority().to_string()))
            .unwrap_or("GET".to_string());

        let source_base_path = self
            .source_url
            .as_ref()
            .map(|u| u.path().split('/').collect_vec())
            .unwrap_or_default();
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

        let source_base_query = self
            .source_url
            .as_ref()
            .map(|u| {
                u.query()
                    .unwrap_or_default()
                    .split("&")
                    .map(|s| {
                        let mut iter = s.splitn(2, '=');
                        (
                            iter.next().unwrap_or_default(),
                            iter.next().unwrap_or_default(),
                        )
                    })
                    .collect_vec()
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

        let mut url = Url::parse(&format!("{}://{}", scheme, authority))
            .map_err(|e| FederationError::internal(format!("Invalid URL: {e}")))?;

        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| FederationError::internal(format!("Invalid URL")))?;
            segments.extend(source_base_path);
            segments.extend(source_path);
            segments.extend(connect_template_path);
            segments.extend(connect_path);
        }

        let qps = source_base_query
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .chain(source_query.into_iter())
            .chain(connect_template_query.into_iter())
            .chain(connect_query.into_iter())
            .collect_vec();

        if !qps.is_empty() {
            url.query_pairs_mut().extend_pairs(qps);
        }

        Ok((url, warnings))
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
    use serde_json_bytes::json;
    use url::Url;

    use crate::sources::connect::JSONSelection;

    #[test]
    fn make_request() {
        let transport = super::HttpJsonTransport {
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
        let transport = super::HttpJsonTransport {
            scheme: JSONSelection::parse("$('http')").ok(),
            authority: JSONSelection::parse("$('example.com')").ok(),
            connect_path: JSONSelection::parse("$(['a', $args.a, ''])").ok(),
            connect_query: JSONSelection::parse("$args { b }").ok(),
            ..Default::default()
        };
        let inputs = IndexMap::from_iter([("$args".to_string(), json!({"a": "1", "b": "2"}))]);
        let (url, warnings) = transport.make_uri(&inputs).unwrap();
        assert_eq!(warnings, vec![]);
        assert_eq!(url.to_string(), "http://example.com/a/1/?b=2");
    }
}
