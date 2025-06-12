use std::path::PathBuf;
use std::time::Duration;

use std::sync::{Arc, Mutex};
use tracing::{Subscriber, subscriber::set_default};
use tracing_subscriber::{Registry, layer::SubscriberExt};

use crate::integration::IntegrationTest;
use crate::integration::common::graph_os_enabled;

mod max_evaluated_plans;

const PROMETHEUS_METRICS_CONFIG: &str = include_str!("telemetry/fixtures/prometheus.router.yaml");

// Helper for capturing outcome field from tracing
#[derive(Clone, Default)]
struct OutcomeLayer {
    outcome: Arc<Mutex<Option<String>>>,
}

impl<S: Subscriber> tracing_subscriber::Layer<S> for OutcomeLayer {
    fn on_record(
        &self,
        _span: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Visitor<'a> {
            outcome: &'a Arc<Mutex<Option<String>>>,
        }
        impl<'a> tracing_core::field::Visit for Visitor<'a> {
            fn record_str(&mut self, field: &tracing_core::Field, value: &str) {
                if field.name() == "outcome" {
                    *self.outcome.lock().unwrap() = Some(value.to_string());
                }
            }
            fn record_debug(&mut self, field: &tracing_core::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "outcome" {
                    *self.outcome.lock().unwrap() = Some(format!("{:?}", value));
                }
            }
        }
        let mut visitor = Visitor {
            outcome: &self.outcome,
        };
        values.record(&mut visitor);
    }
}

fn setup_outcome_tracing() -> (OutcomeLayer, tracing::subscriber::DefaultGuard) {
    let layer = OutcomeLayer::default();
    let subscriber = Registry::default().with(layer.clone());
    let guard = set_default(subscriber);
    (layer, guard)
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_new_qp() {
    let mut router = IntegrationTest::builder()
        .config("{}") // Default config
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router
        .wait_for_log_message(
            "could not create router: \
             failed to initialize the query planner: \
             Supergraphs composed with federation version 1 are not supported.",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed2_schema_with_new_qp() {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_METRICS_CONFIG)
        .supergraph("../examples/graphql/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .assert_metrics_contains(
            r#"apollo_router_lifecycle_query_planner_init_total{init_is_success="true",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn context_with_new_qp() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config("{}") // Default config
        .supergraph("tests/fixtures/set_context/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_schema_with_new_qp_fails_startup() {
    let mut router = IntegrationTest::builder()
        .config("{}") // Default config
        .supergraph("tests/fixtures/broken-supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router
        .wait_for_log_message(
            "could not create router: \
             Federation error: Invalid supergraph: must be a core schema",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_schema_with_new_qp_change_to_broken_schema_keeps_old_config() {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_METRICS_CONFIG)
        .supergraph("tests/fixtures/valid-supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .assert_metrics_contains(
            r#"apollo_router_lifecycle_query_planner_init_total{init_is_success="true",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.execute_default_query().await;
    router
        .update_schema(&PathBuf::from("tests/fixtures/broken-supergraph.graphql"))
        .await;
    router
        .wait_for_log_message("error while reloading, continuing with previous configuration")
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cooperative_cancellation_enforce_mode() {
    // Inline enforce mode config with Prometheus metrics
    let enforce_config = r#"
        supergraph:
          query_planning:
            experimental_cooperative_cancellation:
              enforce: enabled
        telemetry:
          exporters:
            metrics:
              prometheus:
                enabled: true
                listen: 127.0.0.1:0
                path: /metrics
    "#;
    let mut router = IntegrationTest::builder()
        .config(enforce_config)
        .supergraph("tests/fixtures/valid-supergraph.graphql")
        .responder(
            wiremock::ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(2))
                .set_body_json(serde_json::json!({"data": {"topProducts": [{"name": "Table"}]}})),
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let query = crate::integration::common::Query::builder()
        .body(serde_json::json!({"query": "query { topProducts { name } }"}))
        .build();
    let fut = router.execute_query(query);
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(fut);
    tokio::time::sleep(Duration::from_millis(300)).await;
    router.assert_metrics_contains(
        r#"apollo_router_query_planning_plan_duration_seconds{planner="rust",outcome="cancelled"}"#,
        Some(Duration::from_secs(5)),
    ).await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cooperative_cancellation_measure_mode() {
    // Inline measure mode config with Prometheus metrics
    let measure_config = r#"
        supergraph:
          query_planning:
            experimental_cooperative_cancellation:
              measure: enabled
        telemetry:
          exporters:
            metrics:
              prometheus:
                enabled: true
                listen: 127.0.0.1:0
                path: /metrics
    "#;
    let mut router = IntegrationTest::builder()
        .config(measure_config)
        .supergraph("tests/fixtures/valid-supergraph.graphql")
        .responder(
            wiremock::ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(2))
                .set_body_json(serde_json::json!({"data": {"topProducts": [{"name": "Table"}]}})),
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let query = crate::integration::common::Query::builder()
        .body(serde_json::json!({"query": "query { topProducts { name } }"}))
        .build();
    let fut = router.execute_query(query);
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(fut);
    tokio::time::sleep(Duration::from_millis(300)).await;
    router.assert_metrics_contains(
        r#"apollo_router_query_planning_plan_duration_seconds{planner="rust",outcome="cancelled"}"#,
        Some(Duration::from_secs(5)),
    ).await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cooperative_cancellation_enforce_mode_with_timeout() {
    // Inline enforce mode + timeout config with Prometheus metrics
    let enforce_timeout_config = r#"
        supergraph:
          query_planning:
            experimental_cooperative_cancellation:
              enforce:
                enabled_with_timeout_in_seconds: 0.2
        telemetry:
          exporters:
            metrics:
              prometheus:
                enabled: true
                listen: 127.0.0.1:0
                path: /metrics
    "#;
    let mut router = IntegrationTest::builder()
        .config(enforce_timeout_config)
        .supergraph("tests/fixtures/valid-supergraph.graphql")
        .responder(
            wiremock::ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(2))
                .set_body_json(serde_json::json!({"data": {"topProducts": [{"name": "Table"}]}})),
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let query = crate::integration::common::Query::builder()
        .body(serde_json::json!({"query": "query { topProducts { name } }"}))
        .build();
    let _ = router.execute_query(query).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    router.assert_metrics_contains(
        r#"apollo_router_query_planning_plan_duration_seconds{planner="rust",outcome="timeout"}"#,
        Some(Duration::from_secs(5)),
    ).await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cooperative_cancellation_measure_mode_with_timeout() {
    // Inline measure mode + timeout config with Prometheus metrics
    let measure_timeout_config = r#"
        supergraph:
          query_planning:
            experimental_cooperative_cancellation:
              measure:
                enabled_with_timeout_in_seconds: 0.2
        telemetry:
          exporters:
            metrics:
              prometheus:
                enabled: true
                listen: 127.0.0.1:0
                path: /metrics
    "#;
    let mut router = IntegrationTest::builder()
        .config(measure_timeout_config)
        .supergraph("tests/fixtures/valid-supergraph.graphql")
        .responder(
            wiremock::ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(2))
                .set_body_json(serde_json::json!({"data": {"topProducts": [{"name": "Table"}]}})),
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let query = crate::integration::common::Query::builder()
        .body(serde_json::json!({"query": "query { topProducts { name } }"}))
        .build();
    let _ = router.execute_query(query).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    router.assert_metrics_contains(
        r#"apollo_router_query_planning_plan_duration_seconds{planner="rust",outcome="timeout"}"#,
        Some(Duration::from_secs(5)),
    ).await;
    router.graceful_shutdown().await;
}
