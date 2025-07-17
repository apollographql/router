use std::fmt;
use std::fmt::Debug;
use std::fmt::Display;
use std::hash::Hash;
use std::time::Duration;

use ahash::HashMap;
use apollo_router::graphql;
use apollo_router::json_ext::Path;
use displaydoc::Display;
use opentelemetry::Value;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use opentelemetry_proto::tonic::metrics::v1::metric;
use opentelemetry_proto::tonic::metrics::v1::number_data_point;
use opentelemetry_proto::tonic::metrics::v1::NumberDataPoint;
use serde_json::json;
use wiremock::ResponseTemplate;

use crate::integration::common::graph_os_enabled;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn test_validation_error_emits_metric() {
    if !graph_os_enabled() {
        return;
    }
    let expected_service = "";
    let expected_error_code = "GRAPHQL_VALIDATION_FAILED";
    let expected_operation_name = "# GraphQLValidationFailure";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/apollo_otel_metrics.router.yaml"))
        .responder(ResponseTemplate::new(500).append_header("Content-Type", "application/json"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(Query::default().with_invalid_query())
        .await;

    let response = response.text().await.unwrap();
    assert!(response.contains(expected_error_code));

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2), 1000)
        .await;
    assert!(!metrics.is_empty());
    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.error".to_string())
            .attribute("graphql.operation.name", expected_operation_name)
            .attribute("graphql.error.extensions.code", expected_error_code)
            .attribute("apollo.router.error.service", expected_service)
            .value(1)
            .build(),
    );
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_http_error_emits_metric() {
    if !graph_os_enabled() {
        return;
    }
    let expected_service = "products";
    let expected_error_code = "SUBREQUEST_HTTP_ERROR";
    let expected_client_name = "CLIENT_NAME";
    let expected_client_version = "v0.14";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/apollo_otel_metrics_include_subgraph_errors_enabled.router.yaml"))
        .responder(ResponseTemplate::new(500))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                .build(),
        )
        .await;

    let response = response.text().await.unwrap();
    assert!(response.contains(expected_error_code));

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2), 1000)
        .await;

    assert!(!metrics.is_empty());
    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.error".to_string())
            .attribute("graphql.operation.name", "ExampleQuery")
            .attribute("graphql.operation.type", "query")
            .attribute("apollo.client.name", expected_client_name)
            .attribute("apollo.client.version", expected_client_version)
            .attribute("graphql.error.extensions.code", expected_error_code)
            .attribute("apollo.router.error.service", expected_service)
            // One for each subgraph
            .value(2)
            .build(),
    );
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_layer_error_emits_metric() {
    if !graph_os_enabled() {
        return;
    }
    let expected_service = "products";
    let expected_error_code = "SUBGRAPH_CODE";
    let expected_client_name = "CLIENT_NAME";
    let expected_client_version = "v0.14";
    let expected_path = "/topProducts/name";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/apollo_otel_metrics.router.yaml"))
        .responder(
            ResponseTemplate::new(200).set_body_json(
                graphql::Response::builder()
                    .data(json!({"data": null}))
                    .errors(vec![
                        graphql::Error::builder()
                            .message("error in subgraph layer")
                            .extension_code(expected_error_code)
                            .extension("service", expected_service)
                            // Path must not have leading slash to match expected
                            .path(Path::from("topProducts/name"))
                            .build(),
                    ])
                    .build(),
            ),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router
        .execute_query(
            Query::builder()
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                .build(),
        )
        .await;

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2), 1000)
        .await;

    assert!(!metrics.is_empty());
    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.error".to_string())
            .attribute("graphql.operation.name", "ExampleQuery")
            .attribute("graphql.operation.type", "query")
            .attribute("apollo.client.name", expected_client_name)
            .attribute("apollo.client.version", expected_client_version)
            .attribute("graphql.error.extensions.code", expected_error_code)
            .attribute("apollo.router.error.service", expected_service)
            .attribute("graphql.error.path", expected_path)
            .value(1)
            .build(),
    );
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_include_subgraph_error_disabled_does_not_redact_error_metrics() {
    if !graph_os_enabled() {
        return;
    }

    let expected_service = "products";
    let expected_error_code = "SUBGRAPH_CODE";
    let expected_client_name = "CLIENT_NAME";
    let expected_client_version = "v0.14";
    let expected_path = "/topProducts/name";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/apollo_otel_metrics_include_subgraph_errors_disabled.router.yaml"))
        .responder(
            ResponseTemplate::new(200).set_body_json(
                graphql::Response::builder()
                    .data(json!({"data": null}))
                    .errors(vec![
                        graphql::Error::builder()
                            .message("error in subgraph layer")
                            .extension_code(expected_error_code)
                            .extension("service", expected_service)
                            // Path must not have leading slash to match expected
                            .path(Path::from("topProducts/name"))
                            .build(),
                    ])
                    .build(),
            ),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router
        .execute_query(
            Query::builder()
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                .build(),
        )
        .await;

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2), 1000)
        .await;

    assert!(!metrics.is_empty());
    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.error".to_string())
            .attribute("graphql.operation.name", "ExampleQuery")
            .attribute("graphql.operation.type", "query")
            .attribute("apollo.client.name", expected_client_name)
            .attribute("apollo.client.version", expected_client_version)
            .attribute("graphql.error.extensions.code", expected_error_code)
            .attribute("apollo.router.error.service", expected_service)
            .attribute("graphql.error.path", expected_path)
            .value(1)
            .build(),
    );
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_layer_error_emits_metric() {
    if !graph_os_enabled() {
        return;
    }

    // Empty service indicates a router error
    let expected_service = "";
    let expected_error_code = "INTROSPECTION_DISABLED";
    let expected_client_name = "CLIENT_NAME";
    let expected_client_version = "v0.14";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/apollo_otel_metrics_introspection_disabled.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router
        .execute_query(
            Query::builder()
                .body(json!({"query": "{ __schema { queryType { name } } }", "variables":{}}))
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                .build(),
        )
        .await;

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2), 1000)
        .await;

    assert!(!metrics.is_empty());
    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.error".to_string())
            .attribute("graphql.operation.type", "query")
            .attribute("apollo.client.name", expected_client_name)
            .attribute("apollo.client.version", expected_client_version)
            .attribute("graphql.error.extensions.code", expected_error_code)
            .attribute("apollo.router.error.service", expected_service)
            .value(1)
            .build(),
    );
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_execution_layer_error_emits_metric() {
    if !graph_os_enabled() {
        return;
    }

    // Empty service indicates a router error
    let expected_service = "";
    let expected_error_code = "MUTATION_FORBIDDEN";
    let expected_client_name = "CLIENT_NAME";
    let expected_client_version = "v0.14";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/apollo_otel_metrics_forbid_mutations.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_query(
        Query::builder()
            .body(json!({
                "query": "mutation MyMutation($upc: ID!, $name: String!) { createProduct(upc: $upc, name: $name) { name } }",
                "variables":{"upc": 123, "name": "myProduct"}
            })
            )
            .header("apollographql-client-name", expected_client_name)
            .header("apollographql-client-version", expected_client_version)
            .build()
    ).await;

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2), 1000)
        .await;

    assert!(!metrics.is_empty());
    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.error".to_string())
            .attribute("graphql.operation.name", "MyMutation")
            .attribute("graphql.operation.type", "mutation")
            .attribute("apollo.client.name", expected_client_name)
            .attribute("apollo.client.version", expected_client_version)
            .attribute("graphql.error.extensions.code", expected_error_code)
            .attribute("apollo.router.error.service", expected_service)
            .value(1)
            .build(),
    );
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_layer_error_emits_metric() {
    if !graph_os_enabled() {
        return;
    }

    // Empty service indicates a router error
    let expected_service = "";
    let expected_error_code = "CSRF_ERROR";
    let expected_client_name = "CLIENT_NAME";
    let expected_client_version = "v0.14";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/apollo_otel_metrics_csrf_required_headers.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router
        .execute_query(
            Query::builder()
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                // Content type cannot be application/json to trigger the error
                .content_type("")
                .build(),
        )
        .await;

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2), 1000)
        .await;

    assert!(!metrics.is_empty());

    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.error".to_string())
            .attribute("apollo.client.name", expected_client_name)
            .attribute("apollo.client.version", expected_client_version)
            .attribute("graphql.error.extensions.code", expected_error_code)
            .attribute("apollo.router.error.service", expected_service)
            .value(1)
            .build(),
    );
    router.graceful_shutdown().await;
}

