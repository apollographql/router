use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Write;
use std::iter::once;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use either::Either;
use http::Uri;
use http::uri::InvalidUri;
use http::uri::InvalidUriParts;
use http::uri::Parts;
use http::uri::PathAndQuery;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use thiserror::Error;

use super::ProblemLocation;
use crate::connectors::ApplyToError;
use crate::connectors::ConnectSpec;
use crate::connectors::JSONSelection;
use crate::connectors::Namespace;
use crate::connectors::PathSelection;
use crate::connectors::StringTemplate;
use crate::connectors::json_selection::ExternalVarPaths;
use crate::connectors::models::Header;
use crate::connectors::spec::ConnectHTTPArguments;
use crate::connectors::spec::SourceHTTPArguments;
use crate::connectors::string_template;
use crate::connectors::string_template::UriString;
use crate::connectors::string_template::write_value;
use crate::connectors::variable::VariableReference;
use crate::error::FederationError;

#[derive(Clone, Debug, Default)]
pub struct HttpJsonTransport {
    pub source_template: Option<StringTemplate>,
    pub connect_template: StringTemplate,
    pub method: HTTPMethod,
    pub headers: Vec<Header>,
    pub body: Option<JSONSelection>,
    pub source_path: Option<JSONSelection>,
    pub source_query_params: Option<JSONSelection>,
    pub connect_path: Option<JSONSelection>,
    pub connect_query_params: Option<JSONSelection>,
}

