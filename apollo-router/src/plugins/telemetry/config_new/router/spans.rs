use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditional::Conditional;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::router::selectors::RouterSelector;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterSpans {
    /// Custom attributes that are attached to the router span.
    pub(crate) attributes: Extendable<RouterAttributes, Conditional<RouterSelector>>,
}

impl DefaultForLevel for RouterSpans {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.attributes.defaults_for_level(requirement_level, kind);

        // When a user overrides client.name or client.version with a custom
        // selector (e.g. `request_header`), the Extendable deserializer places
        // it in `custom` and leaves the typed attribute field as None.
        // defaults_for_level then mistakenly fills that None with Some(true).
        // Clear it back so the custom selector is the sole source for the key.
        if self.attributes.custom.contains_key("client.name") {
            self.attributes.attributes.client_name = None;
        }
        if self.attributes.custom.contains_key("client.version") {
            self.attributes.attributes.client_version = None;
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use http::header::USER_AGENT;
    use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
    use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_VERSION;
    use opentelemetry_semantic_conventions::trace::URL_PATH;
    use opentelemetry_semantic_conventions::trace::USER_AGENT_ORIGINAL;
    use parking_lot::Mutex;

    use super::RouterSpans;
    use crate::Context;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::plugins::telemetry::OTEL_NAME;
    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::config_new::DefaultForLevel;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
    use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
    use crate::plugins::telemetry::config_new::conditional::Conditional;
    use crate::plugins::telemetry::config_new::conditions::Condition;
    use crate::plugins::telemetry::config_new::conditions::SelectorOrValue;
    use crate::plugins::telemetry::config_new::router::selectors::RouterSelector;
    use crate::plugins::telemetry::otlp::TelemetryDataKind;
    use crate::services::router;

    #[test]
    fn test_router_spans_level_none() {
        let mut spans = RouterSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::None,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header(USER_AGENT, "test")
                .build()
                .unwrap(),
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == NETWORK_PROTOCOL_VERSION)
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == URL_PATH)
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == USER_AGENT_ORIGINAL)
        );
    }

    #[test]
    fn test_router_spans_level_required() {
        let mut spans = RouterSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Required,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header(USER_AGENT, "test")
                .build()
                .unwrap(),
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == NETWORK_PROTOCOL_VERSION)
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == URL_PATH)
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == USER_AGENT_ORIGINAL)
        );
    }

    #[test]
    fn test_router_spans_level_recommended() {
        let mut spans = RouterSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Recommended,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header(USER_AGENT, "test")
                .build()
                .unwrap(),
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == NETWORK_PROTOCOL_VERSION)
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == URL_PATH)
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == USER_AGENT_ORIGINAL)
        );
    }

    #[test]
    fn test_router_request_static_custom_attribute_on_graphql_error() {
        let mut spans = RouterSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: RouterSelector::StaticField {
                    r#static: "my-static-value".to_string().into(),
                },
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(AttributeValue::Bool(true)),
                    SelectorOrValue::Selector(RouterSelector::OnGraphQLError {
                        on_graphql_error: true,
                    }),
                ])))),
                value: Arc::new(Default::default()),
            },
        );
        let context = Context::new();
        context.insert_json_value(CONTAINS_GRAPHQL_ERROR, serde_json_bytes::Value::Bool(true));
        let values = spans.attributes.on_response(
            &router::Response::fake_builder()
                .header("my-header", "test_val")
                .context(context)
                .build()
                .unwrap(),
        );
        assert!(values.iter().any(|key_val| key_val.key
            == opentelemetry::Key::from_static_str("test")
            && key_val.value
                == opentelemetry::Value::String("my-static-value".to_string().into())));
    }

    #[test]
    fn test_router_request_custom_attribute_on_graphql_error() {
        let mut spans = RouterSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: RouterSelector::ResponseHeader {
                    response_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(AttributeValue::Bool(true)),
                    SelectorOrValue::Selector(RouterSelector::OnGraphQLError {
                        on_graphql_error: true,
                    }),
                ])))),
                value: Arc::new(Default::default()),
            },
        );
        let context = Context::new();
        context.insert_json_value(CONTAINS_GRAPHQL_ERROR, serde_json_bytes::Value::Bool(true));
        let values = spans.attributes.on_response(
            &router::Response::fake_builder()
                .header("my-header", "test_val")
                .context(context)
                .build()
                .unwrap(),
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }

    #[test]
    fn test_router_request_custom_attribute_not_on_graphql_error_context_false() {
        let mut spans = RouterSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: RouterSelector::ResponseHeader {
                    response_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(AttributeValue::Bool(true)),
                    SelectorOrValue::Selector(RouterSelector::OnGraphQLError {
                        on_graphql_error: true,
                    }),
                ])))),
                value: Arc::new(Default::default()),
            },
        );
        let context = Context::new();
        let values = spans.attributes.on_response(
            &router::Response::fake_builder()
                .header("my-header", "test_val")
                .context(context)
                .build()
                .unwrap(),
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }

    #[test]
    fn test_router_request_custom_attribute_not_on_graphql_error_context_missing() {
        let mut spans = RouterSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: RouterSelector::ResponseHeader {
                    response_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(AttributeValue::Bool(true)),
                    SelectorOrValue::Selector(RouterSelector::OnGraphQLError {
                        on_graphql_error: true,
                    }),
                ])))),
                value: Arc::new(Default::default()),
            },
        );
        let context = Context::new();
        let values = spans.attributes.on_response(
            &router::Response::fake_builder()
                .header("my-header", "test_val")
                .context(context)
                .build()
                .unwrap(),
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }

    #[test]
    fn test_router_request_custom_attribute_condition_true() {
        let mut spans = RouterSpans::default();
        let selector = RouterSelector::RequestHeader {
            request_header: "my-header".to_string(),
            redact: None,
            default: None,
        };
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: selector.clone(),
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(AttributeValue::String("test_val".to_string())),
                    SelectorOrValue::Selector(selector),
                ])))),
                value: Default::default(),
            },
        );
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header("my-header", "test_val")
                .build()
                .unwrap(),
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }

    #[test]
    fn test_router_request_custom_attribute_condition_false() {
        let mut spans = RouterSpans::default();
        let selector = RouterSelector::RequestHeader {
            request_header: "my-header".to_string(),
            redact: None,
            default: None,
        };
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: selector.clone(),
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(AttributeValue::String("test_val".to_string())),
                    SelectorOrValue::Selector(selector),
                ])))),
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header("my-header", "bar")
                .build()
                .unwrap(),
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }

    #[test]
    fn test_router_request_custom_attribute() {
        let mut spans = RouterSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: RouterSelector::RequestHeader {
                    request_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: None,
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header("my-header", "test_val")
                .build()
                .unwrap(),
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }

    #[test]
    fn test_router_request_standard_attribute_aliased() {
        let mut spans = RouterSpans::default();
        spans.attributes.attributes.common.http_request_method = Some(StandardAttribute::Aliased {
            alias: String::from("my.method"),
        });
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header("my-header", "test_val")
                .build()
                .unwrap(),
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("my.method"))
        );
    }

    #[test]
    fn test_router_response_custom_attribute() {
        let mut spans = RouterSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: RouterSelector::ResponseHeader {
                    response_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: None,
                value: Arc::new(Default::default()),
            },
        );
        spans.attributes.custom.insert(
            OTEL_NAME.to_string(),
            Conditional {
                selector: RouterSelector::StaticField {
                    r#static: String::from("new_name").into(),
                },
                condition: None,
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_response(
            &router::Response::fake_builder()
                .header("my-header", "test_val")
                .build()
                .unwrap(),
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );

        assert!(values.iter().any(|key_val| key_val.key
            == opentelemetry::Key::from_static_str(OTEL_NAME)
            && key_val.value == opentelemetry::Value::String(String::from("new_name").into())));
    }

    #[test]
    fn test_defaults_do_not_duplicate_custom_client_name_selector() {
        use crate::plugins::telemetry::config_new::attributes::StandardAttribute;

        let json = serde_json::json!({
            "attributes": {
                "client.name": {
                    "request_header": "x-custom-client"
                }
            }
        });

        let mut spans: RouterSpans = serde_json::from_value(json).expect("should deserialize");

        assert!(spans.attributes.attributes.client_name.is_none());
        assert!(spans.attributes.custom.contains_key("client.name"));

        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Required,
            TelemetryDataKind::Traces,
        );

        assert!(
            spans.attributes.attributes.client_name.is_none(),
            "defaults_for_level must not set client_name when a custom selector already covers it"
        );

        // client.version has no custom selector, so it should still get the default.
        assert_eq!(
            spans.attributes.attributes.client_version,
            Some(StandardAttribute::Bool(true)),
        );

        let request = router::Request::fake_builder()
            .header("x-custom-client", "from-selector")
            .build()
            .unwrap();
        let attrs = spans.attributes.on_request(&request);
        let client_names: Vec<_> = attrs
            .iter()
            .filter(|kv| kv.key.as_str() == "client.name")
            .collect();

        assert_eq!(
            client_names.len(),
            1,
            "on_request should produce exactly one client.name (from the custom selector), got {client_names:?}"
        );
    }
}
