use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use opentelemetry::metrics::Unit;
use opentelemetry_api::metrics::Counter;
use opentelemetry_api::metrics::Histogram;
use opentelemetry_api::metrics::MeterProvider;
use opentelemetry_api::metrics::UpDownCounter;
use opentelemetry_api::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use opentelemetry_semantic_conventions::trace::SERVER_ADDRESS;
use opentelemetry_semantic_conventions::trace::SERVER_PORT;
use opentelemetry_semantic_conventions::trace::URL_SCHEME;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::time::Instant;
use tower::BoxError;

use super::attributes::HttpServerAttributes;
use super::DefaultForLevel;
use super::Selector;
use crate::metrics;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::cost::CostInstruments;
use crate::plugins::telemetry::config_new::cost::CostInstrumentsConfig;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::Context;

pub(crate) const METER_NAME: &str = "apollo/router";

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct InstrumentsConfig {
    /// The attributes and instruments to include by default in instruments based on their level as specified in the otel semantic conventions and Apollo documentation.
    pub(crate) default_requirement_level: DefaultAttributeRequirementLevel,

    /// Router service instruments. For more information see documentation on Router lifecycle.
    pub(crate) router:
        Extendable<RouterInstrumentsConfig, Instrument<RouterAttributes, RouterSelector>>,
    /// Supergraph service instruments. For more information see documentation on Router lifecycle.
    pub(crate) supergraph: Extendable<
        SupergraphInstrumentsConfig,
        Instrument<SupergraphAttributes, SupergraphSelector>,
    >,
    /// Subgraph service instruments. For more information see documentation on Router lifecycle.
    pub(crate) subgraph:
        Extendable<SubgraphInstrumentsConfig, Instrument<SubgraphAttributes, SubgraphSelector>>,
}

impl InstrumentsConfig {
    /// Update the defaults for spans configuration regarding the `default_attribute_requirement_level`
    pub(crate) fn update_defaults(&mut self) {
        self.router
            .attributes
            .defaults_for_levels(self.default_requirement_level, TelemetryDataKind::Metrics);
        self.supergraph
            .defaults_for_levels(self.default_requirement_level, TelemetryDataKind::Metrics);
        self.subgraph
            .defaults_for_levels(self.default_requirement_level, TelemetryDataKind::Metrics);
    }

