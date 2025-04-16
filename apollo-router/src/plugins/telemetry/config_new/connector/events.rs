use opentelemetry::Key;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use tower::BoxError;

use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::Context;
use crate::plugins::telemetry::config_new::connector::ConnectorRequest;
use crate::plugins::telemetry::config_new::connector::ConnectorResponse;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::events::CustomEvent;
use crate::plugins::telemetry::config_new::events::CustomEvents;
use crate::plugins::telemetry::config_new::events::Event;
use crate::plugins::telemetry::config_new::events::StandardEvent;
use crate::plugins::telemetry::config_new::events::StandardEventConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::extendable::Extendable;

#[derive(Clone)]
pub(crate) struct ConnectorEventRequest {
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Mutex<Condition<ConnectorSelector>>>,
}

#[derive(Clone)]
pub(crate) struct ConnectorEventResponse {
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Mutex<Condition<ConnectorSelector>>>,
}

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
        .filter_map(|(name, config)| CustomEvent::from_config(name, config))
        .collect();

    ConnectorEvents {
        request: StandardEvent::from_config(&config.attributes.request),
        response: StandardEvent::from_config(&config.attributes.response),
        error: StandardEvent::from_config(&config.attributes.error),
        custom: custom_events,
    }
}

impl CustomEvents<ConnectorRequest, ConnectorResponse, (), ConnectorAttributes, ConnectorSelector> {
    pub(crate) fn on_request(&mut self, request: &ConnectorRequest) {
        // Any condition on the request is NOT evaluated here. It must be evaluated later when
        // getting the ConnectorEventRequest from the context. The request context is shared
        // between all connector requests, so any request could find this ConnectorEventRequest in
        // the context. Its presence on the context cannot be conditional on an individual request.
        if let Some(request_event) = self.request.take() {
            request.context.extensions().with_lock(|lock| {
                lock.insert(ConnectorEventRequest {
                    level: request_event.level,
                    condition: Arc::new(Mutex::new(request_event.condition)),
                })
            });
        }

        if let Some(response_event) = self.response.take() {
            request.context.extensions().with_lock(|lock| {
                lock.insert(ConnectorEventResponse{
                    level: response_event.level,
                    condition: Arc::new(Mutex::new(response_event.condition)),
                })
            });
        }

        for custom_event in &mut self.custom {
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &ConnectorResponse) {
        for custom_event in &mut self.custom {
            custom_event.on_response(response);
        }
    }

    pub(crate) fn on_error(&mut self, error: &BoxError, ctx: &Context) {
        if let Some(error_event) = &mut self.error {
            if error_event.condition.evaluate_error(error, ctx) {
                log_event(
                    error_event.level,
                    "connector.http.error",
                    vec![KeyValue::new(
                        Key::from_static_str("error"),
                        opentelemetry::Value::String(error.to_string().into()),
                    )],
                    "",
                );
            }
        }
        for custom_event in &mut self.custom {
            custom_event.on_error(error, ctx);
        }
    }
}
