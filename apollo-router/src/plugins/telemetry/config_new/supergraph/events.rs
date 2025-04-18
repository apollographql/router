use std::sync::Arc;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute::HTTP_REQUEST_METHOD;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::graphql;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_URI;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_VERSION;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::events::CustomEvents;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::config_new::events::StandardEventConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::supergraph::attributes::SupergraphAttributes;
use crate::services::supergraph;

pub(crate) type SupergraphEvents = CustomEvents<
    supergraph::Request,
    supergraph::Response,
    graphql::Response,
    SupergraphAttributes,
    SupergraphSelector,
>;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphEventsConfig {
    /// Log the supergraph request
    pub(crate) request: StandardEventConfig<SupergraphSelector>,
    /// Log the supergraph response
    pub(crate) response: StandardEventConfig<SupergraphSelector>,
    /// Log the supergraph error
    pub(crate) error: StandardEventConfig<SupergraphSelector>,
}

#[derive(Clone)]
pub(crate) struct SupergraphEventResponse {
    // XXX(@IvanGoncharov): As part of removing Arc from StandardEvent I moved it here
    // I think it's not necessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Condition<SupergraphSelector>>,
}

impl
    CustomEvents<
        supergraph::Request,
        supergraph::Response,
        graphql::Response,
        SupergraphAttributes,
        SupergraphSelector,
    >
{
    pub(crate) fn on_request(&mut self, request: &supergraph::Request) {
        if let Some(request_event) = &mut self.request {
            if request_event.condition.evaluate_request(request) != Some(true) {
                return;
            }
            let mut attrs = Vec::with_capacity(5);
            #[cfg(test)]
            let mut headers: indexmap::IndexMap<String, http::HeaderValue> = request
                .supergraph_request
                .headers()
                .clone()
                .into_iter()
                .filter_map(|(name, val)| Some((name?.to_string(), val)))
                .collect();
            #[cfg(test)]
            headers.sort_keys();
            #[cfg(not(test))]
            let headers = request.supergraph_request.headers();
            attrs.push(KeyValue::new(
                HTTP_REQUEST_HEADERS,
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_METHOD,
                opentelemetry::Value::String(
                    format!("{}", request.supergraph_request.method()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_URI,
                opentelemetry::Value::String(
                    format!("{}", request.supergraph_request.uri()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_VERSION,
                opentelemetry::Value::String(
                    format!("{:?}", request.supergraph_request.version()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_BODY,
                opentelemetry::Value::String(
                    serde_json::to_string(request.supergraph_request.body())
                        .unwrap_or_default()
                        .into(),
                ),
            ));
            log_event(request_event.level, "supergraph.request", attrs, "");
        }
        if let Some(response_event) = self.response.take() {
            request.context.extensions().with_lock(|lock| {
                lock.insert(SupergraphEventResponse {
                    level: response_event.level,
                    condition: Arc::new(response_event.condition),
                })
            });
        }
        for custom_event in &mut self.custom {
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &supergraph::Response) {
        for custom_event in &mut self.custom {
            custom_event.on_response(response);
        }
    }

    pub(crate) fn on_response_event(&self, response: &graphql::Response, ctx: &Context) {
        for custom_event in &self.custom {
            custom_event.on_response_event(response, ctx);
        }
    }

    pub(crate) fn on_error(&mut self, error: &BoxError, ctx: &Context) {
        if let Some(error_event) = &self.error {
            if !error_event.condition.evaluate_error(error, ctx) {
                return;
            }
            log_event(
                error_event.level,
                "supergraph.error",
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
