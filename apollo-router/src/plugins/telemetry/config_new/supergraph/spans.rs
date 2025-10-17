use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditional::Conditional;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::supergraph::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::supergraph::selectors::SupergraphSelector;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

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

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::sync::Arc;

    use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
    use parking_lot::Mutex;
    use serde_json_bytes::path::JsonPathInst;

    use super::SupergraphSpans;
    use crate::Context;
    use crate::context::OPERATION_KIND;
    use crate::graphql;
    use crate::plugins::telemetry::config_new::DefaultForLevel;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
    use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
    use crate::plugins::telemetry::config_new::conditional::Conditional;
    use crate::plugins::telemetry::config_new::conditions::Condition;
    use crate::plugins::telemetry::config_new::supergraph::selectors::SupergraphSelector;
    use crate::plugins::telemetry::otlp::TelemetryDataKind;
    use crate::services::supergraph;

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
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == GRAPHQL_DOCUMENT)
        );
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
        assert!(
            !values
                .iter()
                .any(|key_val| key_val.key.as_str() == GRAPHQL_DOCUMENT)
        );
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
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key.as_str() == GRAPHQL_DOCUMENT)
        );
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
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }

    #[test]
    fn test_supergraph_standard_attribute_aliased() {
        let mut spans = SupergraphSpans::default();
        spans.attributes.attributes.graphql_operation_type = Some(StandardAttribute::Aliased {
            alias: String::from("my_op"),
        });
        let context = Context::new();
        context.insert(OPERATION_KIND, "Query".to_string()).unwrap();
        let values = spans.attributes.on_request(
            &supergraph::Request::fake_builder()
                .method(http::Method::POST)
                .header("my-header", "test_val")
                .query("Query { me { id } }")
                .context(context)
                .build()
                .unwrap(),
        );
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("my_op"))
        );
    }

    #[test]
    fn test_supergraph_response_event_custom_attribute() {
        let mut spans = SupergraphSpans::default();
        spans.attributes.custom.insert(
            "otel.status_code".to_string(),
            Conditional {
                selector: SupergraphSelector::StaticField {
                    r#static: String::from("error").into(),
                },
                condition: Some(Arc::new(Mutex::new(Condition::Exists(
                    SupergraphSelector::ResponseErrors {
                        response_errors: JsonPathInst::from_str("$[0].extensions.code").unwrap(),
                        redact: None,
                        default: None,
                    },
                )))),
                value: Arc::new(Default::default()),
            },
        );
        let values = spans.attributes.on_response_event(
            &graphql::Response::builder()
                .error(
                    graphql::Error::builder()
                        .message("foo")
                        .extension_code("MY_EXTENSION_CODE")
                        .build(),
                )
                .build(),
            &Context::new(),
        );
        assert!(values.iter().any(|key_val| key_val.key
            == opentelemetry::Key::from_static_str("otel.status_code")
            && key_val.value == opentelemetry::Value::String(String::from("error").into())));
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
        assert!(
            values
                .iter()
                .any(|key_val| key_val.key == opentelemetry::Key::from_static_str("test"))
        );
    }
}
