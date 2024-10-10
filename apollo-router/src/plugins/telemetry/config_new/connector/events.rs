use opentelemetry_api::Key;
use opentelemetry_api::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_URI;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_VERSION;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_STATUS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_VERSION;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::connector::ConnectorRequest;
use crate::plugins::telemetry::config_new::connector::ConnectorResponse;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::events::CustomEvent;
use crate::plugins::telemetry::config_new::events::CustomEventInner;
use crate::plugins::telemetry::config_new::events::CustomEvents;
use crate::plugins::telemetry::config_new::events::Event;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::config_new::events::StandardEvent;
use crate::plugins::telemetry::config_new::events::StandardEventConfig;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::Context;

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

#[derive(Clone)]
pub(crate) struct ConnectorEventRequest(pub(crate) StandardEvent<ConnectorSelector>);

#[derive(Clone)]
pub(crate) struct ConnectorEventResponse(pub(crate) StandardEvent<ConnectorSelector>);

pub(crate) type ConnectorEvents =
    CustomEvents<ConnectorRequest, ConnectorResponse, ConnectorAttributes, ConnectorSelector>;

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
    for CustomEvents<ConnectorRequest, ConnectorResponse, ConnectorAttributes, ConnectorSelector>
{
    type Request = ConnectorRequest;
    type Response = ConnectorResponse;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if self.request.level() != EventLevel::Off {
            if let Some(condition) = self.request.condition() {
                if condition.lock().evaluate_request(request) != Some(true) {
                    return;
                }
            }
            let mut attrs = Vec::with_capacity(5);
            #[cfg(test)]
            let headers = {
                let mut headers: indexmap::IndexMap<String, http::HeaderValue> = request
                    .http_request
                    .headers()
                    .clone()
                    .into_iter()
                    .filter_map(|(name, val)| Some((name?.to_string(), val)))
                    .collect();
                headers.sort_keys();
                headers
            };
            #[cfg(not(test))]
            let headers = request.http_request.headers();

            attrs.push(KeyValue::new(
                HTTP_REQUEST_HEADERS,
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_METHOD,
                opentelemetry::Value::String(format!("{}", request.http_request.method()).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_URI,
                opentelemetry::Value::String(format!("{}", request.http_request.uri()).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_VERSION,
                opentelemetry::Value::String(
                    format!("{:?}", request.http_request.version()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_BODY,
                opentelemetry::Value::String(format!("{:?}", request.http_request.body()).into()),
            ));
            log_event(self.request.level(), "connector.request", attrs, "");
        }
        for custom_event in &self.custom {
            custom_event.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        if self.response.level() != EventLevel::Off {
            if let Some(condition) = self.response.condition() {
                if !condition.lock().evaluate_response(response) {
                    return;
                }
            }
            let mut attrs = Vec::with_capacity(4);
            #[cfg(test)]
            let headers = {
                let mut headers: indexmap::IndexMap<String, http::HeaderValue> = response
                    .http_response
                    .headers()
                    .clone()
                    .into_iter()
                    .filter_map(|(name, val)| Some((name?.to_string(), val)))
                    .collect();
                headers.sort_keys();
                headers
            };
            #[cfg(not(test))]
            let headers = response.http_response.headers();

            attrs.push(KeyValue::new(
                HTTP_RESPONSE_HEADERS,
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_STATUS,
                opentelemetry::Value::String(format!("{}", response.http_response.status()).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_VERSION,
                opentelemetry::Value::String(
                    format!("{:?}", response.http_response.version()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_BODY,
                opentelemetry::Value::String(format!("{:?}", response.http_response.body()).into()),
            ));
            log_event(self.response.level(), "connector.response", attrs, "");
        }
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
