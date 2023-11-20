use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::DefaultForLevel;
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
        self.router
            .defaults_for_levels(self.default_attribute_requirement_level);
        self.supergraph
            .defaults_for_levels(self.default_attribute_requirement_level);
        self.subgraph
            .defaults_for_levels(self.default_attribute_requirement_level);
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterSpans {
    /// Custom attributes that are attached to the router span.
    pub(crate) attributes: Extendable<RouterAttributes, RouterSelector>,
}

impl DefaultForLevel for RouterSpans {
    fn defaults_for_level(&mut self, requirement_level: DefaultAttributeRequirementLevel) {
        self.attributes.defaults_for_level(requirement_level);
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphSpans {
    /// Custom attributes that are attached to the supergraph span.
    pub(crate) attributes: Extendable<SupergraphAttributes, SupergraphSelector>,
}
impl DefaultForLevel for SupergraphSpans {
    fn defaults_for_level(&mut self, requirement_level: DefaultAttributeRequirementLevel) {
        self.attributes.defaults_for_level(requirement_level);
    }
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphSpans {
    /// Custom attributes that are attached to the subgraph span.
    pub(crate) attributes: Extendable<SubgraphAttributes, SubgraphSelector>,
}

impl DefaultForLevel for SubgraphSpans {
    fn defaults_for_level(&mut self, requirement_level: DefaultAttributeRequirementLevel) {
        self.attributes.defaults_for_level(requirement_level);
    }
}

#[cfg(test)]
mod test {
    use http::header::USER_AGENT;
    use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
    use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
    use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_VERSION;
    use opentelemetry_semantic_conventions::trace::URL_PATH;
    use opentelemetry_semantic_conventions::trace::USER_AGENT_ORIGINAL;

    use crate::graphql;
    use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
    use crate::plugins::telemetry::config_new::attributes::SUBGRAPH_GRAPHQL_DOCUMENT;
    use crate::plugins::telemetry::config_new::spans::RouterSpans;
    use crate::plugins::telemetry::config_new::spans::SubgraphSpans;
    use crate::plugins::telemetry::config_new::spans::SupergraphSpans;
    use crate::plugins::telemetry::config_new::DefaultForLevel;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::services::router;
    use crate::services::subgraph;
    use crate::services::supergraph;

    #[test]
    fn test_router_spans_level_none() {
        let mut spans = RouterSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::None);
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header(USER_AGENT, "test")
                .build()
                .unwrap(),
        );
        assert!(values.get(&HTTP_REQUEST_METHOD).is_none());
        assert!(values.get(&NETWORK_PROTOCOL_VERSION).is_none());
        assert!(values.get(&URL_PATH).is_none());
        assert!(values.get(&USER_AGENT_ORIGINAL).is_none());
    }

    #[test]
    fn test_router_spans_level_required() {
        let mut spans = RouterSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::Required);
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header(USER_AGENT, "test")
                .build()
                .unwrap(),
        );
        assert!(values.get(&HTTP_REQUEST_METHOD).is_some());
        assert!(values.get(&NETWORK_PROTOCOL_VERSION).is_none());
        assert!(values.get(&URL_PATH).is_some());
        assert!(values.get(&USER_AGENT_ORIGINAL).is_none());
    }

    #[test]
    fn test_router_spans_level_recommended() {
        let mut spans = RouterSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::Recommended);
        let values = spans.attributes.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .header(USER_AGENT, "test")
                .build()
                .unwrap(),
        );
        assert!(values.get(&HTTP_REQUEST_METHOD).is_some());
        assert!(values.get(&NETWORK_PROTOCOL_VERSION).is_some());
        assert!(values.get(&URL_PATH).is_some());
        assert!(values.get(&USER_AGENT_ORIGINAL).is_some());
    }

    #[test]
    fn test_supergraph_spans_level_none() {
        let mut spans = SupergraphSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::None);
        let values = spans.attributes.on_request(
            &supergraph::Request::fake_builder()
                .query("query { __typename }")
                .build()
                .unwrap(),
        );
        assert!(values.get(&GRAPHQL_DOCUMENT).is_none());
    }

    #[test]
    fn test_supergraph_spans_level_required() {
        let mut spans = SupergraphSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::Required);
        let values = spans.attributes.on_request(
            &supergraph::Request::fake_builder()
                .query("query { __typename }")
                .build()
                .unwrap(),
        );
        assert!(values.get(&GRAPHQL_DOCUMENT).is_none());
    }

    #[test]
    fn test_supergraph_spans_level_recommended() {
        let mut spans = SupergraphSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::Recommended);
        let values = spans.attributes.on_request(
            &supergraph::Request::fake_builder()
                .query("query { __typename }")
                .build()
                .unwrap(),
        );
        assert!(values.get(&GRAPHQL_DOCUMENT).is_some());
    }

    #[test]
    fn test_subgraph_spans_level_none() {
        let mut spans = SubgraphSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::None);
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
        assert!(values.get(&GRAPHQL_DOCUMENT).is_none());
    }

    #[test]
    fn test_subgraph_spans_level_required() {
        let mut spans = SubgraphSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::Required);
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
        assert!(values.get(&GRAPHQL_DOCUMENT).is_none());
    }

    #[test]
    fn test_subgraph_spans_level_recommended() {
        let mut spans = SubgraphSpans::default();
        spans.defaults_for_levels(DefaultAttributeRequirementLevel::Recommended);
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
        assert!(values.get(&SUBGRAPH_GRAPHQL_DOCUMENT).is_some());
    }
}
