use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditional::Conditional;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::http_client::attributes::HttpClientAttributes;
use crate::plugins::telemetry::config_new::http_client::selectors::HttpClientSelector;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpClientSpans {
    /// Custom attributes that are attached to the HTTP client span.
    pub(crate) attributes: Extendable<HttpClientAttributes, Conditional<HttpClientSelector>>,
}

impl DefaultForLevel for HttpClientSpans {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.attributes.defaults_for_level(requirement_level, kind);
    }
}

#[cfg(test)]
mod test {
    use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;

    use super::*;
    use crate::plugins::telemetry::config_new::DefaultForLevel;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
    use crate::plugins::telemetry::otlp::TelemetryDataKind;
    use crate::services::http::HttpRequest;
    use crate::Context;

    #[test]
    fn test_http_client_spans_level_none() {
        let mut spans = HttpClientSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::None,
            TelemetryDataKind::Traces,
        );

        let http_request = ::http::Request::builder()
            .method(::http::Method::POST)
            .uri("http://localhost/graphql")
            .body(crate::services::router::body::empty())
            .unwrap();

        let request = HttpRequest {
            http_request,
            context: Context::new(),
        };

        let values = spans.attributes.on_request(&request);
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
        );
    }

    #[test]
    fn test_http_client_spans_level_required() {
        let mut spans = HttpClientSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Required,
            TelemetryDataKind::Traces,
        );

        let http_request = ::http::Request::builder()
            .method(::http::Method::POST)
            .uri("http://localhost/graphql")
            .body(crate::services::router::body::empty())
            .unwrap();

        let request = HttpRequest {
            http_request,
            context: Context::new(),
        };

        let values = spans.attributes.on_request(&request);
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
        );
        assert_eq!(
            values
                .iter()
                .find(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
                .map(|key_val| &key_val.value),
            Some(&"POST".into())
        );
    }

    #[test]
    fn test_http_client_spans_level_recommended() {
        let mut spans = HttpClientSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Recommended,
            TelemetryDataKind::Traces,
        );

        let http_request = ::http::Request::builder()
            .method(::http::Method::GET)
            .uri("http://localhost/graphql")
            .body(crate::services::router::body::empty())
            .unwrap();

        let request = HttpRequest {
            http_request,
            context: Context::new(),
        };

        let values = spans.attributes.on_request(&request);
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
        );
        assert_eq!(
            values
                .iter()
                .find(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
                .map(|key_val| &key_val.value),
            Some(&"GET".into())
        );
    }
}