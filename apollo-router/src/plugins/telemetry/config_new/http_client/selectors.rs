use derivative::Derivative;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Stage;
use crate::plugins::telemetry::config_new::instruments::InstrumentValue;
use crate::plugins::telemetry::config_new::instruments::Standard;
use crate::services::http;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum HttpClientValue {
    Standard(Standard),
    Custom(HttpClientSelector),
}

impl From<&HttpClientValue> for InstrumentValue<HttpClientSelector> {
    fn from(value: &HttpClientValue) -> Self {
        match value {
            HttpClientValue::Standard(standard) => InstrumentValue::Standard(standard.clone()),
            HttpClientValue::Custom(selector) => InstrumentValue::Custom(selector.clone()),
        }
    }
}

#[derive(Derivative, Deserialize, JsonSchema, Clone)]
#[serde(deny_unknown_fields, untagged)]
#[derivative(Debug, PartialEq)]
pub(crate) enum HttpClientSelector {
    /// A header from the HTTP request
    HttpClientRequestHeader {
        /// The name of the request header.
        request_header: String,
        /// Optional redaction pattern.
        redact: Option<crate::services::header_masking::RedactMode>,
        /// Optional default value.
        default: Option<String>,
    },
    /// A header from the HTTP response
    HttpClientResponseHeader {
        /// The name of the response header.
        response_header: String,
        /// Optional redaction pattern.
        redact: Option<crate::services::header_masking::RedactMode>,
        /// Optional default value.
        default: Option<String>,
    },
}

impl Selector for HttpClientSelector {
    type Request = http::HttpRequest;
    type Response = http::HttpResponse;
    type EventResponse = ();

    fn on_request(&self, request: &http::HttpRequest) -> Option<opentelemetry::Value> {
        match self {
            HttpClientSelector::HttpClientRequestHeader {
                request_header,
                redact,
                default,
            } => {
                let header_value = request
                    .http_request
                    .headers()
                    .get(request_header)
                    .and_then(|h| h.to_str().ok())
                    .map(|h| h.to_string());
                // The http-client layer is a transport shared across all
                // subgraphs/connectors and carries no subgraph identity, so
                // masking here uses the global request rules. Per-subgraph
                // masking overrides are applied at the subgraph/connector
                // telemetry layers, which do know the subgraph. The global
                // rules include the fail-secure defaults, so common secrets are
                // still masked here.
                let value = crate::services::header_masking::redact_header_value(
                    &request.context,
                    crate::services::header_masking::Direction::Request,
                    None,
                    request_header,
                    header_value,
                    redact.as_ref(),
                );
                value
                    .or_else(|| default.clone())
                    .map(opentelemetry::Value::from)
            }
            HttpClientSelector::HttpClientResponseHeader { default, .. } => {
                default.clone().map(opentelemetry::Value::from)
            }
        }
    }

