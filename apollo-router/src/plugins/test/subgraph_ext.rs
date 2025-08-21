use std::fmt::Debug;

use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::graphql;
use crate::plugins::test::RequestTestExt;
use crate::plugins::test::ResponseTestExt;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::subgraph;

fn canned_request_body() -> Value {
    json!({
            "query":"query SimpleQuery {\ntopProducts {\n  name\n  price\n   \n}\n}"
    })
}

fn canned_query() -> &'static str {
    "query SimpleQuery {\ntopProducts {\n  name\n  price\n   \n}\n}"
}

fn canned_response_body() -> Value {
    json!({
        "field": "value"
    })
}

fn canned_response_body_data() -> Value {
    json!({
        "data": {
            "field": "value"
        }
    })
}

impl RequestTestExt<subgraph::Request, subgraph::Response> for SubgraphRequest {
    fn canned_request() -> subgraph::Request {
        subgraph::Request::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(graphql::Request::builder().query(canned_query()).build())
                    .expect("canned request"),
            )
            .build()
    }

    fn canned_result(self) -> subgraph::ServiceResult {
        Ok(subgraph::Response::fake_builder()
            .context(self.context.clone())
            .data(canned_response_body())
            .build())
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
            .subgraph_request
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
            serde_yaml::to_string(&self.subgraph_request.body()).expect("could not serialize"),
            serde_yaml::to_string(&value).expect("could not serialize")
        );
    }

    async fn assert_canned_body(&mut self) {
        self.assert_body_eq(canned_request_body()).await
    }
}

impl ResponseTestExt for SubgraphResponse {
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
        pretty_assertions::assert_eq!(
            serde_yaml::to_string(&self.response.body_mut()).expect("could not serialize"),
            serde_yaml::to_string(&value).expect("could not serialize")
        );
    }

    async fn assert_contains_error(&mut self, error: &Value) {
        let errors = &self.response.body().errors;
        let serialized = serde_json::to_value(errors).expect("could not serialize");
        let serialized_errors = serialized.as_array().expect("expected array");
        if !serialized_errors.iter().contains(error) {
            panic!(
                "Expected error {}\nActual errors\n{}",
                serde_yaml::to_string(error).expect("error"),
                serde_yaml::to_string(errors).expect("errors")
            )
        }
    }

    async fn assert_canned_body(&mut self) {
        self.assert_body_eq(canned_response_body_data()).await
    }

    fn assert_status_code(&self, status_code: ::http::StatusCode) {
        pretty_assertions::assert_eq!(
            self.response.status(),
            status_code,
            "http status code mismatch"
        );
    }
}
