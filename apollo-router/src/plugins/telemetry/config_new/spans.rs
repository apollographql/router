use schemars::JsonSchema;
use serde::Deserialize;

use super::conditional::Conditional;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::span_factory::SpanMode;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Spans {
    /// Use new OpenTelemetry spec compliant span attributes or preserve existing. This will be defaulted in future to `spec_compliant`, eventually removed in future.
    pub(crate) mode: SpanMode,

    /// The attributes to include by default in spans based on their level as specified in the otel semantic conventions and Apollo documentation.
    pub(crate) default_attribute_requirement_level: DefaultAttributeRequirementLevel,

    /// Configuration of router spans.
    /// Log events inherit attributes from the containing span, so attributes configured here will be included on log events for a request.
    /// Router spans contain http request and response information and therefore contain http specific attributes.
    pub(crate) router: RouterSpans,

    /// Configuration of supergraph spans.
    /// Supergraph spans contain information about the graphql request and response and therefore contain graphql specific attributes.
    pub(crate) supergraph: SupergraphSpans,

    /// Attributes to include on the subgraph span.
    /// Subgraph spans contain information about the subgraph request and response and therefore contain subgraph specific attributes.
    pub(crate) subgraph: SubgraphSpans,
}

impl Spans {
    /// Update the defaults for spans configuration regarding the `default_attribute_requirement_level`
    pub(crate) fn update_defaults(&mut self) {
        self.router.defaults_for_levels(
            self.default_attribute_requirement_level,
            TelemetryDataKind::Traces,
        );
        self.supergraph.defaults_for_levels(
            self.default_attribute_requirement_level,
            TelemetryDataKind::Traces,
        );
        self.subgraph.defaults_for_levels(
            self.default_attribute_requirement_level,
            TelemetryDataKind::Traces,
        );
    }
}

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
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphSpans {
    /// Custom attributes that are attached to the supergraph span.
    pub(crate) attributes: Extendable<SupergraphAttributes, Conditional<SupergraphSelector>>,
}
impl DefaultForLevel for SupergraphSpans {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.attributes.defaults_for_level(requirement_level, kind);
    }
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphSpans {
    /// Custom attributes that are attached to the subgraph span.
    pub(crate) attributes: Extendable<SubgraphAttributes, Conditional<SubgraphSelector>>,
}

impl DefaultForLevel for SubgraphSpans {
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
    use std::sync::Arc;