    fn on_response(&self, response: &http::HttpResponse) -> Option<opentelemetry::Value> {
        match self {
            HttpClientSelector::HttpClientRequestHeader { default, .. } => {
                default.clone().map(opentelemetry::Value::from)
            }
            HttpClientSelector::HttpClientResponseHeader {
                response_header,
                redact,
                default,
            } => {
                let header_value = response
                    .http_response
                    .headers()
                    .get(response_header)
                    .and_then(|h| h.to_str().ok())
                    .map(|h| h.to_string());
                // The http-client layer is a transport shared across all
                // subgraphs/connectors and carries no subgraph identity, so
                // masking here uses the global response rules. Per-subgraph
                // masking overrides are applied at the subgraph/connector
                // telemetry layers, which do know the subgraph. The global
                // rules include the fail-secure defaults, so common secrets are
                // still masked here.
                let value = crate::services::header_masking::redact_header_value(
                    &response.context,
                    crate::services::header_masking::Direction::Response,
                    None,
                    response_header,
                    header_value,
                    redact.as_ref(),
                );
                value
                    .or_else(|| default.clone())
                    .map(opentelemetry::Value::from)
            }
        }
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Option<opentelemetry::Value> {
        match self {
            HttpClientSelector::HttpClientRequestHeader { default, .. } => {
                default.clone().map(opentelemetry::Value::from)
            }
            HttpClientSelector::HttpClientResponseHeader { default, .. } => {
                default.clone().map(opentelemetry::Value::from)
            }
        }
    }

    fn is_active(&self, stage: Stage) -> bool {
        match self {
            HttpClientSelector::HttpClientRequestHeader { .. } => matches!(stage, Stage::Request),
            HttpClientSelector::HttpClientResponseHeader { .. } => matches!(stage, Stage::Response),
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use super::*;
    use crate::Context;

    #[test]
    fn test_http_client_request_header() {
        let selector = HttpClientSelector::HttpClientRequestHeader {
            request_header: "content-type".to_string(),
            redact: None,
            default: None,
        };

        let http_request = ::http::Request::builder()
            .method(::http::Method::GET)
            .uri("http://localhost/graphql")
            .header("content-type", "application/json")
            .body(crate::services::router::body::empty())
            .unwrap();

        let request = http::HttpRequest {
            http_request,
            context: Context::new(),
        };

        assert_eq!(
            selector.on_request(&request),
            Some(opentelemetry::Value::String(
                "application/json".to_string().into()
            ))
        );
    }

    #[test]
    fn http_client_request_header_masks_via_global_rules() {
        use std::collections::HashMap;

        use crate::configuration::header_masking_config::HeaderMaskingConfig;
        use crate::services::header_masking::DirectionRules;
        use crate::services::header_masking::HeaderMaskingRules;
        use crate::services::header_masking::MaskingRulesMap;

        let selector = HttpClientSelector::HttpClientRequestHeader {
            request_header: "authorization".to_string(),
            redact: None,
            default: None,
        };

        let context = Context::new();
        let rules = Arc::new(HeaderMaskingRules::from_config(&HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["authorization".to_string()],
            replace_defaults: false,
        }));
        let map = Arc::new(MaskingRulesMap::new(
            DirectionRules::new(rules.clone(), HashMap::new()),
            DirectionRules::new(rules, HashMap::new()),
        ));
        context.extensions().with_lock(|lock| lock.insert(map));

        let http_request = ::http::Request::builder()
            .method(::http::Method::GET)
            .uri("http://localhost/graphql")
            .header("authorization", "Bearer secret") // gitleaks:allow
            .body(crate::services::router::body::empty())
            .unwrap();
        let request = http::HttpRequest {
            http_request,
            context,
        };
        assert_eq!(
            selector.on_request(&request),
            Some(opentelemetry::Value::String("***MASKED***".into()))
        );
    }

    #[test]
    fn http_client_request_header_redact_allow_bypasses_global_rules() {
        use std::collections::HashMap;

        use crate::configuration::header_masking_config::HeaderMaskingConfig;
        use crate::services::header_masking::DirectionRules;
        use crate::services::header_masking::HeaderMaskingRules;
        use crate::services::header_masking::MaskingRulesMap;

        let selector = HttpClientSelector::HttpClientRequestHeader {
            request_header: "authorization".to_string(),
            redact: Some(crate::services::header_masking::RedactMode::Allow),
            default: None,
        };

        let context = Context::new();
        let rules = Arc::new(HeaderMaskingRules::from_config(&HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["authorization".to_string()],
            replace_defaults: false,
        }));
        let map = Arc::new(MaskingRulesMap::new(
            DirectionRules::new(rules.clone(), HashMap::new()),
            DirectionRules::new(rules, HashMap::new()),
        ));
        context.extensions().with_lock(|lock| lock.insert(map));

        let http_request = ::http::Request::builder()
            .method(::http::Method::GET)
            .uri("http://localhost/graphql")
            .header("authorization", "Bearer secret") // gitleaks:allow
            .body(crate::services::router::body::empty())
            .unwrap();
        let request = http::HttpRequest {
            http_request,
            context,
        };
        assert_eq!(
            selector.on_request(&request),
            Some(opentelemetry::Value::String("Bearer secret".into()))
        );
    }

    #[test]
    fn test_http_client_response_header() {
        let selector = HttpClientSelector::HttpClientResponseHeader {
            response_header: "content-length".to_string(),
            redact: None,
            default: None,
        };

        let http_response = ::http::Response::builder()
            .status(200)
            .header("content-length", "1024")
            .body(crate::services::router::body::empty())
            .unwrap();

        let response = http::HttpResponse {
            http_response,
            context: Context::new(),
        };

        assert_eq!(
            selector.on_response(&response),
            Some(opentelemetry::Value::String("1024".to_string().into()))
        );
    }
}