/// Assert that the given metric exists in the list of Otel requests. This is a crude attempt at
/// replicating _some_ assert_counter!() functionality since that test util can't be accessed here.
fn assert_metrics_contain(actual_metrics: &[ExportMetricsServiceRequest], expected_metric: Metric) {
    let expected_name = &expected_metric.name.clone();
    let actual_metric = find_metric(expected_name, actual_metrics)
        .unwrap_or_else(|| panic!("Metric '{}' not found", expected_name));
    let sum = match &actual_metric.data {
        Some(metric::Data::Sum(sum)) => sum,
        _ => panic!("Metric '{}' is not a sum", expected_name),
    };

    let actual_metrics: Vec<Metric> = sum
        .data_points
        .iter()
        .map(|dp| Metric::from_datapoint(expected_name, dp))
        .collect();

    let metric_found = actual_metrics.iter().any(|m| {
        m.value == expected_metric.value && m.attributes_contain(&expected_metric.attributes)
    });

    assert!(
        metric_found,
        "Expected metric '{}' but no matching datapoint was found.\nInstead, actual metrics with matching name were:\n{}",
        expected_metric,
        actual_metrics
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn find_metric<'a>(
    name: &str,
    metrics: &'a [ExportMetricsServiceRequest],
) -> Option<&'a opentelemetry_proto::tonic::metrics::v1::Metric> {
    metrics
        .iter()
        .flat_map(|req| &req.resource_metrics)
        .flat_map(|rm| &rm.scope_metrics)
        .flat_map(|sm| &sm.metrics)
        .find(|m| m.name == name)
}

