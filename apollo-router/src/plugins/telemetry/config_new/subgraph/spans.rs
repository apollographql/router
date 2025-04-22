use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditional::Conditional;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

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

    use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
    use parking_lot::Mutex;

    use super::SubgraphSpans;
    use crate::graphql;
    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::config_new::DefaultForLevel;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
    use crate::plugins::telemetry::config_new::conditional::Conditional;
    use crate::plugins::telemetry::config_new::conditions::Condition;
    use crate::plugins::telemetry::config_new::conditions::SelectorOrValue;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::plugins::telemetry::config_new::subgraph::attributes::SUBGRAPH_GRAPHQL_DOCUMENT;
    use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
    use crate::plugins::telemetry::otlp::TelemetryDataKind;
    use crate::services::subgraph;

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
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == GRAPHQL_DOCUMENT)
        );
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
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == GRAPHQL_DOCUMENT)
        );
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
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == SUBGRAPH_GRAPHQL_DOCUMENT)
        );
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
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
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
                .subgraph_name(String::default())
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
                .subgraph_name(String::default())
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
                .subgraph_name(String::default())
                .build()
                .unwrap(),
        );
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }
}
