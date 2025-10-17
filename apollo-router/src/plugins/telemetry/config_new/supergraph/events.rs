use std::fmt::Debug;
use std::sync::Arc;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::selectors::SupergraphSelector;
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
use crate::plugins::telemetry::config_new::supergraph::attributes::SupergraphAttributes;
use crate::services::supergraph;

pub(crate) type SupergraphEvents = CustomEvents<
    supergraph::Request,
    supergraph::Response,
    graphql::Response,
    SupergraphAttributes,
    SupergraphSelector,
>;

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
        if let Some(request_event) = &mut self.request
            && request_event.condition.evaluate_request(request) == Some(true)
        {
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
                opentelemetry::Value::String(format!("{headers:?}").into()),
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
        if let Some(mut response_event) = self.response.take()
            && response_event.condition.evaluate_request(request) != Some(false)
        {
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
        if let Some(error_event) = &self.error
            && error_event.condition.evaluate_error(error, ctx)
        {
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
#[derive(Clone)]
pub(crate) struct SupergraphEventResponse {
    // XXX(@IvanGoncharov): As part of removing Arc from StandardEvent I moved it here
    // I think it's not nessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Condition<SupergraphSelector>>,
}

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

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::context::OPERATION_NAME;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_events() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

        async {
            test_harness
                .supergraph_service(|_r| async {
                    supergraph::Response::fake_builder()
                        .header("custom-header", "val1")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .build()
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .build()
                        .unwrap(),
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_events_with_exists_condition() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!(
                "../../testdata/custom_events_exists_condition.router.yaml"
            ))
            .build()
            .await
            .expect("test harness");

        async {
            let ctx = Context::new();
            ctx.insert(OPERATION_NAME, String::from("Test")).unwrap();
            test_harness
                .supergraph_service(|_r| async {
                    supergraph::Response::fake_builder()
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .build()
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query Test { foo }")
                        .context(ctx)
                        .build()
                        .unwrap(),
                )
                .await
                .expect("expecting successful response");
            test_harness
                .supergraph_service(|_r| async {
                    Ok(supergraph::Response::fake_builder()
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .build()
                        .expect("expecting valid response"))
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_events_on_graphql_error() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

        async {
            test_harness
                .supergraph_service(|_r| async {
                    let context_with_error = Context::new();
                    let _ = context_with_error
                        .insert(CONTAINS_GRAPHQL_ERROR, true)
                        .unwrap();
                    supergraph::Response::fake_builder()
                        .header("custom-header", "val1")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .context(context_with_error)
                        .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                        .build()
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_events_on_response() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

        async {
            test_harness
                .supergraph_service(|_r| async {
                    supergraph::Response::fake_builder()
                        .header("custom-header", "val1")
                        .header("x-log-response", HeaderValue::from_static("log"))
                        .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                        .build()
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }
}
