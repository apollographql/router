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

    fn matches(&self, request: &wiremock::Request, index: usize) -> Result<(), String> {
        if let Some(method) = self.method.as_ref() {
            if method != &request.method.to_string() {
                return Err(format!(
                    "[Request {index}]: Expected method {method}, got {}",
                    request.method
                ));
            }
        }

        if let Some(path) = self.path.as_ref() {
            if path != request.url.path() {
                return Err(format!(
                    "[Request {index}]: Expected path {path}, got {}",
                    request.url.path()
                ));
            }
        }

        if let Some(query) = self.query.as_ref() {
            if query != request.url.query().unwrap_or_default() {
                return Err(format!(
                    "[Request {index}]: Expected query {query}, got {}",
                    request.url.query().unwrap_or_default()
                ));
            }
        }

        if let Some(body) = self.body.as_ref() {
            if body != &request.body_json::<serde_json::Value>().unwrap() {
                return Err(format!("[Request {index}]: incorrect body"));
            }
        }

        for (name, expected) in self.headers.iter() {
            match request.headers.get(name) {
                Some(actual) => {
                    let expected: HashSet<String> =
                        expected.iter().map(|v| v.as_str().to_owned()).collect();
                    let actual: HashSet<String> =
                        actual.iter().map(|v| v.as_str().to_owned()).collect();
                    if expected != actual {
                        return Err(format!(
                            "[Request {index}]: expected header {name} to be [{}], was [{}]",
                            expected.iter().join(", "),
                            actual.iter().join(", ")
                        ));
                    }
                }
                None => {
                    return Err(format!(
                        "[Request {index}]: expected header {name}, was missing"
                    ));
                }
            }
        }
        Ok(())
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
        matcher.matches(request, i).unwrap();
    }
}

/// Basically a [`crate::query_planner::PlanNode`], but specialized for testing connectors.
pub(crate) enum Plan {
    Fetch(Matcher),
    Sequence(Vec<Plan>),
    /// Fetches that can run in any order.
    /// TODO: support nesting plans if we need it some day
    Parallel(Vec<Matcher>),
}

impl Plan {
    fn len(&self) -> usize {
        match self {
            Plan::Fetch(_) => 1,
            Plan::Sequence(plans) => plans.iter().map(Plan::len).sum(),
            Plan::Parallel(matchers) => matchers.len(),
        }
    }

    pub(crate) fn assert_matches(self, received: &[wiremock::Request]) {
        assert_eq!(
            received.len(),
            self.len(),
            "Expected {} requests, recorded {}",
            self.len(),
            received.len()
        );
        self.matches(received, 0);
    }

    fn matches(self, received: &[wiremock::Request], index_offset: usize) {
        match self {
            Plan::Fetch(matcher) => {
                matcher.matches(&received[0], index_offset).unwrap();
            }
            Plan::Sequence(plans) => {
                let mut index = 0;
                for plan in plans {
                    let len = plan.len();
                    plan.matches(&received[index..index + len], index_offset + index);
                    index += len;
                }
            }
            Plan::Parallel(mut matchers) => {
                // These can be received in any order, so we need to make sure _one_ of the matchers
                // matches each request.
                'requests: for (request_index, request) in received.iter().enumerate() {
                    for (matcher_index, matcher) in matchers.iter().enumerate() {
                        if matcher
                            .matches(request, request_index + index_offset)
                            .is_ok()
                        {
                            matchers.remove(matcher_index);
                            continue 'requests;
                        }
                    }
                    panic!("No plan matched request {:?}", request);
                }
            }
        }
    }
}
