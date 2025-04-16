use std::fmt::Display;
use std::fmt::Formatter;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use either::Either;
use http::HeaderName;
use http::Uri;

use crate::error::FederationError;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Namespace;
use crate::sources::connect::PathSelection;
use crate::sources::connect::StringTemplate;
use crate::sources::connect::header::HeaderValue;
use crate::sources::connect::json_selection::ExternalVarPaths;
use crate::sources::connect::spec::ConnectHTTPArguments;
use crate::sources::connect::spec::SourceHTTPArguments;
use crate::sources::connect::string_template;
use crate::sources::connect::variable::VariableReference;

#[derive(Clone, Debug)]
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
        format!("http: {} {}", self.method.as_str(), self.connect_template)
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
}

/// The HTTP arguments needed for a connect request
#[derive(Debug, Clone, Copy)]
pub enum HTTPMethod {
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
