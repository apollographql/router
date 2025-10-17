use std::fmt::Debug;

use futures::StreamExt;
use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::graphql;
use crate::plugins::test::RequestTestExt;
use crate::plugins::test::ResponseTestExt;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::supergraph;

fn canned_request_body() -> Value {
    json!({
            "query":"query SimpleQuery {\ntopProducts {\n  name\n  price\n   \n}\n}"
    })
}

fn canned_request_query() -> &'static str {
    "query SimpleQuery {\ntopProducts {\n  name\n  price\n   \n}\n}"
}

fn canned_response_body() -> Value {
    json!({
            "field": "value"
    })
}

fn canned_response_body_array() -> Value {
    json!([{
        "data": {
            "field": "value"
        }
    }])
}

impl RequestTestExt<supergraph::Request, supergraph::Response> for SupergraphRequest {
    fn canned_request() -> supergraph::Request {
        supergraph::Request::fake_builder()
            .query(canned_request_query())
            .build()
            .expect("canned request")
    }

    fn canned_result(self) -> supergraph::ServiceResult {
        supergraph::Response::fake_builder()
            .context(self.context.clone())
            .data(canned_response_body())
            .build()
    }

    fn assert_context_eq<T>(&self, key: &str, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug,
    {
        let ctx_value = self
            .context
            .get::<_, T>(key)
            .expect("context value not deserializable")
            .expect("context value not found");
        pretty_assertions::assert_eq!(ctx_value, value, "context '{}' value mismatch", key);
    }

    fn assert_context_contains(&self, key: &str) {
        if !self.context.contains_key(key) {
            panic!("context '{key}' value not found")
        }
    }

    fn assert_context_not_contains(&self, key: &str) {
        if self.context.contains_key(key) {
            panic!("context '{key}' value was present")
        }
    }

    fn assert_header_eq(&self, key: &str, value: &str) {
        let header_value = self
            .supergraph_request
            .headers()
            .get(key)
            .unwrap_or_else(|| panic!("header '{key}' not found"));
        pretty_assertions::assert_eq!(header_value, value, "header '{}' value mismatch", key);
    }

    async fn assert_body_eq<T>(&mut self, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug + Serialize,
    {
        pretty_assertions::assert_eq!(
            serde_yaml::to_string(&self.supergraph_request.body_mut())
                .expect("could not serialize"),
            serde_yaml::to_string(&value).expect("could not serialize")
        );
    }

    async fn assert_canned_body(&mut self) {
        self.assert_body_eq(canned_request_body()).await
    }
}

impl ResponseTestExt for SupergraphResponse {
    fn assert_context_eq<T>(&self, key: &str, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug,
    {
        let ctx_value = self
            .context
            .get::<_, T>(key)
            .expect("context value not deserializable")
            .expect("context value not found");
        pretty_assertions::assert_eq!(ctx_value, value, "context '{}' value mismatch", key);
    }

    fn assert_context_contains(&self, key: &str) {
        if !self.context.contains_key(key) {
            panic!("context '{key}' value not found")
        }
    }

    fn assert_context_not_contains(&self, key: &str) {
        if self.context.contains_key(key) {
            panic!("context '{key}' value was present")
        }
    }

    fn assert_header_eq(&self, key: &str, value: &str) {
        let header_value = self
            .response
            .headers()
            .get(key)
            .unwrap_or_else(|| panic!("header '{key}' not found"));
        pretty_assertions::assert_eq!(header_value, value, "header '{}' value mismatch", key);
    }

    async fn assert_body_eq<T>(&mut self, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug + Serialize,
    {
        let response_stream = self.response.body_mut();
        let responses: Vec<_> = response_stream.collect().await;
        pretty_assertions::assert_eq!(
            serde_yaml::to_string(&responses).expect("could not serialize"),
            serde_yaml::to_string(&value).expect("could not serialize")
        );
    }

    async fn assert_contains_error(&mut self, error: &Value) {
        let responses: Vec<graphql::Response> = self.response.body_mut().collect::<Vec<_>>().await;
        let errors: Vec<Value> = responses.iter().fold(Vec::new(), |mut errors, r| {
            errors.append(
                &mut r
                    .errors
                    .iter()
                    .map(|e| serde_json::to_value(e).expect("could not serialize error"))
                    .collect::<Vec<_>>(),
            );
            errors
        });
        if !errors.iter().contains(error) {
            panic!(
                "Expected error {}\nActual errors\n{}",
                serde_yaml::to_string(error).expect("error"),
                serde_yaml::to_string(&errors).expect("errors")
            )
        }
    }

    async fn assert_canned_body(&mut self) {
        self.assert_body_eq(canned_response_body_array()).await
    }

    fn assert_status_code(&self, status_code: ::http::StatusCode) {
        pretty_assertions::assert_eq!(
            self.response.status(),
            status_code,
            "http status code mismatch"
        );
    }
}
