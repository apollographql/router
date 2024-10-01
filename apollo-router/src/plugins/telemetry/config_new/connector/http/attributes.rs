//! Attributes for HTTP connectors.

use opentelemetry_api::Key;
use opentelemetry_api::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::config_new::attributes::SUBGRAPH_NAME;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::connector_service::ConnectorInfo;
use crate::services::connector_service::CONNECTOR_INFO_CONTEXT_KEY;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::Context;

const CONNECTOR_HTTP_METHOD: Key = Key::from_static_str("connector.http.method");
const CONNECTOR_SOURCE_NAME: Key = Key::from_static_str("connector.source.name");
const CONNECTOR_URL_TEMPLATE: Key = Key::from_static_str("connector.url.template");

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConnectorHttpAttributes {
    /// The name of the subgraph containing the connector
    /// Examples:
    ///
    /// * posts
    ///
    /// Requirement level: Required
    #[serde(rename = "subgraph.name")]
    subgraph_name: Option<StandardAttribute>,

    /// The name of the source for this connector, if defined
    /// Examples:
    ///
    /// * posts_api
    ///
    /// Requirement level: Conditionally Required: If the connector has a source defined
    #[serde(rename = "connector.source.name")]
    connector_source_name: Option<StandardAttribute>,

    /// The HTTP method for the connector
    /// Examples:
    ///
    /// * GET
    /// * POST
    ///
    /// Requirement level: Required
    #[serde(rename = "connector.http.method")]
    connector_http_method: Option<StandardAttribute>,

    /// The connector URL template, relative to the source base URL if one is defined
    /// Examples:
    ///
    /// * /users/{$this.id!}/post
    ///
    /// Requirement level: Required
    #[serde(rename = "connector.url.template")]
    connector_url_template: Option<StandardAttribute>,
}

impl DefaultForLevel for ConnectorHttpAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                if self.subgraph_name.is_none() {
                    self.subgraph_name = Some(StandardAttribute::Bool(true));
                }
            }
            DefaultAttributeRequirementLevel::Recommended => {
                if self.subgraph_name.is_none() {
                    self.subgraph_name = Some(StandardAttribute::Bool(true));
                }
                if self.connector_source_name.is_none() {
                    self.connector_source_name = Some(StandardAttribute::Bool(true));
                }
                if self.connector_http_method.is_none() {
                    self.connector_http_method = Some(StandardAttribute::Bool(true));
                }
                if self.connector_url_template.is_none() {
                    self.connector_url_template = Some(StandardAttribute::Bool(true));
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl Selectors for ConnectorHttpAttributes {
    type Request = HttpRequest;
    type Response = HttpResponse;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();

        if let Ok(Some(connector_info)) = request
            .context
            .get::<&str, ConnectorInfo>(CONNECTOR_INFO_CONTEXT_KEY)
        {
            if let Some(key) = self
                .subgraph_name
                .as_ref()
                .and_then(|a| a.key(SUBGRAPH_NAME))
            {
                attrs.push(KeyValue::new(key, connector_info.subgraph_name.to_string()));
            }
            if let Some(key) = self
                .connector_source_name
                .as_ref()
                .and_then(|a| a.key(CONNECTOR_SOURCE_NAME))
            {
                if let Some(source_name) = connector_info.source_name {
                    attrs.push(KeyValue::new(key, source_name.to_string()));
                }
            }
            if let Some(key) = self
                .connector_http_method
                .as_ref()
                .and_then(|a| a.key(CONNECTOR_HTTP_METHOD))
            {
                attrs.push(KeyValue::new(key, connector_info.http_method));
            }
            if let Some(key) = self
                .connector_url_template
                .as_ref()
                .and_then(|a| a.key(CONNECTOR_URL_TEMPLATE))
            {
                attrs.push(KeyValue::new(key, connector_info.url_template.to_string()));
            }
        }
        attrs
    }

    fn on_response(&self, _response: &Self::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::default()
    }
}