impl HttpJsonTransport {
    pub fn from_directive(
        http: ConnectHTTPArguments,
        source: Option<&SourceHTTPArguments>,
        spec: ConnectSpec,
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

        let mut headers = http.headers;
        for header in source.map(|source| &source.headers).into_iter().flatten() {
            if !headers
                .iter()
                .any(|connect_header| connect_header.name == header.name)
            {
                headers.push(header.clone());
            }
        }

        Ok(Self {
            source_template: source.map(|source| source.base_url.template.clone()),
            connect_template: StringTemplate::parse_with_spec(connect_url, spec).map_err(
                |e: string_template::Error| {
                    FederationError::internal(format!(
                        "could not parse URL template: {message}",
                        message = e.message
                    ))
                },
            )?,
            method,
            headers,
            body: http.body,
            source_path: source.and_then(|s| s.path.clone()),
            source_query_params: source.and_then(|s| s.query_params.clone()),
            connect_path: http.path,
            connect_query_params: http.query_params,
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
            .flat_map(|header| header.source.expressions());

        let source_selections = self
            .source_template
            .iter()
            .flat_map(|template| template.expressions().map(|e| &e.expression));

        url_selections
            .chain(header_selections)
            .chain(source_selections)
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

    pub fn make_uri(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Uri, Vec<(ProblemLocation, ApplyToError)>), MakeUriError> {
        let mut uri_parts = Parts::default();
        let mut warnings = Vec::new();

        let (connect_uri, connect_template_warnings) =
            self.connect_template.interpolate_uri(inputs)?;
        warnings.extend(
            connect_template_warnings
                .into_iter()
                .map(|warning| (ProblemLocation::ConnectUrl, warning)),
        );
        let resolved_source_uri = match &self.source_template {
            Some(template) => {
                let (uri, source_template_warnings) = template.interpolate_uri(inputs)?;
                warnings.extend(
                    source_template_warnings
                        .into_iter()
                        .map(|warning| (ProblemLocation::SourceUrl, warning)),
                );
                Some(uri)
            }
            None => None,
        };

        if let Some(source_uri) = &resolved_source_uri {
            uri_parts.scheme = source_uri.scheme().cloned();
            uri_parts.authority = source_uri.authority().cloned();
        } else {
            uri_parts.scheme = connect_uri.scheme().cloned();
            uri_parts.authority = connect_uri.authority().cloned();
        }

        let mut path = UriString::new();
        if let Some(source_uri) = &resolved_source_uri {
            path.write_without_encoding(source_uri.path())?;
        }
        if let Some(source_path) = self.source_path.as_ref() {
            warnings.extend(
                extend_path_from_expression(&mut path, source_path, inputs)?
                    .into_iter()
                    .map(|error| (ProblemLocation::SourcePath, error)),
            );
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
            warnings.extend(
                extend_path_from_expression(&mut path, connect_path, inputs)?
                    .into_iter()
                    .map(|error| (ProblemLocation::ConnectPath, error)),
            );
        }

        let mut query = UriString::new();
        if let Some(source_uri_query) = resolved_source_uri
            .as_ref()
            .and_then(|source_uri| source_uri.query())
        {
            query.write_without_encoding(source_uri_query)?;
        }
        if let Some(source_query) = self.source_query_params.as_ref() {
            warnings.extend(
                extend_query_from_expression(&mut query, source_query, inputs)?
                    .into_iter()
                    .map(|error| (ProblemLocation::SourceQueryParams, error)),
            );
        }
        let connect_query = connect_uri.query().unwrap_or_default();
        if !connect_query.is_empty() {
            if !query.is_empty() && !query.ends_with('&') {
                query.write_without_encoding("&")?;
            }
            query.write_without_encoding(connect_query)?;
        }
        if let Some(connect_query) = self.connect_query_params.as_ref() {
            warnings.extend(
                extend_query_from_expression(&mut query, connect_query, inputs)?
                    .into_iter()
                    .map(|error| (ProblemLocation::ConnectQueryParams, error)),
            );
        }

        let path = path.into_string();
        let query = query.into_string();

        uri_parts.path_and_query = Some(match (path.is_empty(), query.is_empty()) {
            (true, true) => PathAndQuery::from_static(""),
            (true, false) => PathAndQuery::try_from(format!("?{query}"))?,
            (false, true) => PathAndQuery::try_from(path)?,
            (false, false) => PathAndQuery::try_from(format!("{path}?{query}"))?,
        });

        let uri = Uri::from_parts(uri_parts).map_err(MakeUriError::BuildMergedUri)?;

        Ok((uri, warnings))
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

    let all_params = map
        .iter()
        .filter(|(_, value)| !value.is_null())
        .flat_map(|(key, value)| {
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
    pub const fn as_str(&self) -> &str {
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

#[cfg(test)]
mod test_make_uri {
    use std::str::FromStr;

    use apollo_compiler::collections::IndexMap;
    use pretty_assertions::assert_eq;
    use serde_json_bytes::json;

    use super::*;
    use crate::connectors::JSONSelection;

    /// Take data from all the places it can come from and make sure they combine in the right order
    #[test]
    fn merge_all_sources() {
        let transport = HttpJsonTransport {
            source_template: StringTemplate::from_str(
                "http://example.com/sourceUri?shared=sourceUri&sourceUri=sourceUri",
            )
            .ok(),
            connect_template: StringTemplate::parse_with_spec(
                "/{$args.connectUri}?shared={$args.connectUri}&{$args.connectUri}={$args.connectUri}",
                ConnectSpec::latest(),
            )
            .unwrap(),
            source_path: JSONSelection::parse("$args.sourcePath").ok(),
            connect_path: JSONSelection::parse("$args.connectPath").ok(),
            source_query_params: JSONSelection::parse("$args.sourceQuery").ok(),
            connect_query_params: JSONSelection::parse("$args.connectQuery").ok(),
            ..Default::default()
        };
        let inputs = IndexMap::from_iter([(
            "$args".to_string(),
            json!({
                "connectUri": "connectUri",
                "sourcePath": ["sourcePath1", "sourcePath2"],
                "connectPath": ["connectPath1", "connectPath2"],
                "sourceQuery": {"shared": "sourceQuery", "sourceQuery": "sourceQuery"},
                "connectQuery": {"shared": "connectQuery", "connectQuery": "connectQuery"},
            }),
        )]);
        let (url, _) = transport.make_uri(&inputs).unwrap();
        assert_eq!(
            url.to_string(),
            "http://example.com/sourceUri/sourcePath1/sourcePath2/connectUri/connectPath1/connectPath2\
            ?shared=sourceUri&sourceUri=sourceUri\
            &shared=sourceQuery&sourceQuery=sourceQuery\
            &shared=connectUri&connectUri=connectUri\
            &shared=connectQuery&connectQuery=connectQuery"
        );
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
                source_template: StringTemplate::from_str(source_uri).ok(),
                connect_template: connect_path.parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport
                    .make_uri(&Default::default())
                    .unwrap()
                    .0
                    .to_string(),
                "https://localhost:8080/v1/hello"
            );
        }

        #[rstest]
        #[case::when_base_has_trailing("http://localhost/")]
        #[case::when_base_does_not_have_trailing("http://localhost")]
        fn handle_slashes_when_adding_path_expression(#[case] base: &str) {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str(base).ok(),
                source_path: JSONSelection::parse("$([1, 2])").ok(),
                ..Default::default()
            };
            assert_eq!(
                transport
                    .make_uri(&Default::default())
                    .unwrap()
                    .0
                    .to_string(),
                "http://localhost/1/2"
            );
        }

        #[test]
        fn preserve_trailing_slash_from_connect() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("https://localhost:8080/v1").ok(),
                connect_template: "/hello/".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport
                    .make_uri(&Default::default())
                    .unwrap()
                    .0
                    .to_string(),
                "https://localhost:8080/v1/hello/"
            );
        }

        #[test]
        fn preserve_trailing_slash_from_source() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("https://localhost:8080/v1/").ok(),
                connect_template: "/".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport
                    .make_uri(&Default::default())
                    .unwrap()
                    .0
                    .to_string(),
                "https://localhost:8080/v1/"
            );
        }

        #[test]
        fn preserve_no_trailing_slash_from_source() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("https://localhost:8080/v1").ok(),
                connect_template: "/".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport
                    .make_uri(&Default::default())
                    .unwrap()
                    .0
                    .to_string(),
                "https://localhost:8080/v1"
            );
        }

