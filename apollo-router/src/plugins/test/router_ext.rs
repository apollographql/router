use std::fmt::Debug;

use http_body_util::BodyExt;
use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::plugins::test::RequestTestExt;
use crate::plugins::test::ResponseTestExt;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::router;

fn canned_request_body() -> Value {
    json!({
            "query":"query SimpleQuery {\ntopProducts {\n  name\n  price\n   \n}\n}"
    })
}

fn canned_response_body() -> Value {
    json!({
        "data": {
            "field": "value"
        }
    })
}

impl RequestTestExt<router::Request, router::Response> for RouterRequest {
    fn canned_request() -> router::Request {
        router::Request::fake_builder()
            .body(canned_request_body().to_string())
            .build()
            .expect("canned request")
    }

    fn canned_result(self) -> router::ServiceResult {
        router::Response::fake_builder()
            .context(self.context.clone())
            .data(json! ({"field": "value"}))
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
            .router_request
            .headers()
            .get(key)
            .unwrap_or_else(|| panic!("header '{key}' not found"));
        pretty_assertions::assert_eq!(header_value, value, "header '{}' value mismatch", key);
    }

    async fn assert_body_eq<T>(&mut self, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug + Serialize,
    {
        let body_value = self
            .router_request
            .body_mut()
            .collect()
            .await
            .expect("no body");
        let body_bytes = body_value.to_bytes();
        if body_bytes.is_empty() {
            panic!("body value is empty");
        }
        let body_value = serde_json::from_slice::<serde_json::Value>(&body_bytes)
            .expect("body value not deserializable");
        let expected_value = serde_json::to_value(value).expect("expected value not serializable");
        pretty_assertions::assert_eq!(
            serde_yaml::to_string(&body_value).expect("could not serialize"),
            serde_yaml::to_string(&expected_value).expect("could not serialize")
        );
    }

    async fn assert_canned_body(&mut self) {
        self.assert_body_eq(canned_request_body()).await
    }
}

impl ResponseTestExt for RouterResponse {
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
        let body_value = self.response.body_mut().collect().await.expect("no body");
        let body_bytes = body_value.to_bytes();
        if body_bytes.is_empty() {
            panic!("body value is empty");
        }
        let body_value = serde_json::from_slice::<serde_json::Value>(&body_bytes)
            .expect("body value not deserializable");
        let expected_value = serde_json::to_value(value).expect("expected value not serializable");
        pretty_assertions::assert_eq!(
            serde_yaml::to_string(&body_value).expect("could not serialize"),
            serde_yaml::to_string(&expected_value).expect("could not serialize")
        );
    }

    async fn assert_contains_error(&mut self, error: &Value) {
        let body_value = self.response.body_mut().collect().await.expect("no body");
        let body_bytes = body_value.to_bytes();
        if body_bytes.is_empty() {
            panic!("body value is empty");
        }
        let body_value = serde_json::from_slice::<serde_json::Value>(&body_bytes)
            .expect("body value not deserializable");

        let errors = body_value
            .get("errors")
            .expect("errors not found")
            .as_array()
            .expect("expected object");
        if !errors.iter().contains(error) {
            panic!(
                "Expected error {}\nActual errors\n{}",
                serde_yaml::to_string(error).expect("error"),
                serde_yaml::to_string(errors).expect("errors")
            )
        }
    }

    async fn assert_canned_body(&mut self) {
        self.assert_body_eq(canned_response_body()).await
    }

    fn assert_status_code(&self, status_code: ::http::StatusCode) {
        pretty_assertions::assert_eq!(
            self.response.status(),
            status_code,
            "http status code mismatch"
        );
    }
}
