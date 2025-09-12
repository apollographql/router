use std::fmt::Debug;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::selectors::RouterSelector;
use crate::Context;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_STATUS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_VERSION;
use crate::plugins::telemetry::config_new::events::CustomEvents;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::config_new::events::StandardEventConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::services::router;

#[derive(Clone)]
pub(crate) struct DisplayRouterRequest(pub(crate) EventLevel);
#[derive(Default, Clone, Debug)]
pub(crate) struct DisplayRouterResponse;
#[derive(Default, Clone, Debug)]
pub(crate) struct RouterResponseBodyExtensionType(pub(crate) String);

pub(crate) type RouterEvents =
    CustomEvents<router::Request, router::Response, (), RouterAttributes, RouterSelector>;

impl CustomEvents<router::Request, router::Response, (), RouterAttributes, RouterSelector> {
    pub(crate) fn on_request(&mut self, request: &router::Request) {
        if let Some(request_event) = &mut self.request
            && request_event.condition.evaluate_request(request) == Some(true)
        {
            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(DisplayRouterRequest(request_event.level)));
        }
        if let Some(response_event) = &mut self.response
            && response_event.condition.evaluate_request(request) != Some(false)
        {
            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(DisplayRouterResponse));
        }
        for custom_event in &mut self.custom {
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &router::Response) {
        if let Some(response_event) = &self.response
            && response_event.condition.evaluate_response(response)
        {
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
                opentelemetry::Value::String(format!("{headers:?}").into()),
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
                // Clone here in case anything else also needs access to the body
                .with_lock(|ext| ext.get::<RouterResponseBodyExtensionType>().cloned())
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
        if let Some(error_event) = &self.error
            && error_event.condition.evaluate_error(error, ctx)
        {
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

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use http::header::CONTENT_LENGTH;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_events() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

        async {
            test_harness
                .router_service(|_r| async {
                    Ok(router::Response::fake_builder()
                        .header("custom-header", "val1")
                        .header(CONTENT_LENGTH, "25")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .data(serde_json_bytes::json!({"data": "res"}))
                        .build()
                        .expect("expecting valid response"))
                })
                .call(
                    router::Request::fake_builder()
                        .header(CONTENT_LENGTH, "0")
                        .header("custom-header", "val1")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .build()
                        .unwrap(),
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!({
            r#"[].span["apollo_private.duration_ns"]"# => "[duration]",
            r#"[].spans[]["apollo_private.duration_ns"]"# => "[duration]",
            "[].fields.attributes" => insta::sorted_redaction()
        }))
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_events_graphql_error() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

        async {
            // Without the header to enable custom event
            test_harness
                .router_service(

                    |_r| async {
                        let context_with_error = Context::new();
                        let _ = context_with_error
                            .insert(CONTAINS_GRAPHQL_ERROR, true)
                            .unwrap();
                        Ok(router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .context(context_with_error)
                            .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                            .build()
                            .expect("expecting valid response"))
                    },
                )
                .call(router::Request::fake_builder()
                    .header("custom-header", "val1")
                    .build()
                    .unwrap())
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(
            assert_snapshot_subscriber!({r#"[].span["apollo_private.duration_ns"]"# => "[duration]", r#"[].spans[]["apollo_private.duration_ns"]"# => "[duration]", "[].fields.attributes" => insta::sorted_redaction()}),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_events_graphql_response() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

        async {
            // Without the header to enable custom event
            test_harness
                .router_service(
                    |_r| async {
                        Ok(router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header(CONTENT_LENGTH, "25")
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .data(serde_json_bytes::json!({"data": "res"}))
                            .build()
                            .expect("expecting valid response"))
                    },
                )
                .call(router::Request::fake_builder()
                    .header("custom-header", "val1")
                    .build()
                    .unwrap())
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(
            assert_snapshot_subscriber!({r#"[].span["apollo_private.duration_ns"]"# => "[duration]", r#"[].spans[]["apollo_private.duration_ns"]"# => "[duration]", "[].fields.attributes" => insta::sorted_redaction()}),
        )
        .await
    }
}
