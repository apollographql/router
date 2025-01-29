use std::collections::HashMap;
use std::collections::HashSet;

use itertools::Itertools;
use wiremock::http::HeaderName;
use wiremock::http::HeaderValue;
use wiremock::http::HeaderValues;

#[derive(Clone)]
pub(crate) struct Matcher {
    method: Option<String>,
    path: Option<String>,
    query: Option<String>,
    body: Option<serde_json::Value>,
    headers: HashMap<HeaderName, HeaderValues>,
}

impl Matcher {
    pub(crate) fn new() -> Self {
        Self {
            method: None,
            path: None,
            query: None,
            body: None,
            headers: Default::default(),
        }
    }

    pub(crate) fn method(mut self, method: &str) -> Self {
        self.method = Some(method.to_string());
        self
    }

    pub(crate) fn path(mut self, path: &str) -> Self {
        self.path = Some(path.to_string());
        self
    }

    pub(crate) fn query(mut self, query: &str) -> Self {
        self.query = Some(query.to_string());
        self
    }

    pub(crate) fn body(mut self, body: serde_json::Value) -> Self {
        self.body = Some(body);
        self
    }

    pub(crate) fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        let values = self.headers.entry(name).or_insert(Vec::new().into());
        values.append(&mut Vec::from([value]).into());
        self
    }

    fn matches(&self, request: &wiremock::Request, index: usize) {
        if let Some(method) = self.method.as_ref() {
            assert_eq!(
                method,
                &request.method.to_string(),
                "[Request {}]: Expected method {}, got {}",
                index,
                method,
                request.method
            )
        }

        if let Some(path) = self.path.as_ref() {
            assert_eq!(
                path,
                request.url.path(),
                "[Request {}]: Expected path {}, got {}",
                index,
                path,
                request.url.path()
            )
        }

        if let Some(query) = self.query.as_ref() {
            assert_eq!(
                query,
                request.url.query().unwrap_or_default(),
                "[Request {}]: Expected query {}, got {}",
                index,
                query,
                request.url.query().unwrap_or_default()
            )
        }

        if let Some(body) = self.body.as_ref() {
            assert_eq!(
                body,
                &request.body_json::<serde_json::Value>().unwrap(),
                "[Request {}]: incorrect body",
                index,
            )
        }

        for (name, expected) in self.headers.iter() {
            match request.headers.get(name) {
                Some(actual) => {
                    let expected: HashSet<String> =
                        expected.iter().map(|v| v.as_str().to_owned()).collect();
                    let actual: HashSet<String> =
                        actual.iter().map(|v| v.as_str().to_owned()).collect();
                    assert_eq!(
                        expected,
                        actual,
                        "[Request {}]: expected header {} to be [{}], was [{}]",
                        index,
                        name,
                        expected.iter().join(", "),
                        actual.iter().join(", ")
                    );
                }
                None => {
                    panic!("[Request {}]: expected header {}, was missing", index, name);
                }
            }
        }
    }
}

pub(crate) fn matches(received: &[wiremock::Request], matchers: Vec<Matcher>) {
    assert_eq!(
        received.len(),
        matchers.len(),
        "Expected {} requests, recorded {}",
        matchers.len(),
        received.len()
    );
    for (i, (request, matcher)) in received.iter().zip(matchers.iter()).enumerate() {
        matcher.matches(request, i);
    }
}
