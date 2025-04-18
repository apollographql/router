use std::fmt::Debug;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_STATUS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_VERSION;
use crate::plugins::telemetry::config_new::events::CustomEvents;
use crate::plugins::telemetry::config_new::events::DisplayRouterRequest;
use crate::plugins::telemetry::config_new::events::DisplayRouterResponse;
use crate::plugins::telemetry::config_new::events::RouterResponseBodyExtensionType;
use crate::plugins::telemetry::config_new::events::StandardEventConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::services::router;

pub(crate) type RouterEvents =
    CustomEvents<router::Request, router::Response, (), RouterAttributes, RouterSelector>;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterEventsConfig {
    /// Log the router request
    pub(crate) request: StandardEventConfig<RouterSelector>,
    /// Log the router response
    pub(crate) response: StandardEventConfig<RouterSelector>,
    /// Log the router error
    pub(crate) error: StandardEventConfig<RouterSelector>,
}

impl CustomEvents<router::Request, router::Response, (), RouterAttributes, RouterSelector> {
    pub(crate) fn on_request(&mut self, request: &router::Request) {
        if let Some(request_event) = &mut self.request {
            if request_event.condition.evaluate_request(request) != Some(true) {
                return;
            }

            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(DisplayRouterRequest(request_event.level)));
        }
        if self.response.is_some() {
            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(DisplayRouterResponse(true)));
        }
        for custom_event in &mut self.custom {
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &router::Response) {
        if let Some(response_event) = &self.response {
            if !response_event.condition.evaluate_response(response) {
                return;
            }
            let mut attrs = Vec::with_capacity(4);

            #[cfg(test)]
            let mut headers: indexmap::IndexMap<String, http::HeaderValue> = response
                .response
                .headers()
                .clone()
                .into_iter()
                .filter_map(|(name, val)| Some((name?.to_string(), val)))
                .collect();
            #[cfg(test)]
            headers.sort_keys();
            #[cfg(not(test))]
            let headers = response.response.headers();
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_HEADERS,
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_STATUS,
                opentelemetry::Value::String(format!("{}", response.response.status()).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_VERSION,
                opentelemetry::Value::String(format!("{:?}", response.response.version()).into()),
            ));

            if let Some(body) = response
                .context
                .extensions()
                .with_lock(|ext| ext.remove::<RouterResponseBodyExtensionType>())
            {
                attrs.push(KeyValue::new(
                    HTTP_RESPONSE_BODY,
                    opentelemetry::Value::String(body.0.into()),
                ));
            }

            log_event(response_event.level, "router.response", attrs, "");
        }
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
                "router.error",
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
