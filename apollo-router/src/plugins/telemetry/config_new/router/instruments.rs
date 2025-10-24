use std::fmt::Debug;

use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::selectors::RouterSelector;
use super::selectors::RouterValue;
use crate::Context;
use crate::plugins::telemetry::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::ActiveRequestsAttributes;
use crate::plugins::telemetry::config_new::instruments::ActiveRequestsCounter;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomInstruments;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::router_overhead::RouterOverheadAttributes;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterInstrumentsConfig {
    /// Histogram of server request duration
    #[serde(rename = "http.server.request.duration")]
    pub(crate) http_server_request_duration:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Counter of active requests
    #[serde(rename = "http.server.active_requests")]
    pub(crate) http_server_active_requests: DefaultedStandardInstrument<ActiveRequestsAttributes>,

    /// Histogram of server request body size
    #[serde(rename = "http.server.request.body.size")]
    pub(crate) http_server_request_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Histogram of server response body size
    #[serde(rename = "http.server.response.body.size")]
    pub(crate) http_server_response_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Histogram of router overhead (time not spent in subgraph requests)
    #[serde(rename = "apollo.router.overhead")]
    pub(crate) router_overhead:
        DefaultedStandardInstrument<Extendable<RouterOverheadAttributes, RouterSelector>>,
}

impl DefaultForLevel for RouterInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.http_server_request_duration
            .defaults_for_levels(requirement_level, kind);
        self.http_server_active_requests
            .defaults_for_levels(requirement_level, kind);
        self.http_server_request_body_size
            .defaults_for_levels(requirement_level, kind);
        self.http_server_response_body_size
            .defaults_for_levels(requirement_level, kind);
        self.router_overhead
            .defaults_for_levels(requirement_level, kind);
    }
}

pub(crate) struct RouterInstruments {
    pub(crate) http_server_request_duration: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) http_server_active_requests: Option<ActiveRequestsCounter>,
    pub(crate) http_server_request_body_size: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) http_server_response_body_size: Option<
        CustomHistogram<router::Request, router::Response, (), RouterAttributes, RouterSelector>,
    >,
    pub(crate) router_overhead: Option<
        CustomHistogram<
            router::Request,
            router::Response,
            (),
            RouterOverheadAttributes,
            RouterSelector,
        >,
    >,
    pub(crate) custom: RouterCustomInstruments,
}

impl Instrumented for RouterInstruments {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_request(request);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_request(request);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_request(request);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_request(request);
        }
        if let Some(router_overhead) = &self.router_overhead {
            router_overhead.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_response(response);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_response(response);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_response(response);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_response(response);
        }
        if let Some(router_overhead) = &self.router_overhead {
            router_overhead.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_error(error, ctx);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_error(error, ctx);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_error(error, ctx);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_error(error, ctx);
        }
        if let Some(router_overhead) = &self.router_overhead {
            router_overhead.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }
}

pub(crate) type RouterCustomInstruments = CustomInstruments<
    router::Request,
    router::Response,
    (),
    RouterAttributes,
    RouterSelector,
    RouterValue,
>;