#[derive(Display, Clone, Debug)]
struct Metric {
    pub name: String,
    pub attributes: HashMap<String, AnyValue>,
    pub value: i64,
}

#[buildstructor::buildstructor]
impl Metric {
    #[builder]
    fn new<V>(name: String, attributes: HashMap<String, V>, value: i64) -> Self
    where
        V: Into<Value>,
    {
        Metric {
            name: name.into(),
            attributes: attributes
                .into_iter()
                .map(|(k, v)| (k, v.into().into()))
                .collect(),
            value,
        }
    }
    fn from_datapoint(name: &str, datapoint: &NumberDataPoint) -> Self {
        Metric {
            name: name.to_string(),
            attributes: datapoint
                .attributes
                .iter()
                .map(|kv| (kv.key.clone(), kv.value.clone().unwrap()))
                .collect::<HashMap<String, AnyValue>>(),
            value: match datapoint.value {
                Some(number_data_point::Value::AsInt(value)) => value,
                _ => panic!("expected integer datapoint"),
            },
        }
    }
    fn attributes_contain(&self, other_attributes: &HashMap<String, AnyValue>) -> bool {
        other_attributes
            .iter()
            .all(|(other_key, other_value)| self.attributes.get(other_key) == Some(other_value))
    }
}

impl Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "name: {}, value: {}, attributes: [",
            self.name, self.value
        )?;
        for (i, (key, any)) in self.attributes.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            let value = any
                .value
                .clone()
                .map(|value| match value {
                    StringValue(sv) => sv.clone(),
                    BoolValue(b) => b.to_string(),
                    IntValue(n) => n.to_string(),
                    DoubleValue(d) => d.to_string(),
                    other => format!("{:?}", other),
                })
                .unwrap_or_else(|| "nil".into());
            write!(f, "{}={}", key, value)?;
        }
        write!(f, "]")
    }
}
