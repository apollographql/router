use std::sync::Arc;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::events::CustomEvents;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::config_new::events::StandardEventConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::services::subgraph;

pub(crate) type SubgraphEvents =
    CustomEvents<subgraph::Request, subgraph::Response, (), SubgraphAttributes, SubgraphSelector>;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphEventsConfig {
    /// Log the subgraph request
    pub(crate) request: StandardEventConfig<SubgraphSelector>,
    /// Log the subgraph response
    pub(crate) response: StandardEventConfig<SubgraphSelector>,
    /// Log the subgraph error
    pub(crate) error: StandardEventConfig<SubgraphSelector>,
}

#[derive(Clone)]
pub(crate) struct SubgraphEventRequest {
    // XXX(@IvanGoncharov): As part of removing Mutex from StandardEvent I moved it here
    // I think it's not necessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Mutex<Condition<SubgraphSelector>>>,
}

#[derive(Clone)]
pub(crate) struct SubgraphEventResponse {
    // XXX(@IvanGoncharov): As part of removing Arc from StandardEvent I moved it here
    // I think it's not necessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Condition<SubgraphSelector>>,
}

impl CustomEvents<subgraph::Request, subgraph::Response, (), SubgraphAttributes, SubgraphSelector> {
    pub(crate) fn on_request(&mut self, request: &subgraph::Request) {
        if let Some(mut request_event) = self.request.take() {
            if request_event.condition.evaluate_request(request) != Some(true) {
                return;
            }
            request.context.extensions().with_lock(|lock| {
                lock.insert(SubgraphEventRequest {
                    level: request_event.level,
                    condition: Arc::new(Mutex::new(request_event.condition)),
                })
            });
        }
        if let Some(response_event) = self.response.take() {
            request.context.extensions().with_lock(|lock| {
                lock.insert(SubgraphEventResponse {
                    level: response_event.level,
                    condition: Arc::new(response_event.condition),
                })
            });
        }
        for custom_event in &mut self.custom {
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &subgraph::Response) {
        for custom_event in &mut self.custom {
            custom_event.on_response(response);
        }
    }

    pub(crate) fn on_error(&mut self, error: &BoxError, ctx: &Context) {
        if let Some(error_event) = &self.error {
            if !error_event.condition.evaluate_error(error, ctx) {
                return;
            }
            log_event(
                error_event.level,
                "subgraph.error",
                vec![KeyValue::new(
                    Key::from_static_str("error"),
                    opentelemetry::Value::String(error.to_string().into()),
                )],
                "",
            );
        }
        for custom_event in &mut self.custom {
            custom_event.on_error(error, ctx);
        }
    }
}
