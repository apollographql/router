use std::fmt::Debug;

use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::http;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpClientAttributes {
    /// HTTP request method.
    /// Examples:
    ///
    /// * GET
    /// * POST
    /// * HEAD
    ///
    /// Requirement level: Required
    #[serde(rename = "http.request.method")]
    pub(crate) http_request_method: Option<StandardAttribute>,
}

impl DefaultForLevel for HttpClientAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
            }
            DefaultAttributeRequirementLevel::Recommended => {
                if self.http_request_method.is_none() {
                    self.http_request_method = Some(StandardAttribute::Bool(true));
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl Selectors<http::HttpRequest, http::HttpResponse, ()> for HttpClientAttributes {
    fn on_request(&self, request: &http::HttpRequest) -> Vec<KeyValue> {
        let mut attrs = Vec::new();

        if let Some(key) = self
            .http_request_method
            .as_ref()
            .and_then(|a| a.key(HTTP_REQUEST_METHOD.into()))
        {
            attrs.push(KeyValue::new(
                key,
                request.http_request.method().as_str().to_string(),
            ));
        }

        attrs
    }

    fn on_response(&self, _response: &http::HttpResponse) -> Vec<KeyValue> {
        Vec::new()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::new()
    }
}

#[cfg(test)]
mod test {
    use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;

    use super::*;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::services::http::HttpRequest;

    #[test]
    fn test_http_client_request_method() {
        let attributes = HttpClientAttributes {
            http_request_method: Some(StandardAttribute::Bool(true)),
        };

        let http_request = ::http::Request::builder()
            .method(::http::Method::POST)
            .uri("http://localhost/graphql")
            .body(crate::services::router::body::empty())
            .unwrap();

        let request = HttpRequest {
            http_request,
            context: crate::Context::new(),
        };

        let attributes = attributes.on_request(&request);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
                .map(|key_val| &key_val.value),
            Some(&"POST".into())
        );
    }

    #[test]
    fn test_http_client_request_method_aliased() {
        let attributes = HttpClientAttributes {
            http_request_method: Some(StandardAttribute::Aliased {
                alias: "custom.request.method".to_string(),
            }),
        };

        let http_request = ::http::Request::builder()
            .method(::http::Method::GET)
            .uri("http://localhost/graphql")
            .body(crate::services::router::body::empty())
            .unwrap();

        let request = HttpRequest {
            http_request,
            context: crate::Context::new(),
        };

        let attributes = attributes.on_request(&request);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == "custom.request.method")
                .map(|key_val| &key_val.value),
            Some(&"GET".into())
        );
    }
}