    use http::header::USER_AGENT;
    use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
    use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
    use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_VERSION;
    use opentelemetry_semantic_conventions::trace::URL_PATH;
    use opentelemetry_semantic_conventions::trace::USER_AGENT_ORIGINAL;
    use parking_lot::Mutex;

    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::graphql;
    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
    use crate::plugins::telemetry::config_new::attributes::SUBGRAPH_GRAPHQL_DOCUMENT;
    use crate::plugins::telemetry::config_new::conditional::Conditional;
    use crate::plugins::telemetry::config_new::conditions::Condition;
    use crate::plugins::telemetry::config_new::conditions::SelectorOrValue;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::plugins::telemetry::config_new::selectors::RouterSelector;
    use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
    use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
    use crate::plugins::telemetry::config_new::spans::RouterSpans;
    use crate::plugins::telemetry::config_new::spans::SubgraphSpans;
    use crate::plugins::telemetry::config_new::spans::SupergraphSpans;
    use crate::plugins::telemetry::config_new::DefaultForLevel;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::otlp::TelemetryDataKind;
    use crate::services::router;
    use crate::services::subgraph;
    use crate::services::supergraph;
    use crate::Context;

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
        assert!(!values
            .iter()
            .any(|key_val| key_val.key == HTTP_REQUEST_METHOD));
        assert!(!values
            .iter()
            .any(|key_val| key_val.key == NETWORK_PROTOCOL_VERSION));
        assert!(!values.iter().any(|key_val| key_val.key == URL_PATH));
        assert!(!values
            .iter()
            .any(|key_val| key_val.key == USER_AGENT_ORIGINAL));
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
        assert!(values
            .iter()
            .any(|key_val| key_val.key == HTTP_REQUEST_METHOD));
        assert!(!values
            .iter()
            .any(|key_val| key_val.key == NETWORK_PROTOCOL_VERSION));
        assert!(values.iter().any(|key_val| key_val.key == URL_PATH));
        assert!(!values
            .iter()
            .any(|key_val| key_val.key == USER_AGENT_ORIGINAL));
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
        assert!(values
            .iter()
            .any(|key_val| key_val.key == HTTP_REQUEST_METHOD));
        assert!(values
            .iter()
            .any(|key_val| key_val.key == NETWORK_PROTOCOL_VERSION));
        assert!(values.iter().any(|key_val| key_val.key == URL_PATH));
        assert!(values
            .iter()
            .any(|key_val| key_val.key == USER_AGENT_ORIGINAL));
    }

    #[test]
    fn test_supergraph_spans_level_none() {
        let mut spans = SupergraphSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::None,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &supergraph::Request::fake_builder()
                .query("query { __typename }")
                .build()
                .unwrap(),
        );
        assert!(!values.iter().any(|key_val| key_val.key == GRAPHQL_DOCUMENT));
    }

    #[test]
    fn test_supergraph_spans_level_required() {
        let mut spans = SupergraphSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Required,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &supergraph::Request::fake_builder()
                .query("query { __typename }")
                .build()
                .unwrap(),
        );
        assert!(!values.iter().any(|key_val| key_val.key == GRAPHQL_DOCUMENT));
    }

    #[test]
    fn test_supergraph_spans_level_recommended() {
        let mut spans = SupergraphSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Recommended,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &supergraph::Request::fake_builder()
                .query("query { __typename }")
                .build()
                .unwrap(),
        );
        assert!(values.iter().any(|key_val| key_val.key == GRAPHQL_DOCUMENT));
    }

    #[test]
    fn test_subgraph_spans_level_none() {
        let mut spans = SubgraphSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::None,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(
                            graphql::Request::fake_builder()
                                .query("query { __typename }")
                                .build(),
                        )
                        .unwrap(),
                )
                .build(),
        );
        assert!(!values.iter().any(|key_val| key_val.key == GRAPHQL_DOCUMENT));
    }

    #[test]
    fn test_subgraph_spans_level_required() {
        let mut spans = SubgraphSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Required,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(
                            graphql::Request::fake_builder()
                                .query("query { __typename }")
                                .build(),
                        )
                        .unwrap(),
                )
                .build(),
        );
        assert!(!values.iter().any(|key_val| key_val.key == GRAPHQL_DOCUMENT));
    }

    #[test]
    fn test_subgraph_spans_level_recommended() {
        let mut spans = SubgraphSpans::default();
        spans.defaults_for_levels(
            DefaultAttributeRequirementLevel::Recommended,
            TelemetryDataKind::Traces,
        );
        let values = spans.attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(
                            graphql::Request::fake_builder()
                                .query("query { __typename }")
                                .build(),
                        )
                        .unwrap(),
                )
                .build(),
        );
        assert!(values
            .iter()
            .any(|key_val| key_val.key == SUBGRAPH_GRAPHQL_DOCUMENT));
    }

    #[test]
    fn test_router_request_static_custom_attribute_on_graphql_error() {
        let mut spans = RouterSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: RouterSelector::StaticField {
                    r#static: "my-static-value".to_string(),
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
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
    }

    #[test]
    fn test_router_request_custom_attribute_not_on_graphql_error() {
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
        context.insert_json_value(CONTAINS_GRAPHQL_ERROR, serde_json_bytes::Value::Bool(false));
        let values = spans.attributes.on_response(
            &router::Response::fake_builder()
                .header("my-header", "test_val")
                .context(context)
                .build()
                .unwrap(),
        );
        assert!(!values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
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
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
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
        assert!(!values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
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
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
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
        let values = spans.attributes.on_response(
            &router::Response::fake_builder()
                .header("my-header", "test_val")
                .build()
                .unwrap(),
        );
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
    }

    #[test]
    fn test_supergraph_request_custom_attribute() {
        let mut spans = SupergraphSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: SupergraphSelector::RequestHeader {
                    request_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: None,
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_request(
            &supergraph::Request::fake_builder()
                .method(http::Method::POST)
                .header("my-header", "test_val")
                .build()
                .unwrap(),
        );
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
    }

    #[test]
    fn test_supergraph_response_custom_attribute() {
        let mut spans = SupergraphSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: SupergraphSelector::ResponseHeader {
                    response_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: None,
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_response(
            &supergraph::Response::fake_builder()
                .header("my-header", "test_val")
                .build()
                .unwrap(),
        );
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
    }

    #[test]
    fn test_subgraph_request_custom_attribute() {
        let mut spans = SubgraphSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: SubgraphSelector::SubgraphRequestHeader {
                    subgraph_request_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: None,
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .header("my-header", "test_val")
                        .body(
                            graphql::Request::fake_builder()
                                .query("query { __typename }")
                                .build(),
                        )
                        .unwrap(),
                )
                .build(),
        );
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
    }

    #[test]
    fn test_subgraph_response_custom_attribute() {
        let mut spans = SubgraphSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: SubgraphSelector::SubgraphResponseHeader {
                    subgraph_response_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: None,
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_response(
            &subgraph::Response::fake2_builder()
                .header("my-header", "test_val")
                .build()
                .unwrap(),
        );
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
    }

    #[test]
    fn test_subgraph_response_custom_attribute_good_condition() {
        let mut spans = SubgraphSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: SubgraphSelector::SubgraphResponseHeader {
                    subgraph_response_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(AttributeValue::I64(200)),
                    SelectorOrValue::Selector(SubgraphSelector::SubgraphResponseStatus {
                        subgraph_response_status: ResponseStatus::Code,
                    }),
                ])))),
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_response(
            &subgraph::Response::fake2_builder()
                .header("my-header", "test_val")
                .status_code(http::StatusCode::OK)
                .build()
                .unwrap(),
        );
        assert!(values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
    }

    #[test]
    fn test_subgraph_response_custom_attribute_bad_condition() {
        let mut spans = SubgraphSpans::default();
        spans.attributes.custom.insert(
            "test".to_string(),
            Conditional {
                selector: SubgraphSelector::SubgraphResponseHeader {
                    subgraph_response_header: "my-header".to_string(),
                    redact: None,
                    default: None,
                },
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(AttributeValue::I64(400)),
                    SelectorOrValue::Selector(SubgraphSelector::SubgraphResponseStatus {
                        subgraph_response_status: ResponseStatus::Code,
                    }),
                ])))),
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_response(
            &subgraph::Response::fake2_builder()
                .header("my-header", "test_val")
                .status_code(http::StatusCode::OK)
                .build()
                .unwrap(),
        );
        assert!(!values
            .iter()
            .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test")));
    }
}