    pub(crate) fn new_router_instruments(&self) -> RouterInstruments {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let http_server_request_duration = self
            .router
            .attributes
            .http_server_request_duration
            .is_enabled()
            .then(|| CustomHistogram {
                inner: Mutex::new(CustomHistogramInner {
                    increment: Increment::Duration(Instant::now()),
                    condition: Condition::True,
                    histogram: Some(meter.f64_histogram("http.server.request.duration").init()),
                    attributes: Vec::new(),
                    selector: None,
                    selectors: match &self.router.attributes.http_server_request_duration {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            Some(attributes.clone())
                        }
                    },
                    updated: false,
                }),
            });
        let http_server_request_body_size =
            self.router
                .attributes
                .http_server_request_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.router.attributes.http_server_request_body_size {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };
                    CustomHistogram {
                        inner: Mutex::new(CustomHistogramInner {
                            increment: Increment::Custom(None),
                            condition: Condition::True,
                            histogram: Some(
                                meter.f64_histogram("http.server.request.body.size").init(),
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(RouterSelector::RequestHeader {
                                request_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        let http_server_response_body_size =
            self.router
                .attributes
                .http_server_response_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.router.attributes.http_server_response_body_size {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };

                    CustomHistogram {
                        inner: Mutex::new(CustomHistogramInner {
                            increment: Increment::Custom(None),
                            condition: Condition::True,
                            histogram: Some(
                                meter.f64_histogram("http.server.response.body.size").init(),
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(RouterSelector::ResponseHeader {
                                response_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        let http_server_active_requests = self
            .router
            .attributes
            .http_server_active_requests
            .is_enabled()
            .then(|| ActiveRequestsCounter {
                inner: Mutex::new(ActiveRequestsCounterInner {
                    counter: Some(
                        meter
                            .i64_up_down_counter("http.server.active_requests")
                            .init(),
                    ),
                    attrs_config: match &self.router.attributes.http_server_active_requests {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => Default::default(),
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            attributes.clone()
                        }
                    },
                    attributes: Vec::new(),
                }),
            });
        RouterInstruments {
            http_server_request_duration,
            http_server_request_body_size,
            http_server_response_body_size,
            http_server_active_requests,
            custom: CustomInstruments::new(&self.router.custom),
        }
    }

    pub(crate) fn new_supergraph_instruments(&self) -> SupergraphInstruments {
        SupergraphInstruments {
            cost: self.supergraph.attributes.cost.to_instruments(),
            custom: CustomInstruments::new(&self.supergraph.custom),
        }
    }

    pub(crate) fn new_subgraph_instruments(&self) -> SubgraphInstruments {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let http_client_request_duration = self
            .subgraph
            .attributes
            .http_client_request_duration
            .is_enabled()
            .then(|| {
                let mut nb_attributes = 0;
                let selectors = match &self.subgraph.attributes.http_client_request_duration {
                    DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => {
                        None
                    }
                    DefaultedStandardInstrument::Extendable { attributes } => {
                        nb_attributes = attributes.custom.len();
                        Some(attributes.clone())
                    }
                };
                CustomHistogram {
                    inner: Mutex::new(CustomHistogramInner {
                        increment: Increment::Duration(Instant::now()),
                        condition: Condition::True,
                        histogram: Some(meter.f64_histogram("http.client.request.duration").init()),
                        attributes: Vec::with_capacity(nb_attributes),
                        selector: None,
                        selectors,
                        updated: false,
                    }),
                }
            });
        let http_client_request_body_size =
            self.subgraph
                .attributes
                .http_client_request_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.subgraph.attributes.http_client_request_body_size {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };
                    CustomHistogram {
                        inner: Mutex::new(CustomHistogramInner {
                            increment: Increment::Custom(None),
                            condition: Condition::True,
                            histogram: Some(
                                meter.f64_histogram("http.client.request.body.size").init(),
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(SubgraphSelector::SubgraphRequestHeader {
                                subgraph_request_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        let http_client_response_body_size =
            self.subgraph
                .attributes
                .http_client_response_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.subgraph.attributes.http_client_response_body_size {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };
                    CustomHistogram {
                        inner: Mutex::new(CustomHistogramInner {
                            increment: Increment::Custom(None),
                            condition: Condition::True,
                            histogram: Some(
                                meter.f64_histogram("http.client.response.body.size").init(),
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(SubgraphSelector::SubgraphResponseHeader {
                                subgraph_response_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        SubgraphInstruments {
            http_client_request_duration,
            http_client_request_body_size,
            http_client_response_body_size,
            custom: CustomInstruments::new(&self.subgraph.custom),
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterInstrumentsConfig {
    /// Histogram of server request duration
    #[serde(rename = "http.server.request.duration")]
    http_server_request_duration:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Counter of active requests
    #[serde(rename = "http.server.active_requests")]
    http_server_active_requests: DefaultedStandardInstrument<ActiveRequestsAttributes>,

    /// Histogram of server request body size
    #[serde(rename = "http.server.request.body.size")]
    http_server_request_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Histogram of server response body size
    #[serde(rename = "http.server.response.body.size")]
    http_server_response_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,
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
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ActiveRequestsAttributes {
    /// The HTTP request method
    #[serde(rename = "http.request.method")]
    http_request_method: bool,
    /// The server address
    #[serde(rename = "server.address")]
    server_address: bool,
    /// The server port
    #[serde(rename = "server.port")]
    server_port: bool,
    /// The URL scheme
    #[serde(rename = "url.scheme")]
    url_scheme: bool,
}

impl DefaultForLevel for ActiveRequestsAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                self.http_request_method = true;
                self.url_scheme = true;
            }
            DefaultAttributeRequirementLevel::Recommended
            | DefaultAttributeRequirementLevel::None => {}
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum DefaultedStandardInstrument<T> {
    #[default]
    Unset,
    Bool(bool),
    Extendable {
        attributes: Arc<T>,
    },
}

impl<T> DefaultedStandardInstrument<T> {
    pub(crate) fn is_enabled(&self) -> bool {
        match self {
            Self::Unset => false,
            Self::Bool(enabled) => *enabled,
            Self::Extendable { .. } => true,
        }
    }
}

impl<T> DefaultForLevel for DefaultedStandardInstrument<T>
where
    T: DefaultForLevel + Clone + Default,
{
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        match self {
            DefaultedStandardInstrument::Bool(enabled) if *enabled => match requirement_level {
                DefaultAttributeRequirementLevel::None => {}
                DefaultAttributeRequirementLevel::Required
                | DefaultAttributeRequirementLevel::Recommended => {
                    let mut attrs = T::default();
                    attrs.defaults_for_levels(requirement_level, kind);
                    *self = Self::Extendable {
                        attributes: Arc::new(attrs),
                    }
                }
            },
            DefaultedStandardInstrument::Unset => match requirement_level {
                DefaultAttributeRequirementLevel::None => {}
                DefaultAttributeRequirementLevel::Required
                | DefaultAttributeRequirementLevel::Recommended => {
                    let mut attrs = T::default();
                    attrs.defaults_for_levels(requirement_level, kind);
                    *self = Self::Extendable {
                        attributes: Arc::new(attrs),
                    }
                }
            },
            DefaultedStandardInstrument::Extendable { attributes } => {
                Arc::make_mut(attributes).defaults_for_levels(requirement_level, kind);
            }
            _ => {}
        }
    }
}

impl<T, Request, Response, EventResponse> Selectors for DefaultedStandardInstrument<T>
where
    T: Selectors<Request = Request, Response = Response, EventResponse = EventResponse>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) -> Vec<opentelemetry_api::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_request(request),
        }
    }

    fn on_response(&self, response: &Self::Response) -> Vec<opentelemetry_api::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_response(response),
        }
    }

    fn on_error(&self, error: &BoxError) -> Vec<opentelemetry_api::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_error(error),
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) -> Vec<KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_response_event(response, ctx),
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphInstrumentsConfig {
    #[serde(flatten)]
    pub(crate) cost: CostInstrumentsConfig,
}

impl DefaultForLevel for SupergraphInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        _requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphInstrumentsConfig {
    /// Histogram of client request duration
    #[serde(rename = "http.client.request.duration")]
    http_client_request_duration:
        DefaultedStandardInstrument<Extendable<SubgraphAttributes, SubgraphSelector>>,

    /// Histogram of client request body size
    #[serde(rename = "http.client.request.body.size")]
    http_client_request_body_size:
        DefaultedStandardInstrument<Extendable<SubgraphAttributes, SubgraphSelector>>,

    /// Histogram of client response body size
    #[serde(rename = "http.client.response.body.size")]
    http_client_response_body_size:
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

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Instrument<A, E>
where
    A: Default + Debug,
    E: Debug,
{
    /// The type of instrument.
    #[serde(rename = "type")]
    ty: InstrumentType,

    /// The value of the instrument.
    value: InstrumentValue<E>,

    /// The description of the instrument.
    description: String,

    /// The units of the instrument, e.g. "ms", "bytes", "requests".
    unit: String,

    /// Attributes to include on the instrument.
    #[serde(default = "Extendable::empty_arc::<A, E>")]
    attributes: Arc<Extendable<A, E>>,

    /// The instrument conditions.
    #[serde(default = "Condition::empty::<E>")]
    condition: Condition<E>,
}

impl<A, E, Request, Response, EventResponse> Selectors for Instrument<A, E>
where
    A: Debug
        + Default
        + Selectors<Request = Request, Response = Response, EventResponse = EventResponse>,
    E: Debug + Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) -> Vec<opentelemetry_api::KeyValue> {
        self.attributes.on_request(request)
    }

    fn on_response(&self, response: &Self::Response) -> Vec<opentelemetry_api::KeyValue> {
        self.attributes.on_response(response)
    }

    fn on_response_event(
        &self,
        response: &Self::EventResponse,
        ctx: &Context,
    ) -> Vec<opentelemetry_api::KeyValue> {
        self.attributes.on_response_event(response, ctx)
    }

    fn on_error(&self, error: &BoxError) -> Vec<opentelemetry_api::KeyValue> {
        self.attributes.on_error(error)
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum InstrumentType {
    /// A monotonic counter https://opentelemetry.io/docs/specs/otel/metrics/data-model/#sums
    Counter,

    // /// A counter https://opentelemetry.io/docs/specs/otel/metrics/data-model/#sums
    // UpDownCounter,
    /// A histogram https://opentelemetry.io/docs/specs/otel/metrics/data-model/#histogram
    Histogram,
    // /// A gauge https://opentelemetry.io/docs/specs/otel/metrics/data-model/#gauge
    // Gauge,
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum InstrumentValue<T> {
    Standard(Standard),
    Chunked(Event<T>),
    Custom(T),
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Standard {
    Duration,
    Unit,
    // Active,
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Event<T> {
    /// For every supergraph response payload (including subscription events and defer events)
    #[serde(rename = "event_duration")]
    Duration,
    /// For every supergraph response payload (including subscription events and defer events)
    #[serde(rename = "event_unit")]
    Unit,
    /// For every supergraph response payload (including subscription events and defer events)
    #[serde(rename = "event_custom")]
    Custom(T),
}

pub(crate) trait Instrumented {
    type Request;
    type Response;
    type EventResponse;

    fn on_request(&self, request: &Self::Request);
    fn on_response(&self, response: &Self::Response);
    fn on_response_event(&self, _response: &Self::EventResponse, _ctx: &Context) {}
    fn on_error(&self, error: &BoxError, ctx: &Context);
}

impl<A, B, E, Request, Response, EventResponse> Instrumented for Extendable<A, Instrument<B, E>>
where
    A: Default
        + Instrumented<Request = Request, Response = Response, EventResponse = EventResponse>,
    B: Default
        + Debug
        + Selectors<Request = Request, Response = Response, EventResponse = EventResponse>,
    E: Debug + Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        self.attributes.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        self.attributes.on_response(response);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        self.attributes.on_response_event(response, ctx);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        self.attributes.on_error(error, ctx);
    }
}

impl Selectors for SubgraphInstrumentsConfig {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Vec<opentelemetry_api::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_request(request);
        attrs.extend(self.http_client_request_duration.on_request(request));
        attrs.extend(self.http_client_response_body_size.on_request(request));

        attrs
    }

    fn on_response(&self, response: &Self::Response) -> Vec<opentelemetry_api::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_response(response);
        attrs.extend(self.http_client_request_duration.on_response(response));
        attrs.extend(self.http_client_response_body_size.on_response(response));

        attrs
    }

    fn on_error(&self, error: &BoxError) -> Vec<opentelemetry_api::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_error(error);
        attrs.extend(self.http_client_request_duration.on_error(error));
        attrs.extend(self.http_client_response_body_size.on_error(error));

        attrs
    }
}

pub(crate) struct CustomInstruments<Request, Response, Attributes, Select>
where
    Attributes: Selectors<Request = Request, Response = Response> + Default,
    Select: Selector<Request = Request, Response = Response> + Debug,
{
    counters: Vec<CustomCounter<Request, Response, Attributes, Select>>,
    histograms: Vec<CustomHistogram<Request, Response, Attributes, Select>>,
}

impl<Request, Response, Attributes, Select> CustomInstruments<Request, Response, Attributes, Select>
where
    Attributes: Selectors<Request = Request, Response = Response> + Default + Debug + Clone,
    Select: Selector<Request = Request, Response = Response> + Debug + Clone,
{
    pub(crate) fn new(config: &HashMap<String, Instrument<Attributes, Select>>) -> Self {
        let mut counters = Vec::new();
        let mut histograms = Vec::new();
        let meter = metrics::meter_provider().meter(METER_NAME);

        for (instrument_name, instrument) in config {
            match instrument.ty {
                InstrumentType::Counter => {
                    let (selector, increment) = match &instrument.value {
                        InstrumentValue::Standard(incr) => {
                            let incr = match incr {
                                Standard::Duration => Increment::Duration(Instant::now()),
                                Standard::Unit => Increment::Unit,
                            };
                            (None, incr)
                        }
                        InstrumentValue::Custom(selector) => {
                            (Some(Arc::new(selector.clone())), Increment::Custom(None))
                        }
                        InstrumentValue::Chunked(incr) => match incr {
                            Event::Duration => (None, Increment::EventDuration(Instant::now())),
                            Event::Unit => (None, Increment::EventUnit),
                            Event::Custom(selector) => (
                                Some(Arc::new(selector.clone())),
                                Increment::EventCustom(None),
                            ),
                        },
                    };
                    let counter = CustomCounterInner {
                        increment,
                        condition: instrument.condition.clone(),
                        counter: Some(
                            meter
                                .f64_counter(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                        attributes: Vec::new(),
                        selector,
                        selectors: instrument.attributes.clone(),
                        incremented: false,
                    };

                    counters.push(CustomCounter {
                        inner: Mutex::new(counter),
                    })
                }
                InstrumentType::Histogram => {
                    let (selector, increment) = match &instrument.value {
                        InstrumentValue::Standard(incr) => {
                            let incr = match incr {
                                Standard::Duration => Increment::Duration(Instant::now()),
                                Standard::Unit => Increment::Unit,
                            };
                            (None, incr)
                        }
                        InstrumentValue::Custom(selector) => {
                            (Some(Arc::new(selector.clone())), Increment::Custom(None))
                        }
                        InstrumentValue::Chunked(incr) => match incr {
                            Event::Duration => (None, Increment::EventDuration(Instant::now())),
                            Event::Unit => (None, Increment::EventUnit),
                            Event::Custom(selector) => (
                                Some(Arc::new(selector.clone())),
                                Increment::EventCustom(None),
                            ),
                        },
                    };
                    let histogram = CustomHistogramInner {
                        increment,
                        condition: instrument.condition.clone(),
                        histogram: Some(
                            meter
                                .f64_histogram(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                        attributes: Vec::new(),
                        selector,
                        selectors: Some(instrument.attributes.clone()),
                        updated: false,
                    };

                    histograms.push(CustomHistogram {
                        inner: Mutex::new(histogram),
                    })
                }
            }
        }

        Self {
            counters,
            histograms,
        }
    }
}

impl<Request, Response, EventResponse, Attributes, Select> Instrumented
    for CustomInstruments<Request, Response, Attributes, Select>
where
    Attributes:
        Selectors<Request = Request, Response = Response, EventResponse = EventResponse> + Default,
    Select: Selector<Request = Request, Response = Response, EventResponse = EventResponse> + Debug,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        for counter in &self.counters {
            counter.on_request(request);
        }
        for histogram in &self.histograms {
            histogram.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        for counter in &self.counters {
            counter.on_response(response);
        }
        for histogram in &self.histograms {
            histogram.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        for counter in &self.counters {
            counter.on_error(error, ctx);
        }
        for histogram in &self.histograms {
            histogram.on_error(error, ctx);
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        for counter in &self.counters {
            counter.on_response_event(response, ctx);
        }
        for histogram in &self.histograms {
            histogram.on_response_event(response, ctx);
        }
    }
}

pub(crate) struct RouterInstruments {
    http_server_request_duration: Option<
        CustomHistogram<router::Request, router::Response, RouterAttributes, RouterSelector>,
    >,
    http_server_active_requests: Option<ActiveRequestsCounter>,
    http_server_request_body_size: Option<
        CustomHistogram<router::Request, router::Response, RouterAttributes, RouterSelector>,
    >,
    http_server_response_body_size: Option<
        CustomHistogram<router::Request, router::Response, RouterAttributes, RouterSelector>,
    >,
    custom: RouterCustomInstruments,
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
        self.custom.on_error(error, ctx);
    }
}

pub(crate) struct SupergraphInstruments {
    cost: CostInstruments,
    custom: SupergraphCustomInstruments,
}

impl Instrumented for SupergraphInstruments {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &Self::Request) {
        self.cost.on_request(request);
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        self.cost.on_response(response);
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        self.cost.on_error(error, ctx);
        self.custom.on_error(error, ctx);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        self.cost.on_response_event(response, ctx);
        self.custom.on_response_event(response, ctx);
    }
}

pub(crate) struct SubgraphInstruments {
    http_client_request_duration: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    http_client_request_body_size: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    http_client_response_body_size: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    custom: SubgraphCustomInstruments,
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

pub(crate) type RouterCustomInstruments =
    CustomInstruments<router::Request, router::Response, RouterAttributes, RouterSelector>;

pub(crate) type SupergraphCustomInstruments = CustomInstruments<
    supergraph::Request,
    supergraph::Response,
    SupergraphAttributes,
    SupergraphSelector,
>;

pub(crate) type SubgraphCustomInstruments =
    CustomInstruments<subgraph::Request, subgraph::Response, SubgraphAttributes, SubgraphSelector>;

// ---------------- Counter -----------------------
#[derive(Debug)]
pub(crate) enum Increment {
    Unit,
    EventUnit,
    Duration(Instant),
    EventDuration(Instant),
    Custom(Option<i64>),
    EventCustom(Option<i64>),
}

struct CustomCounter<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    inner: Mutex<CustomCounterInner<Request, Response, A, T>>,
}

struct CustomCounterInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    increment: Increment,
    selector: Option<Arc<T>>,
    selectors: Arc<Extendable<A, T>>,
    counter: Option<Counter<f64>>,
    condition: Condition<T>,
    attributes: Vec<opentelemetry_api::KeyValue>,
    // Useful when it's a counter on events to know if we have to count for an event or not
    incremented: bool,
}

impl<A, T, Request, Response, EventResponse> Instrumented for CustomCounter<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response, EventResponse = EventResponse> + Default,
    T: Selector<Request = Request, Response = Response, EventResponse = EventResponse>
        + Debug
        + Debug,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.condition.evaluate_request(request) == Some(false) {
            let _ = inner.counter.take();
            return;
        }
        inner.attributes = inner.selectors.on_request(request).into_iter().collect();
        if let Some(selected_value) = inner.selector.as_ref().and_then(|s| s.on_request(request)) {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => {
                    Increment::EventCustom(selected_value.as_str().parse::<i64>().ok())
                }
                Increment::Custom(None) => {
                    Increment::Custom(selected_value.as_str().parse::<i64>().ok())
                }
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }
    }

    fn on_response(&self, response: &Self::Response) {
        let mut inner = self.inner.lock();
        if !inner.condition.evaluate_response(response) {
            if !matches!(
                &inner.increment,
                Increment::EventCustom(_) | Increment::EventDuration(_) | Increment::EventUnit
            ) {
                let _ = inner.counter.take();
            }
            return;
        }
        let attrs: Vec<KeyValue> = inner.selectors.on_response(response).into_iter().collect();
        inner.attributes.extend(attrs);

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response(response))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => {
                    Increment::Custom(selected_value.as_str().parse::<i64>().ok())
                }
                Increment::Custom(None) => {
                    Increment::Custom(selected_value.as_str().parse::<i64>().ok())
                }
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment = match inner.increment {
            Increment::Unit => 1f64,
            Increment::Duration(instant) => instant.elapsed().as_secs_f64(),
            Increment::Custom(val) => match val {
                Some(incr) => incr as f64,
                None => 0f64,
            },
            Increment::EventUnit | Increment::EventDuration(_) | Increment::EventCustom(_) => {
                // Nothing to do because we're incrementing on events
                return;
            }
        };

        if increment != 0.0 {
            if let Some(counter) = &inner.counter {
                counter.add(increment, &inner.attributes);
            }
            inner.incremented = true;
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        let mut inner = self.inner.lock();
        if !inner.condition.evaluate_event_response(response, ctx) {
            return;
        }
        let attrs: Vec<KeyValue> = inner
            .selectors
            .on_response_event(response, ctx)
            .into_iter()
            .collect();
        inner.attributes.extend(attrs);

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response_event(response, ctx))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => {
                    Increment::EventCustom(selected_value.as_str().parse::<i64>().ok())
                }
                Increment::Custom(None) => {
                    Increment::EventCustom(selected_value.as_str().parse::<i64>().ok())
                }
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment = match &mut inner.increment {
            Increment::EventUnit => 1f64,
            Increment::EventDuration(instant) => {
                let incr = instant.elapsed().as_secs_f64();
                // Set it to new instant for the next event
                *instant = Instant::now();
                incr
            }
            Increment::Custom(val) | Increment::EventCustom(val) => {
                let incr = match val {
                    Some(incr) => *incr as f64,
                    None => 0f64,
                };
                // Set it to None again for the next event
                *val = None;
                incr
            }
            _ => 0f64,
        };

        inner.incremented = true;
        if let Some(counter) = &inner.counter {
            counter.add(increment, &inner.attributes);
        }
    }

    fn on_error(&self, error: &BoxError, _ctx: &Context) {
        let mut inner = self.inner.lock();
        let mut attrs: Vec<KeyValue> = inner.selectors.on_error(error).into_iter().collect();
        attrs.append(&mut inner.attributes);

        let increment = match inner.increment {
            Increment::Unit | Increment::EventUnit => 1f64,
            Increment::Duration(instant) | Increment::EventDuration(instant) => {
                instant.elapsed().as_secs_f64()
            }
            Increment::Custom(val) | Increment::EventCustom(val) => match val {
                Some(incr) => incr as f64,
                None => 0f64,
            },
        };

        if let Some(counter) = inner.counter.take() {
            counter.add(increment, &attrs);
        }
    }
}

impl<A, T, Request, Response> Drop for CustomCounter<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    fn drop(&mut self) {
        // TODO add attribute error broken pipe ? cf https://github.com/apollographql/router/issues/4866
        let inner = self.inner.try_lock();
        if let Some(mut inner) = inner {
            if inner.incremented {
                return;
            }
            if let Some(counter) = inner.counter.take() {
                let incr: f64 = match &inner.increment {
                    Increment::Unit | Increment::EventUnit => 1f64,
                    Increment::Duration(instant) | Increment::EventDuration(instant) => {
                        instant.elapsed().as_secs_f64()
                    }
                    Increment::Custom(val) | Increment::EventCustom(val) => match val {
                        Some(incr) => *incr as f64,
                        None => 0f64,
                    },
                };
                counter.add(incr, &inner.attributes);
            }
        }
    }
}

struct ActiveRequestsCounter {
    inner: Mutex<ActiveRequestsCounterInner>,
}

struct ActiveRequestsCounterInner {
    counter: Option<UpDownCounter<i64>>,
    attrs_config: Arc<ActiveRequestsAttributes>,
    attributes: Vec<opentelemetry_api::KeyValue>,
}

impl Instrumented for ActiveRequestsCounter {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.attrs_config.http_request_method {
            if let Some(attr) = (RouterSelector::RequestMethod {
                request_method: true,
            })
            .on_request(request)
            {
                inner
                    .attributes
                    .push(KeyValue::new(HTTP_REQUEST_METHOD, attr));
            }
        }
        if inner.attrs_config.server_address {
            if let Some(attr) = HttpServerAttributes::forwarded_host(request)
                .and_then(|h| h.host().map(|h| h.to_string()))
            {
                inner.attributes.push(KeyValue::new(SERVER_ADDRESS, attr));
            }
        }
        if inner.attrs_config.server_port {
            if let Some(attr) =
                HttpServerAttributes::forwarded_host(request).and_then(|h| h.port_u16())
            {
                inner
                    .attributes
                    .push(KeyValue::new(SERVER_PORT, attr as i64));
            }
        }
        if inner.attrs_config.url_scheme {
            if let Some(attr) = request.router_request.uri().scheme_str() {
                inner
                    .attributes
                    .push(KeyValue::new(URL_SCHEME, attr.to_string()));
            }
        }
        if let Some(counter) = &inner.counter {
            counter.add(1, &inner.attributes);
        }
    }

    fn on_response(&self, _response: &Self::Response) {
        let mut inner = self.inner.lock();
        if let Some(counter) = &inner.counter.take() {
            counter.add(-1, &inner.attributes);
        }
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) {
        let mut inner = self.inner.lock();
        if let Some(counter) = &inner.counter.take() {
            counter.add(-1, &inner.attributes);
        }
    }
}

impl Drop for ActiveRequestsCounter {
    fn drop(&mut self) {
        let inner = self.inner.try_lock();
        if let Some(mut inner) = inner {
            if let Some(counter) = &inner.counter.take() {
                counter.add(-1, &inner.attributes);
            }
        }
    }
}

// ---------------- Histogram -----------------------

pub(crate) struct CustomHistogram<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response>,
{
    pub(crate) inner: Mutex<CustomHistogramInner<Request, Response, A, T>>,
}

pub(crate) struct CustomHistogramInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response>,
{
    pub(crate) increment: Increment,
    pub(crate) condition: Condition<T>,
    pub(crate) selector: Option<Arc<T>>,
    pub(crate) selectors: Option<Arc<Extendable<A, T>>>,
    pub(crate) histogram: Option<Histogram<f64>>,
    pub(crate) attributes: Vec<opentelemetry_api::KeyValue>,
    // Useful when it's an histogram on events to know if we have to count for an event or not
    pub(crate) updated: bool,
}

impl<A, T, Request, Response, EventResponse> Instrumented
    for CustomHistogram<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response, EventResponse = EventResponse> + Default,
    T: Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.condition.evaluate_request(request) == Some(false) {
            let _ = inner.histogram.take();
            return;
        }
        if let Some(selectors) = &inner.selectors {
            inner.attributes = selectors.on_request(request).into_iter().collect();
        }
        if let Some(selected_value) = inner.selector.as_ref().and_then(|s| s.on_request(request)) {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => {
                    Increment::EventCustom(selected_value.as_str().parse::<i64>().ok())
                }
                Increment::Custom(None) => {
                    Increment::Custom(selected_value.as_str().parse::<i64>().ok())
                }
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }
    }

    fn on_response(&self, response: &Self::Response) {
        let mut inner = self.inner.lock();
        if !inner.condition.evaluate_response(response) {
            if !matches!(
                &inner.increment,
                Increment::EventCustom(_) | Increment::EventDuration(_) | Increment::EventUnit
            ) {
                let _ = inner.histogram.take();
            }
            return;
        }
        let attrs: Vec<KeyValue> = inner
            .selectors
            .as_ref()
            .map(|s| s.on_response(response).into_iter().collect())
            .unwrap_or_default();
        inner.attributes.extend(attrs);
        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response(response))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => {
                    Increment::EventCustom(selected_value.as_str().parse::<i64>().ok())
                }
                Increment::Custom(None) => {
                    Increment::Custom(selected_value.as_str().parse::<i64>().ok())
                }
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment = match inner.increment {
            Increment::Unit => Some(1f64),
            Increment::Duration(instant) => Some(instant.elapsed().as_secs_f64()),
            Increment::Custom(val) => val.map(|incr| incr as f64),
            Increment::EventUnit | Increment::EventDuration(_) | Increment::EventCustom(_) => {
                // Nothing to do because we're incrementing on events
                return;
            }
        };

        if let (Some(histogram), Some(increment)) = (&inner.histogram, increment) {
            histogram.record(increment, &inner.attributes);
            inner.updated = true;
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        let mut inner = self.inner.lock();
        if !inner.condition.evaluate_event_response(response, ctx) {
            return;
        }
        let mut attrs: Vec<KeyValue> = inner
            .selectors
            .as_ref()
            .map(|s| s.on_response_event(response, ctx).into_iter().collect())
            .unwrap_or_default();
        attrs.extend(inner.attributes.clone());
        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response_event(response, ctx))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => {
                    Increment::EventCustom(selected_value.as_str().parse::<i64>().ok())
                }
                Increment::Custom(None) => {
                    Increment::EventCustom(selected_value.as_str().parse::<i64>().ok())
                }
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment: Option<f64> = match &mut inner.increment {
            Increment::EventUnit => Some(1f64),
            Increment::EventDuration(instant) => {
                let incr = Some(instant.elapsed().as_secs_f64());
                // Need a new instant for the next event
                *instant = Instant::now();
                incr
            }
            Increment::EventCustom(val) => {
                let incr = val.map(|incr| incr as f64);
                // Set it to None again
                *val = None;
                incr
            }
            Increment::Unit | Increment::Duration(_) | Increment::Custom(_) => {
                // Nothing to do because we're incrementing on events
                return;
            }
        };
        if let (Some(histogram), Some(increment)) = (&inner.histogram, increment) {
            histogram.record(increment, &attrs);
            inner.updated = true;
        }
    }

    fn on_error(&self, error: &BoxError, _ctx: &Context) {
        let mut inner = self.inner.lock();
        let mut attrs: Vec<KeyValue> = inner
            .selectors
            .as_ref()
            .map(|s| s.on_error(error).into_iter().collect())
            .unwrap_or_default();
        attrs.append(&mut inner.attributes);

        let increment = match inner.increment {
            Increment::Unit | Increment::EventUnit => Some(1f64),
            Increment::Duration(instant) | Increment::EventDuration(instant) => {
                Some(instant.elapsed().as_secs_f64())
            }
            Increment::Custom(val) | Increment::EventCustom(val) => val.map(|incr| incr as f64),
        };

        if let (Some(histogram), Some(increment)) = (inner.histogram.take(), increment) {
            histogram.record(increment, &attrs);
        }
    }
}

impl<A, T, Request, Response> Drop for CustomHistogram<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response>,
{
    fn drop(&mut self) {
        // TODO add attribute error broken pipe ? cf https://github.com/apollographql/router/issues/4866
        let inner = self.inner.try_lock();
        if let Some(mut inner) = inner {
            if inner.updated {
                return;
            }
            if let Some(histogram) = inner.histogram.take() {
                let increment = match &inner.increment {
                    Increment::Unit | Increment::EventUnit => Some(1f64),
                    Increment::Duration(instant) | Increment::EventDuration(instant) => {
                        Some(instant.elapsed().as_secs_f64())
                    }
                    Increment::Custom(val) | Increment::EventCustom(val) => {
                        val.map(|incr| incr as f64)
                    }
                };

                if let Some(increment) = increment {
                    histogram.record(increment, &inner.attributes);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::context::OPERATION_KIND;
    use crate::graphql;
    use crate::metrics::FutureMetricsExt;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;

    #[tokio::test]
    async fn test_router_instruments() {
        async {
            let config: InstrumentsConfig = serde_json::from_str(
                json!({
                    "router": {
                        "http.server.request.body.size": true,
                        "http.server.response.body.size": {
                            "attributes": {
                                "http.response.status_code": false,
                                "acme.my_attribute": {
                                    "response_header": "x-my-header",
                                    "default": "unknown"
                                }
                            }
                        },
                        "acme.request.on_error": {
                            "value": "unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "not": {
                                    "eq": [
                                        200,
                                        {
                                            "response_status": "code"
                                        }
                                    ]
                                }
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.on_critical_error": {
                            "value": "unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "request time out",
                                    {
                                        "error": "reason"
                                    }
                                ]
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.on_error_histo": {
                            "value": "unit",
                            "type": "histogram",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "not": {
                                    "eq": [
                                        200,
                                        {
                                            "response_status": "code"
                                        }
                                    ]
                                }
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.header_value": {
                            "value": {
                                "request_header": "x-my-header-count"
                            },
                            "type": "counter",
                            "description": "my description",
                            "unit": "nb"
                        }
                    }
                })
                .to_string()
                .as_str(),
            )
            .unwrap();

            let router_instruments = config.new_router_instruments();
            let router_req = RouterRequest::fake_builder()
                .header("conditional-custom", "X")
                .header("x-my-header-count", "55")
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("x-my-header", "TEST")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 55.0);
            assert_counter!(
                "acme.request.on_error",
                1.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                1.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!("http.server.request.body.size", 35.0);
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "TEST"
            );

            let router_instruments = config.new_router_instruments();
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("x-my-header-count", "5")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 60.0);
            assert_counter!(
                "acme.request.on_error",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!("http.server.request.body.size", 70.0);
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "TEST"
            );
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "unknown"
            );

            let router_instruments = config.new_router_instruments();
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::OK)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 60.0);
            assert_counter!(
                "acme.request.on_error",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                2.0,
                "http.response.status_code" = 400
            );

            let router_instruments = config.new_router_instruments();
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            router_instruments.on_error(&BoxError::from("request time out"), &Context::new());
            assert_counter!(
                "acme.request.on_critical_error",
                1.0,
                "http.response.status_code" = 500
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_supergraph_instruments() {
        async {
            let config: InstrumentsConfig = serde_json::from_str(
                json!({
                    "supergraph": {
                        "acme.request.on_error": {
                            "value": "unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "not": {
                                    "eq": [
                                        200,
                                        {
                                            "response_status": "code"
                                        }
                                    ]
                                }
                            }
                        },
                        "acme.request.on_graphql_error": {
                            "value": "event_unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "NOPE",
                                    {
                                        "response_errors": "$.[0].extensions.code"
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_error_selector": {
                            "value": "event_unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    true,
                                    {
                                        "on_graphql_error": true
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_error_histo": {
                            "value": "event_unit",
                            "type": "histogram",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "NOPE",
                                    {
                                        "response_errors": "$.[0].extensions.code"
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_data": {
                            "value": {
                                "response_data": "$.price"
                            },
                            "type": "counter",
                            "unit": "$",
                            "description": "my description",
                            "attributes": {
                                "response.data": {
                                    "response_data": "$.*"
                                }
                            }
                        },
                        "acme.query": {
                            "value": "unit",
                            "type": "counter",
                            "description": "nb of queries",
                            "condition": {
                                "eq": [
                                    "query",
                                    {
                                        "operation_kind": "string"
                                    }
                                ]
                            },
                            "unit": "query",
                            "attributes": {
                                "query": {
                                    "query": "string"
                                }
                            }
                        }
                    }
                })
                .to_string()
                .as_str(),
            )
            .unwrap();

            let custom_instruments = SupergraphCustomInstruments::new(&config.supergraph.custom);
            let context = crate::context::Context::new();
            let _ = context.insert(OPERATION_KIND, "query".to_string()).unwrap();
            let context_with_error = crate::context::Context::new();
            let _ = context_with_error
                .insert(OPERATION_KIND, "query".to_string())
                .unwrap();
            let _ = context_with_error
                .insert(CONTAINS_GRAPHQL_ERROR, true)
                .unwrap();
            let supergraph_req = supergraph::Request::fake_builder()
                .header("conditional-custom", "X")
                .header("x-my-header-count", "55")
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .query("{me{name}}")
                .context(context.clone())
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("x-my-header", "TEST")
                .header("content-length", "35")
                .errors(vec![graphql::Error::builder()
                    .message("nope")
                    .extension_code("NOPE")
                    .build()])
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(json!({
                        "price": 500
                    }))
                    .errors(vec![graphql::Error::builder()
                        .message("nope")
                        .extension_code("NOPE")
                        .build()])
                    .build(),
                &context_with_error,
            );

            assert_counter!("acme.query", 1.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 1.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 500.0, response.data = 500);

            let custom_instruments = SupergraphCustomInstruments::new(&config.supergraph.custom);
            let supergraph_req = supergraph::Request::fake_builder()
                .header("content-length", "35")
                .header("x-my-header-count", "5")
                .header("content-type", "application/graphql")
                .context(context.clone())
                .query("Subscription {me{name}}")
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .errors(vec![graphql::Error::builder()
                    .message("nope")
                    .extension_code("NOPE")
                    .build()])
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(json!({
                        "price": 500
                    }))
                    .errors(vec![graphql::Error::builder()
                        .message("nope")
                        .extension_code("NOPE")
                        .build()])
                    .build(),
                &context_with_error,
            );

            assert_counter!("acme.query", 1.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 2.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 1000.0, response.data = 500);

            let custom_instruments = SupergraphCustomInstruments::new(&config.supergraph.custom);
            let supergraph_req = supergraph::Request::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .context(context.clone())
                .query("{me{name}}")
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::OK)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(serde_json_bytes::json!({"foo": "bar"}))
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(serde_json_bytes::json!({"foo": "bar"}))
                    .build(),
                &supergraph_req.context,
            );

            assert_counter!("acme.query", 2.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 2.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 1000.0, response.data = 500);
        }
        .with_metrics()
        .await;
    }
}
