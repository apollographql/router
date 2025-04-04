use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use http::HeaderName;
use itertools::Itertools;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use url::Url;

use super::HeaderSource;
use crate::error::FederationError;
use crate::sources::connect::ApplyToError;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Namespace;
use crate::sources::connect::spec::ConnectHTTPArguments;
use crate::sources::connect::spec::SourceHTTPArguments;

#[derive(Debug, Clone)]
pub struct Http {
    pub method: Option<JSONSelection>,
    pub scheme: Option<JSONSelection>,
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
            method: None,
            scheme: None,
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

#[cfg(test)]
#[buildstructor::buildstructor]
impl Http {
    #[builder]
    fn new(
        method: Option<JSONSelection>,
        scheme: Option<JSONSelection>,
        authority: Option<JSONSelection>,
        source_path: Option<JSONSelection>,
        connect_path: Option<JSONSelection>,
        source_query: Option<JSONSelection>,
        connect_query: Option<JSONSelection>,
        body: Option<JSONSelection>,
        headers: Option<IndexMap<HeaderName, HeaderSource>>,
    ) -> Self {
        Self {
            method,
            scheme,
            authority: authority.unwrap_or(JSONSelection::parse("$('localhost')").unwrap()),
            source_path,
            connect_path,
            source_query,
            connect_query,
            body,
            headers: headers.unwrap_or_default(),
        }
    }
}

impl Http {
    pub(crate) fn from_directive(
        http: &ConnectHTTPArguments,
        source: Option<&SourceHTTPArguments>,
    ) -> Result<Self, FederationError> {
        let method = http
            .method
            .clone()
            .or_else(|| source.and_then(|s| s.method.clone()))
            .map(|s| {
                JSONSelection::parse(&s).map_err(|_| FederationError::internal("invalid method"))
            })
            .transpose()?;

        let scheme = http
            .scheme
            .clone()
            .or_else(|| source.and_then(|s| s.scheme.clone()))
            .map(|s| {
                JSONSelection::parse(&s).map_err(|_| FederationError::internal("invalid scheme"))
            })
            .transpose()?;

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
                dbg!(&s);
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

        let Some(method) = self.method.as_ref() else {
            return Ok((http::Method::GET, vec![]));
        };

        let (method, errors) = method.apply_with_vars(&json!({}), inputs);
        all_errors.extend(errors);
        let method = method
            .as_ref()
            .and_then(|s| s.as_str())
            .ok_or_else(|| "invalid method".to_string())?;

        Ok((method.try_into().map_err(|_| "Invalid method")?, all_errors))
    }

    fn base_url(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Url, Vec<ApplyToError>), String> {
        let mut all_errors = vec![];

        let scheme = if let Some(scheme) = &self.scheme {
            let (scheme, errors) = scheme.apply_with_vars(&json!({}), inputs);
            all_errors.extend(errors);
            scheme
                .and_then(|s| match s {
                    Value::String(byte_string) => Some(byte_string.as_str().to_string()),
                    _ => None,
                })
                .ok_or_else(|| "invalid scheme".to_string())?
        } else {
            "https".to_string()
        };

        let (authority, errors) = self.authority.apply_with_vars(&json!({}), inputs);
        all_errors.extend(errors);
        let authority = authority
            .as_ref()
            .and_then(|s| s.as_str())
            .ok_or_else(|| "invalid authority".to_string())?;

        let url_str = format!("{}://{}", scheme, authority);
        let url = Url::parse(&url_str).map_err(|_| "invalid URL".to_string())?;

        Ok((url, all_errors))
    }

