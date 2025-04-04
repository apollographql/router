use apollo_compiler::collections::{HashSet, IndexMap};
use http::HeaderName;
use serde_json_bytes::{Value, json};
use url::Url;

use crate::{
    error::FederationError,
    sources::connect::{
        ApplyToError, JSONSelection, Namespace,
        spec::{ConnectHTTPArguments, SourceHTTPArguments},
    },
};

use super::HeaderSource;

#[derive(Debug, Clone)]
pub struct Http {
    pub method: JSONSelection,
    pub scheme: JSONSelection,
    pub authority: JSONSelection,
    pub source_path: Option<JSONSelection>,
    pub connect_path: Option<JSONSelection>,
    pub source_query: Option<JSONSelection>,
    pub connect_query: Option<JSONSelection>,
    pub body: Option<JSONSelection>,
    pub headers: IndexMap<HeaderName, HeaderSource>,
}

impl Default for Http {
    fn default() -> Self {
        Self {
            method: JSONSelection::parse("$('GET')").unwrap(),
            scheme: JSONSelection::parse("$('https')").unwrap(),
            authority: JSONSelection::parse("$('localhost')").unwrap(),
            source_path: None,
            connect_path: None,
            source_query: None,
            connect_query: None,
            body: None,
            headers: IndexMap::default(),
        }
    }
}

