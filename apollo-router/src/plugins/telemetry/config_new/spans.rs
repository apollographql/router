use std::collections::HashMap;

use opentelemetry_api::Key;
use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_NAME;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_TYPE;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::context::OPERATION_KIND;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::HttpCommonAttributes;
use crate::plugins::telemetry::config_new::attributes::HttpServerAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::trace_id;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::GetAttributes;
use crate::plugins::telemetry::span_factory::SpanMode;
use crate::query_planner::OperationKind;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::tracer::TraceId;

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Spans {
    /// Whether to create a `request` span if you set `legacy` mode. This will be removed in future, and users should set this to `new`.
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
            .defaults_for_level(&self.default_attribute_requirement_level);
        self.supergraph
            .defaults_for_level(&self.default_attribute_requirement_level);
        self.subgraph
            .defaults_for_level(&self.default_attribute_requirement_level);
    }
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterSpans {
    /// Custom attributes that are attached to the router span.
    pub(crate) attributes: Extendable<RouterAttributes, RouterSelector>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(default)]
pub(crate) struct RouterAttributes {
    /// Attach the datadog trace ID to the router span as dd.trace_id.
    /// This can be output in logs and used to correlate traces in Datadog.
    #[serde(rename = "dd.trace_id")]
    datadog_trace_id: Option<bool>,

    /// Attach the opentelemetry trace ID to the router span as trace_id.
    /// This can be output in logs.
    #[serde(rename = "trace_id")]
    trace_id: Option<bool>,

    /// Span http attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    common: HttpCommonAttributes,
    /// Span http server attributes from Open Telemetry semantic conventions.
    // TODO unskip it
    #[serde(flatten)]
    server: HttpServerAttributes,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphSpans {
    /// Custom attributes that are attached to the supergraph span.
    pub(crate) attributes: Extendable<SupergraphAttributes, SupergraphSelector>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphSpans {
    /// Custom attributes that are attached to the subgraph span.
    pub(crate) attributes: Extendable<SubgraphAttributes, SubgraphSelector>,
}

impl GetAttributes<router::Request, router::Response> for RouterAttributes {
    fn on_request(&self, request: &router::Request) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = self.common.on_request(request);
        if let Some(true) = &self.trace_id {
            if let Some(trace_id) = TraceId::maybe_new().map(|t| t.to_string()) {
                attrs.insert(Key::from_static_str("trace_id"), trace_id.into());
            }
        }
        if let Some(true) = &self.datadog_trace_id {
            if let Some(trace_id) = trace_id() {
                attrs.insert(
                    Key::from_static_str("dd.trace_id"),
                    trace_id.to_datadog().into(),
                );
            }
        }

        attrs
    }

    fn on_response(&self, response: &router::Response) -> HashMap<Key, opentelemetry::Value> {
        self.common.on_response(response)
    }

    fn on_error(&self, error: &BoxError) -> HashMap<Key, opentelemetry::Value> {
        self.common.on_error(error)
    }
}

impl GetAttributes<supergraph::Request, supergraph::Response> for SupergraphAttributes {
    fn on_request(&self, request: &supergraph::Request) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.graphql_document {
            if let Some(query) = &request.supergraph_request.body().query {
                attrs.insert(GRAPHQL_DOCUMENT, query.clone().into());
            }
        }
        if let Some(true) = &self.graphql_operation_name {
            if let Some(op_name) = &request.supergraph_request.body().operation_name {
                attrs.insert(GRAPHQL_OPERATION_NAME, op_name.clone().into());
            }
        }
        if let Some(true) = &self.graphql_operation_type {
            let operation_kind: OperationKind = request
                .context
                .get(OPERATION_KIND)
                .ok()
                .flatten()
                .unwrap_or_default();
            attrs.insert(
                GRAPHQL_OPERATION_TYPE,
                operation_kind.as_apollo_operation_type().clone().into(),
            );
        }

        attrs
    }

    fn on_response(&self, _response: &supergraph::Response) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }

    fn on_error(&self, _error: &BoxError) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }
}

impl GetAttributes<subgraph::Request, subgraph::Response> for SubgraphAttributes {
    fn on_request(&self, request: &subgraph::Request) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.graphql_document {
            if let Some(query) = &request.supergraph_request.body().query {
                attrs.insert(
                    Key::from_static_str("subgraph.graphql.document"),
                    query.clone().into(),
                );
            }
        }
        if let Some(true) = &self.graphql_operation_name {
            if let Some(op_name) = &request.supergraph_request.body().operation_name {
                attrs.insert(
                    Key::from_static_str("subgraph.graphql.operation.name"),
                    op_name.clone().into(),
                );
            }
        }
        if let Some(true) = &self.graphql_operation_type {
            let operation_kind: OperationKind = request
                .context
                .get(OPERATION_KIND)
                .ok()
                .flatten()
                .unwrap_or_default();
            attrs.insert(
                Key::from_static_str("subgraph.graphql.operation.type"),
                operation_kind.as_apollo_operation_type().into(),
            );
        }
        if let Some(true) = &self.graphql_federation_subgraph_name {
            if let Some(subgraph_name) = &request.subgraph_name {
                attrs.insert(
                    Key::from_static_str("subgraph.name"),
                    subgraph_name.clone().into(),
                );
            }
        }

        attrs
    }

    fn on_response(&self, _response: &subgraph::Response) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }

    fn on_error(&self, _error: &BoxError) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }
}

impl DefaultForLevel for RouterSpans {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel) {
        self.attributes
            .attributes
            .common
            .defaults_for_level(requirement_level);
    }
}

impl DefaultForLevel for SupergraphSpans {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {}
            DefaultAttributeRequirementLevel::Recommended => {
                if self.attributes.attributes.graphql_document.is_none() {
                    self.attributes.attributes.graphql_document = Some(true);
                }
                if self.attributes.attributes.graphql_operation_name.is_none() {
                    self.attributes.attributes.graphql_operation_name = Some(true);
                }
                if self.attributes.attributes.graphql_operation_type.is_none() {
                    self.attributes.attributes.graphql_operation_type = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl DefaultForLevel for SubgraphSpans {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                if self
                    .attributes
                    .attributes
                    .graphql_federation_subgraph_name
                    .is_none()
                {
                    self.attributes.attributes.graphql_federation_subgraph_name = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::Recommended => {
                if self
                    .attributes
                    .attributes
                    .graphql_federation_subgraph_name
                    .is_none()
                {
                    self.attributes.attributes.graphql_federation_subgraph_name = Some(true);
                }
                if self.attributes.attributes.graphql_document.is_none() {
                    self.attributes.attributes.graphql_document = Some(true);
                }
                if self.attributes.attributes.graphql_operation_name.is_none() {
                    self.attributes.attributes.graphql_operation_name = Some(true);
                }
                if self.attributes.attributes.graphql_operation_type.is_none() {
                    self.attributes.attributes.graphql_operation_type = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}