    fn path(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(impl Iterator<Item = String>, Vec<ApplyToError>), String> {
        let mut all_errors = vec![];

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

        let strings = source_path
            .into_iter()
            .flatten()
            .chain(connect_path.into_iter().flatten())
            .flat_map(|v| match v {
                Value::Null => Some("".to_string()),
                Value::String(byte_string) => Some(byte_string.as_str().to_string()),
                _ => None,
            });

        Ok((strings, all_errors))
    }

    #[allow(clippy::type_complexity)] // TODO
    fn query_key_pairs_from_selection(
        selection: &JSONSelection,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Vec<(String, String)>, Vec<ApplyToError>), String> {
        let (query, warnings) = selection.apply_with_vars(&json!({}), inputs);

        let Some(query) = query else {
            return Ok((vec![], warnings));
        };

        let pairs = match query {
            Value::Array(values) => {
                let mut pairs = vec![];
                for value in values {
                    if let Value::Object(map) = value {
                        let Some(name) = map.get("name").and_then(|v| v.as_str()) else {
                            continue;
                        };

                        match map.get("value") {
                            Some(Value::String(value)) => {
                                pairs.push((name.to_string(), value.as_str().to_string()));
                            }
                            Some(Value::Number(value)) => {
                                pairs.push((name.to_string(), value.to_string()));
                            }
                            Some(Value::Bool(value)) => {
                                pairs.push((name.to_string(), value.to_string()));
                            }
                            _ => {}
                        }
                    }
                }
                pairs
            }
            Value::Object(map) => map
                .into_iter()
                .filter_map(|(key, value)| {
                    let key = key.as_str().to_string();
                    match value {
                        Value::String(value) => Some((key, value.as_str().to_string())),
                        Value::Number(value) => Some((key, value.to_string())),
                        Value::Bool(value) => Some((key, value.to_string())),
                        Value::Null => None,
                        _ => None,
                    }
                })
                .collect_vec(),
            _ => {
                return Err("invalid query".to_string());
            }
        };

        Ok((pairs, warnings))
    }

    fn query(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(impl Iterator<Item = (String, String)>, Vec<ApplyToError>), String> {
        let mut all_errors = vec![];
        let mut query = vec![];

        if let Some((source_query, warnings)) = self
            .source_query
            .as_ref()
            .map(|q| Self::query_key_pairs_from_selection(q, inputs))
            .transpose()?
        {
            query.extend(source_query);
            all_errors.extend(warnings);
        };

        if let Some((connect_query, warnings)) = self
            .connect_query
            .as_ref()
            .map(|q| Self::query_key_pairs_from_selection(q, inputs))
            .transpose()?
        {
            query.extend(connect_query);
            all_errors.extend(warnings);
        };

        Ok((query.into_iter(), all_errors))
    }

    pub fn to_uri(
        &self,
        inputs: &IndexMap<String, Value>,
    ) -> Result<(Url, Vec<ApplyToError>), String> {
        let (mut url, mut all_warnings) = self.base_url(inputs)?;

        let (path, warnings) = self.path(inputs)?;
        all_warnings.extend(warnings);

        {
            let mut mut_path = url.path_segments_mut().map_err(|_| "cannot mutate path?")?;
            mut_path.extend(path);
        }

        let (query, warnings) = self.query(inputs)?;
        let query = query.collect_vec();
        all_warnings.extend(warnings);
        if !query.is_empty() {
            let mut qp = url.query_pairs_mut();
            for (key, value) in query {
                qp.append_pair(&key, &value);
            }
        }

        Ok((url, all_warnings))
    }

    pub fn variables(&self) -> HashSet<Namespace> {
        self.authority
            .external_variables()
            .chain(self.method.iter().flat_map(|m| m.external_variables()))
            .chain(self.scheme.iter().flat_map(|s| s.external_variables()))
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

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;
    use serde_json_bytes::json;

    use crate::selection;

    #[test]
    fn test_paths() {
        let http = super::Http::builder()
            .source_path(selection!("$(['api', 'v1'])"))
            .connect_path(selection!("$(['posts', $args.id])"))
            .build();

        let inputs = IndexMap::from_iter([("$args".to_string(), json!({"id": "1"}))]);
        let (url, warnings) = http.to_uri(&inputs).unwrap();

        assert_eq!(warnings, vec![]);
        assert_eq!(url.to_string(), "https://localhost/api/v1/posts/1");
    }

    #[test]
    fn test_queries() {
        let http = super::Http::builder()
            .source_query(selection!("$({ api_key: '1234' })"))
            .connect_query(selection!(
                "$([{ name: 'limit', value: $args.first }, { name: 'offset', value: $args.after }])"
            ))
            .build();

        let inputs =
            IndexMap::from_iter([("$args".to_string(), json!({ "first": 10, "after": null }))]);
        let (url, warnings) = http.to_uri(&inputs).unwrap();

        assert_eq!(warnings, vec![]);
        assert_eq!(url.to_string(), "https://localhost/?api_key=1234&limit=10");
    }

    #[test]
    fn test_repeated_params() {
        let http = super::Http::builder()
            .connect_query(selection!("$batch.id { name: $('id[]') value: $ }"))
            .build();

        let inputs =
            IndexMap::from_iter([("$batch".to_string(), json!([{ "id": 1 }, { "id": 2 }]))]);
        let (url, warnings) = http.to_uri(&inputs).unwrap();

        assert_eq!(warnings, vec![]);
        assert_eq!(url.to_string(), "https://localhost/?id%5B%5D=1&id%5B%5D=2");
    }

    #[test]
    fn test_path_characters() {
        let http = super::Http::builder()
            .connect_path(selection!(
                r#"
                $([
                    'normal',
                    'with space',
                    'slash/in/segment',
                    'semi;colon',
                    'colon:colon',
                    'unicode-✓',
                    'quote"\'',
                    'reserved!*();:@&=+$',
                    'weird%20chars'
                ])
            "#
            ))
            .build();

        let inputs = IndexMap::default();
        let (url, warnings) = http.to_uri(&inputs).unwrap();

        assert_eq!(warnings, vec![]);
        assert_eq!(
            url.to_string(),
            "https://localhost/normal/with%20space/slash%2Fin%2Fsegment/semi;colon/colon:colon/unicode-%E2%9C%93/quote%22'/reserved!*();:@&=+$/weird%2520chars"
        );
    }

    #[test]
    fn test_param_names() {
        let http = super::Http::builder()
            .connect_query(selection!(
                r#"
                $({
                    q: 1,
                    'user name': 1,
                    'na&me': 1,
                    'fil/ter': 1,
                    'emoji_😀': 1,
                    'key=val': 1,
                    '!weird-key': 1
                })
            "#
            ))
            .build();

        let inputs = IndexMap::default();
        let (url, warnings) = http.to_uri(&inputs).unwrap();

        assert_eq!(warnings, vec![]);
        assert_eq!(
            url.to_string(),
            "https://localhost/?q=1&user+name=1&na%26me=1&fil%2Fter=1&emoji_%F0%9F%98%80=1&key%3Dval=1&%21weird-key=1"
        );
    }

    #[test]
    fn test_param_values() {
        let http = super::Http::builder()
            .connect_query(selection!(
                r#"
                $({
                    a: 'simple',
                    b: 'has space',
                    c: 'a&b=c',
                    d: '10% increase',
                    e: '?question',
                    f: '#fragment',
                    g: 'weird:;value',
                    h: 'multi\nline',
                    i: '💯',
                    j: 'a=b&c=d'
                })
            "#
            ))
            .build();

        let inputs = IndexMap::default();
        let (url, warnings) = http.to_uri(&inputs).unwrap();

        assert_eq!(warnings, vec![]);
        assert_eq!(
            url.to_string(),
            "https://localhost/?a=simple&b=has+space&c=a%26b%3Dc&d=10%25+increase&e=%3Fquestion&f=%23fragment&g=weird%3A%3Bvalue&h=multi%0Aline&i=%F0%9F%92%AF&j=a%3Db%26c%3Dd"
        );
    }
}