impl Http {
    pub(crate) fn from_directive(
        http: &ConnectHTTPArguments,
        source: Option<&SourceHTTPArguments>,
    ) -> Result<Self, FederationError> {
        let method_str = http.method.clone().unwrap_or_else(|| {
            source
                .and_then(|s| s.method.clone())
                .unwrap_or_else(|| "$('GET')".to_string())
        });
        let method = JSONSelection::parse(&method_str)
            .map_err(|_| FederationError::internal("invalid method"))?;

        let scheme_str = http.scheme.clone().unwrap_or_else(|| {
            source
                .and_then(|s| s.scheme.clone())
                .unwrap_or_else(|| "$('https')".to_string())
        });
        let scheme = JSONSelection::parse(&scheme_str)
            .map_err(|_| FederationError::internal("invalid scheme"))?;

        let authority_str = http
            .authority
            .clone()
            .or_else(|| source.and_then(|s| s.authority.clone()))
            .ok_or_else(|| FederationError::internal("invalid scheme"))?;
        let authority = JSONSelection::parse(&authority_str)
            .map_err(|_| FederationError::internal("invalid authority"))?;

        let source_path = source
            .and_then(|s| s.path.clone())
            .map(|s| {
                JSONSelection::parse(&s)
                    .map_err(|_| FederationError::internal("invalid source path"))
            })
            .transpose()?;

        let connect_path = http
            .path
            .clone()
            .map(|s| {
                JSONSelection::parse(&s)
                    .map_err(|_| FederationError::internal("invalid connect path"))
            })
            .transpose()?;

        let source_query = source
            .and_then(|s| s.query.clone())
            .map(|s| {
                JSONSelection::parse(&s)
                    .map_err(|_| FederationError::internal("invalid source query"))
            })
            .transpose()?;

        let connect_query = http
            .query
            .clone()
            .map(|s| {
                JSONSelection::parse(&s)
                    .map_err(|_| FederationError::internal("invalid connect query"))
            })
            .transpose()?;

        let body = http.body.clone();

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
            method,
            scheme,
            authority,
            source_path,
            connect_path,
            source_query,
            connect_query,
            body,
            headers,
        })
    }

    pub fn to_method(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(http::Method, Vec<ApplyToError>), String> {
        let mut all_errors = vec![];

        let (method, errors) = self.method.apply_with_vars(&json!({}), inputs);
        all_errors.extend(errors);
        let method = method
            .as_ref()
            .and_then(|s| s.as_str())
            .ok_or_else(|| "invalid method".to_string())?;

        Ok((method.try_into().map_err(|_| "Invalid method")?, all_errors))
    }

    pub fn to_uri(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Url, Vec<ApplyToError>), String> {
        let mut all_errors = vec![];

        let (scheme, errors) = self.scheme.apply_with_vars(&json!({}), inputs);
        let scheme = scheme
            .as_ref()
            .and_then(|s| s.as_str())
            .ok_or_else(|| "invalid scheme".to_string())?;
        all_errors.extend(errors);
        let (authority, errors) = self.authority.apply_with_vars(&json!({}), inputs);
        all_errors.extend(errors);
        let authority = authority
            .as_ref()
            .and_then(|s| s.as_str())
            .ok_or_else(|| "invalid authority".to_string())?;

        // PATH
        let source_path = self
            .source_path
            .as_ref()
            .map(|p| p.apply_with_vars(&json!({}), inputs))
            .and_then(|(path, errors)| {
                all_errors.extend(errors);
                path
            })
            .and_then(|p| match p {
                Value::Array(values) => Some(values),
                _ => None,
            });

        let connect_path = self
            .connect_path
            .as_ref()
            .map(|p| p.apply_with_vars(&json!({}), inputs))
            .and_then(|(path, errors)| {
                all_errors.extend(errors);
                path
            })
            .and_then(|p| match p {
                Value::Array(values) => Some(values),
                _ => None,
            });

        let mut path = vec![];
        if let Some(source_path) = source_path {
            path.extend(source_path);
        }
        if let Some(connect_path) = connect_path {
            path.extend(connect_path);
        }
        let path = path
            .into_iter()
            .flat_map(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");

        // QUERY

        let source_query = self
            .source_query
            .as_ref()
            .map(|q| q.apply_with_vars(&json!({}), inputs))
            .and_then(|(query, errors)| {
                all_errors.extend(errors);
                query
            })
            .and_then(|p| match p {
                Value::Array(values) => Some(values),
                _ => None,
            });

        let connect_query = self
            .connect_query
            .as_ref()
            .map(|q| q.apply_with_vars(&json!({}), inputs))
            .and_then(|(query, errors)| {
                all_errors.extend(errors);
                query
            })
            .and_then(|q| match q {
                Value::Array(values) => Some(values),
                _ => None,
            });

        let mut query = vec![];
        if let Some(source_query) = source_query {
            query.extend(source_query);
        }
        if let Some(connect_query) = connect_query {
            query.extend(connect_query);
        }
        let query = query
            .into_iter()
            .flat_map(|v| match v {
                Value::Object(m) => {
                    let name = m
                        .iter()
                        .find(|(k, _)| k.as_str() == "name")
                        .map(|(_, v)| v.as_str().unwrap_or_default())
                        .unwrap_or_default();
                    let value = m
                        .iter()
                        .find(|(k, _)| k.as_str() == "value")
                        .map(|(_, v)| v.as_str().unwrap_or_default())
                        .unwrap_or_default();
                    Some(format!("{}={}", name, value))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("&");

        let query = if !query.is_empty() {
            format!("?{}", query)
        } else {
            String::new()
        };
        let url_str = format!("{}://{}/{}{}", scheme, authority, path, query);
        let url = Url::parse(&url_str).map_err(|_| "invalid URL".to_string())?;

        Ok((url, all_errors))
    }

    pub fn variables(&self) -> HashSet<Namespace> {
        self.method
            .external_variables()
            .chain(self.scheme.external_variables())
            .chain(self.authority.external_variables())
            .chain(self.source_path.iter().flat_map(|p| p.external_variables()))
            .chain(
                self.connect_path
                    .iter()
                    .flat_map(|p| p.external_variables()),
            )
            .chain(
                self.source_query
                    .iter()
                    .flat_map(|q| q.external_variables()),
            )
            .chain(
                self.connect_query
                    .iter()
                    .flat_map(|q| q.external_variables()),
            )
            .chain(self.body.iter().flat_map(|b| b.external_variables()))
            .collect()
    }
}