        #[test]
        fn add_path_before_query_params() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("https://localhost:8080/v1?something")
                    .ok(),
                connect_template: "/hello".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport
                    .make_uri(&this! { "id": 42 })
                    .unwrap()
                    .0
                    .to_string(),
                "https://localhost:8080/v1/hello?something"
            );
        }

        #[test]
        fn trailing_slash_plus_query_params() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("https://localhost:8080/v1/?something")
                    .ok(),
                connect_template: "/hello/".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport
                    .make_uri(&this! { "id": 42 })
                    .unwrap()
                    .0
                    .to_string(),
                "https://localhost:8080/v1/hello/?something"
            );
        }

        #[test]
        fn with_trailing_slash_in_base_plus_query_params() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("https://localhost:8080/v1/?foo=bar")
                    .ok(),
                connect_template: StringTemplate::parse_with_spec(
                    "/hello/{$this.id}?id={$this.id}",
                    ConnectSpec::latest(),
                )
                .unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport
                    .make_uri(&this! {"id": 42 })
                    .unwrap()
                    .0
                    .to_string(),
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
                source_template: StringTemplate::from_str("http://localhost/users?a=b").ok(),
                connect_template: "/123".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap().0,
                "http://localhost/users/123?a=b"
            );
        }

        #[test]
        fn connect_only() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("http://localhost/users").ok(),
                connect_template: "?a=b&c=d".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap().0,
                "http://localhost/users?a=b&c=d"
            )
        }

        #[test]
        fn combine_from_both_uris() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("http://localhost/users?a=b").ok(),
                connect_template: "?c=d".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap().0,
                "http://localhost/users?a=b&c=d"
            )
        }

        #[test]
        fn source_and_connect_have_same_param() {
            let transport = HttpJsonTransport {
                source_template: StringTemplate::from_str("http://localhost/users?a=b").ok(),
                connect_template: "?a=d".parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(
                transport.make_uri(&Default::default()).unwrap().0,
                "http://localhost/users?a=b&a=d"
            )
        }

        #[test]
        fn repeated_params_from_array() {
            let transport = HttpJsonTransport {
                connect_template: "http://localhost".parse().unwrap(),
                connect_query_params: JSONSelection::parse("$args.connectQuery").ok(),
                ..Default::default()
            };
            let inputs = IndexMap::from_iter([(
                "$args".to_string(),
                json!({
                    "connectQuery": {"multi": ["first", "second"]},
                }),
            )]);
            assert_eq!(
                transport.make_uri(&inputs).unwrap().0,
                "http://localhost?multi=first&multi=second"
            )
        }
    }

    #[test]
    fn fragments_are_dropped() {
        let transport = HttpJsonTransport {
            source_template: StringTemplate::from_str("http://localhost/source?a=b#SourceFragment")
                .ok(),
            connect_template: "/connect?c=d#connectFragment".parse().unwrap(),
            ..Default::default()
        };
        assert_eq!(
            transport.make_uri(&Default::default()).unwrap().0,
            "http://localhost/source/connect?a=b&c=d"
        )
    }

    /// When merging source and connect pieces, we sometimes have to apply encoding as we go.
    /// This double-checks that we never _double_ encode pieces.
    #[test]
    fn pieces_are_not_double_encoded() {
        let transport = HttpJsonTransport {
            source_template: StringTemplate::from_str(
                "http://localhost/source%20path?param=source%20param",
            )
            .ok(),
            connect_template: "/connect%20path?param=connect%20param".parse().unwrap(),
            ..Default::default()
        };
        assert_eq!(
            transport.make_uri(&Default::default()).unwrap().0,
            "http://localhost/source%20path/connect%20path?param=source%20param&param=connect%20param"
        )
    }

    /// Regression test for a very specific case where the resulting `Uri` might not be valid
    /// because we did _too little_ work.
    #[test]
    fn empty_path_and_query() {
        let transport = HttpJsonTransport {
            source_template: None,
            connect_template: "http://localhost/".parse().unwrap(),
            ..Default::default()
        };
        assert_eq!(
            transport.make_uri(&Default::default()).unwrap().0,
            "http://localhost/"
        )
    }

    #[test]
    fn skip_null_query_params() {
        let transport = HttpJsonTransport {
            source_template: None,
            connect_template: "http://localhost/".parse().unwrap(),
            connect_query_params: JSONSelection::parse("something: $(null)").ok(),
            ..Default::default()
        };

        assert_eq!(
            transport.make_uri(&Default::default()).unwrap().0,
            "http://localhost/"
        )
    }

    #[test]
    fn skip_null_path_params() {
        let transport = HttpJsonTransport {
            source_template: None,
            connect_template: "http://localhost/".parse().unwrap(),
            connect_path: JSONSelection::parse("$([1, null, 2])").ok(),
            ..Default::default()
        };

        assert_eq!(
            transport.make_uri(&Default::default()).unwrap().0,
            "http://localhost/1/2"
        )
    }

    #[test]
    fn source_template_variables_retained() {
        let transport = HttpJsonTransport {
            source_template: StringTemplate::parse_with_spec(
                "http://${$config.subdomain}.localhost",
                ConnectSpec::latest(),
            )
            .ok(),
            connect_template: "/connect?c=d".parse().unwrap(),
            ..Default::default()
        };

        // Transport variables contain the config reference
        transport
            .variable_references()
            .find(|var_ref| var_ref.namespace.namespace == Namespace::Config)
            .unwrap();
    }

    #[test]
    fn source_template_interpolated_correctly() {
        let transport = HttpJsonTransport {
            source_template: StringTemplate::parse_with_spec(
                "http://{$config.subdomain}.localhost:{$config.port}",
                ConnectSpec::latest(),
            )
            .ok(),
            connect_template: "/connect?c=d".parse().unwrap(),
            ..Default::default()
        };
        let mut vars: IndexMap<String, Value> = Default::default();
        vars.insert(
            "$config".to_string(),
            json!({ "subdomain": "api", "port": 5000 }),
        );
        assert_eq!(
            transport.make_uri(&vars).unwrap().0,
            "http://api.localhost:5000/connect?c=d"
        );
    }
}
