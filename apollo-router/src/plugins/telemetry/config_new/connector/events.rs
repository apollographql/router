use opentelemetry::Key;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::connector::ConnectorRequest;
use crate::plugins::telemetry::config_new::connector::ConnectorResponse;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::events::CustomEvent;
use crate::plugins::telemetry::config_new::events::CustomEventInner;
use crate::plugins::telemetry::config_new::events::CustomEvents;
use crate::plugins::telemetry::config_new::events::Event;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::config_new::events::StandardEvent;
use crate::plugins::telemetry::config_new::events::StandardEventConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::Instrumented;

#[derive(Clone)]
pub(crate) struct ConnectorEventRequest(pub(crate) StandardEvent<ConnectorSelector>);
#[derive(Clone)]
pub(crate) struct ConnectorEventResponse(pub(crate) StandardEvent<ConnectorSelector>);

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConnectorEventsConfig {
    /// Log the connector HTTP request
    pub(crate) request: StandardEventConfig<ConnectorSelector>,
    /// Log the connector HTTP response
    pub(crate) response: StandardEventConfig<ConnectorSelector>,
    /// Log the connector HTTP error
    pub(crate) error: StandardEventConfig<ConnectorSelector>,
}

pub(crate) type ConnectorEvents =
    CustomEvents<ConnectorRequest, ConnectorResponse, (), ConnectorAttributes, ConnectorSelector>;

pub(crate) fn new_connector_events(
    config: &Extendable<ConnectorEventsConfig, Event<ConnectorAttributes, ConnectorSelector>>,
) -> ConnectorEvents {
    let custom_events = config
        .custom
        .iter()
        .filter_map(|(event_name, event_cfg)| match &event_cfg.level {
            EventLevel::Off => None,
            _ => Some(CustomEvent {
                inner: Mutex::new(CustomEventInner {
                    name: event_name.clone(),
                    level: event_cfg.level,
                    event_on: event_cfg.on,
                    message: event_cfg.message.clone(),
                    selectors: event_cfg.attributes.clone().into(),
                    condition: event_cfg.condition.clone(),
                    attributes: Vec::new(),
                    _phantom: Default::default(),
                }),
            }),
        })
        .collect();

    ConnectorEvents {
        request: config.attributes.request.clone().into(),
        response: config.attributes.response.clone().into(),
        error: config.attributes.error.clone().into(),
        custom: custom_events,
    }
}

impl Instrumented
    for CustomEvents<
        ConnectorRequest,
        ConnectorResponse,
        (),
        ConnectorAttributes,
        ConnectorSelector,
    >
{
    type Request = ConnectorRequest;
    type Response = ConnectorResponse;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        // Any condition on the request is NOT evaluated here. It must be evaluated later when
        // getting the ConnectorEventRequest from the context. The request context is shared
        // between all connector requests, so any request could find this ConnectorEventRequest in
        // the context. Its presence on the context cannot be conditional on an individual request.
        if self.request.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|lock| lock.insert(ConnectorEventRequest(self.request.clone())));
        }

        if self.response.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|lock| lock.insert(ConnectorEventResponse(self.response.clone())));
        }

        for custom_event in &self.custom {
            custom_event.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        for custom_event in &self.custom {
            custom_event.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if self.error.level() != EventLevel::Off {
            if let Some(condition) = self.error.condition() {
                if !condition.lock().evaluate_error(error, ctx) {
                    return;
                }
            }
            log_event(
                self.error.level(),
                "connector.http.error",
                vec![KeyValue::new(
                    Key::from_static_str("error"),
                    opentelemetry::Value::String(error.to_string().into()),
                )],
                "",
            );
        }
        for custom_event in &self.custom {
            custom_event.on_error(error, ctx);
        }
    }
}
