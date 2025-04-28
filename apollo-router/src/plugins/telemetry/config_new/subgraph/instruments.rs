use std::fmt::Debug;

use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::selectors::SubgraphSelector;
use super::selectors::SubgraphValue;
use crate::Context;
use crate::plugins::telemetry::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomInstruments;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::subgraph;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphInstrumentsConfig {
    /// Histogram of client request duration
    #[serde(rename = "http.client.request.duration")]
    pub(crate) http_client_request_duration:
        DefaultedStandardInstrument<Extendable<SubgraphAttributes, SubgraphSelector>>,

    /// Histogram of client request body size
    #[serde(rename = "http.client.request.body.size")]
    pub(crate) http_client_request_body_size:
        DefaultedStandardInstrument<Extendable<SubgraphAttributes, SubgraphSelector>>,

    /// Histogram of client response body size
    #[serde(rename = "http.client.response.body.size")]
    pub(crate) http_client_response_body_size:
        DefaultedStandardInstrument<Extendable<SubgraphAttributes, SubgraphSelector>>,
}

impl DefaultForLevel for SubgraphInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.http_client_request_duration
            .defaults_for_level(requirement_level, kind);
        self.http_client_request_body_size
            .defaults_for_level(requirement_level, kind);
        self.http_client_response_body_size
            .defaults_for_level(requirement_level, kind);
    }
}

impl Selectors<subgraph::Request, subgraph::Response, ()> for SubgraphInstrumentsConfig {
    fn on_request(&self, request: &subgraph::Request) -> Vec<opentelemetry::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_request(request);
        attrs.extend(self.http_client_request_duration.on_request(request));
        attrs.extend(self.http_client_response_body_size.on_request(request));

        attrs
    }

    fn on_response(&self, response: &subgraph::Response) -> Vec<opentelemetry::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_response(response);
        attrs.extend(self.http_client_request_duration.on_response(response));
        attrs.extend(self.http_client_response_body_size.on_response(response));

        attrs
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Vec<opentelemetry::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_error(error, ctx);
        attrs.extend(self.http_client_request_duration.on_error(error, ctx));
        attrs.extend(self.http_client_response_body_size.on_error(error, ctx));

        attrs
    }
}

pub(crate) struct SubgraphInstruments {
    pub(crate) http_client_request_duration: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            (),
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    pub(crate) http_client_request_body_size: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            (),
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    pub(crate) http_client_response_body_size: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            (),
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    pub(crate) custom: SubgraphCustomInstruments,
}

impl Instrumented for SubgraphInstruments {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(http_client_request_duration) = &self.http_client_request_duration {
            http_client_request_duration.on_request(request);
        }
        if let Some(http_client_request_body_size) = &self.http_client_request_body_size {
            http_client_request_body_size.on_request(request);
        }
        if let Some(http_client_response_body_size) = &self.http_client_response_body_size {
            http_client_response_body_size.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(http_client_request_duration) = &self.http_client_request_duration {
            http_client_request_duration.on_response(response);
        }
        if let Some(http_client_request_body_size) = &self.http_client_request_body_size {
            http_client_request_body_size.on_response(response);
        }
        if let Some(http_client_response_body_size) = &self.http_client_response_body_size {
            http_client_response_body_size.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(http_client_request_duration) = &self.http_client_request_duration {
            http_client_request_duration.on_error(error, ctx);
        }
        if let Some(http_client_request_body_size) = &self.http_client_request_body_size {
            http_client_request_body_size.on_error(error, ctx);
        }
        if let Some(http_client_response_body_size) = &self.http_client_response_body_size {
            http_client_response_body_size.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }
}

pub(crate) type SubgraphCustomInstruments = CustomInstruments<
    subgraph::Request,
    subgraph::Response,
    (),
    SubgraphAttributes,
    SubgraphSelector,
    SubgraphValue,
>;
