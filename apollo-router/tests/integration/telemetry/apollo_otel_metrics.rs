use std::fmt;
use std::fmt::Debug;
use std::fmt::Display;
use std::path::PathBuf;
use std::time::Duration;

use ahash::HashMap;
use apollo_router::graphql;
use displaydoc::Display;
use opentelemetry::Value;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue;
use opentelemetry_proto::tonic::metrics::v1::HistogramDataPoint;
use opentelemetry_proto::tonic::metrics::v1::NumberDataPoint;
use opentelemetry_proto::tonic::metrics::v1::metric;
use opentelemetry_proto::tonic::metrics::v1::number_data_point;
use serde_json::json;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;

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
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                errors:
                  preview_extended_error_metrics: enabled
        "#,
        )
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
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                errors:
                  preview_extended_error_metrics: enabled
            include_subgraph_errors:
              all: true
        "#,
        )
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
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                errors:
                  preview_extended_error_metrics: enabled
        "#,
        )
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
                            .path("topProducts/name")
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
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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
async fn test_subgraph_layer_entities_error_emits_metric() {
    if !graph_os_enabled() {
        return;
    }
    let expected_service = "products";
    let expected_error_code = "SUBGRAPH_CODE";
    let expected_client_name = "CLIENT_NAME";
    let expected_client_version = "v0.14";
    let expected_path = "/_entities/0/name";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                errors:
                  preview_extended_error_metrics: enabled
        "#,
        )
        .responder(
            ResponseTemplate::new(200).set_body_json(
                graphql::Response::builder()
                    .data(json!({"data": {"_entities": [{"name": null}]}}))
                    .errors(vec![
                        graphql::Error::builder()
                            .message("error in subgraph layer")
                            // Explicitly exclude setting service as it should get populated by subgraph_service
                            .extension_code(expected_error_code)
                            // Path must not have leading slash to match expected
                            .path("_entities/0/name")
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
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                errors:
                  preview_extended_error_metrics: enabled
            include_subgraph_errors:
              all: false
        "#,
        )
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
                            .path("topProducts/name")
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
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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
        .config(
            r#"
          telemetry:
            apollo:
              experimental_otlp_metrics_protocol: http
              metrics:
                otlp:
                  batch_processor:
                    scheduled_delay: 100ms
              errors:
                preview_extended_error_metrics: enabled
          supergraph:
            introspection: false
        "#,
        )
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
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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
        .config(
            r#"
          telemetry:
            apollo:
              experimental_otlp_metrics_protocol: http
              metrics:
                otlp:
                  batch_processor:
                    scheduled_delay: 100ms
              errors:
                preview_extended_error_metrics: enabled
          forbid_mutations: true
        "#,
        )
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
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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
        .config(
            r#"
          telemetry:
            apollo:
              experimental_otlp_metrics_protocol: http
              metrics:
                otlp:
                  batch_processor:
                    scheduled_delay: 100ms
              errors:
                preview_extended_error_metrics: enabled
          csrf:
            required_headers:
              - x-not-matched-header
        "#,
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
                // Content type cannot be application/json to trigger the error
                .content_type("")
                .build(),
        )
        .await;

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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

// This test verifies that Apollo Studio metrics (using Apollo/ApolloRealtime meter providers)
// are NOT affected by rename views.
//
// Architecture:
// - Public MeterProvider -> Prometheus & OTLP (views APPLIED)
// - Apollo MeterProvider -> Apollo Studio backend (views NOT APPLIED)
// - ApolloRealtime MeterProvider -> Apollo Realtime backend (views NOT APPLIED)
#[tokio::test(flavor = "multi_thread")]
async fn test_apollo_studio_metrics_not_affected_by_rename() {
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
        .config(
            r#"
          telemetry:
            apollo:
              experimental_otlp_metrics_protocol: http
              metrics:
                otlp:
                  batch_processor:
                    scheduled_delay: 100ms
              errors:
                preview_extended_error_metrics: enabled
            exporters:
              metrics:
                common:
                  service_name: router
                  views:
                    - name: apollo.router.operations.error
                      rename: renamed.apollo.router.operations.error
          csrf:
            required_headers:
              - x-not-matched-header
        "#,
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
                // Content type cannot be application/json to trigger the error
                .content_type("")
                .build(),
        )
        .await;
    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
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

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_request_emits_histogram() {
    if !graph_os_enabled() {
        return;
    }
    let expected_operation_name = "ExampleQuery";
    let expected_client_name = "myClient";
    let expected_client_version = "v0.14";
    let expected_service = "products";
    let expected_operation_type = "query";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                subgraph_metrics: true
            include_subgraph_errors:
              all: true
        "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, _response) = router
        .execute_query(
            Query::builder()
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                .build(),
        )
        .await;

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
        .await;
    assert!(!metrics.is_empty());
    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.fetch.duration".to_string())
            .attribute("graphql.operation.name", expected_operation_name)
            .attribute("apollo.client.name", expected_client_name)
            .attribute("apollo.client.version", expected_client_version)
            .attribute("subgraph.name", expected_service)
            .attribute("graphql.operation.type", expected_operation_type)
            .attribute("has_errors", false)
            .count(1)
            .build(),
    );
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_failed_subgraph_request_emits_histogram() {
    if !graph_os_enabled() {
        return;
    }
    let expected_operation_name = "ExampleQuery";
    let expected_client_name = "myClient";
    let expected_client_version = "v0.14";
    let expected_service = "products";
    let expected_operation_type = "query";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                subgraph_metrics: true
            include_subgraph_errors:
              all: true
        "#,
        )
        .responder(ResponseTemplate::new(500).append_header("Content-Type", "application/json"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, _response) = router
        .execute_query(
            Query::builder()
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                .build(),
        )
        .await;

    let metrics = router
        .wait_for_emitted_otel_metrics(Duration::from_secs(2))
        .await;
    assert!(!metrics.is_empty());
    assert_metrics_contain(
        &metrics,
        Metric::builder()
            .name("apollo.router.operations.fetch.duration".to_string())
            .attribute("graphql.operation.name", expected_operation_name)
            .attribute("apollo.client.name", expected_client_name)
            .attribute("apollo.client.version", expected_client_version)
            .attribute("subgraph.name", expected_service)
            .attribute("graphql.operation.type", expected_operation_type)
            .attribute("has_errors", true)
            .count(1)
            .build(),
    );
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connector_request_emits_histogram() {
    if !graph_os_enabled() {
        return;
    }
    let expected_operation_name = "ExampleQuery";
    let expected_client_name = "myClient";
    let expected_client_version = "v0.14";
    let expected_service = "connectors";
    let expected_operation_type = "query";
    let expected_connector_source = "jsonPlaceholder";

    // The `connectors.sources` stanza is required for the `IntegrationTest` harness to
    // auto-inject `override_url` pointing at the local wiremock subgraph (see
    // `merge_overrides` in `tests/common.rs`: the override is only inserted if the
    // config already declares `connectors.sources`). Without it the connector falls
    // through to the schema's `https://jsonplaceholder.typicode.com/` baseURL and makes
    // a real network request — which on CircleCI's amd_linux_test / arm_linux_test
    // executors hangs past the router's 30 s subgraph timeout. The router then emits
    // an `apollo.router.operations.fetch.duration` datapoint without the `has_errors`
    // attribute the assertion expects (the early
    // `apollo.operation.id`-stamped datapoint is the only one that lands within the
    // poll window). That was the `test_connector_request_emits_histogram` flake on
    // PR #9339's CircleCI build 366174.
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                subgraph_metrics: true
            include_subgraph_errors:
              all: true
            connectors:
              sources: {}
        "#,
        )
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "Awesome post",
            "body:": "This is a really great post",
            "userId": 1
        }])))
        .http_method("GET")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, _response) = router
        .execute_query(
            Query::builder()
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                .body(json!({"query":"query ExampleQuery {posts{id}}","variables":{}}))
                .build(),
        )
        .await;

    let expected = Metric::builder()
        .name("apollo.router.operations.fetch.duration".to_string())
        .attribute("graphql.operation.name", expected_operation_name)
        .attribute("apollo.client.name", expected_client_name)
        .attribute("apollo.client.version", expected_client_version)
        .attribute("subgraph.name", expected_service)
        .attribute("graphql.operation.type", expected_operation_type)
        .attribute("has_errors", false)
        .attribute("connector.source", expected_connector_source)
        .count(1)
        .build();
    let metrics = wait_for_metric_match(&mut router, &expected, Duration::from_secs(5)).await;
    assert!(!metrics.is_empty());
    assert_metrics_contain(&metrics, expected);
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_failed_connector_request_emits_histogram() {
    if !graph_os_enabled() {
        return;
    }
    let expected_operation_name = "ExampleQuery";
    let expected_client_name = "myClient";
    let expected_client_version = "v0.14";
    let expected_service = "connectors";
    let expected_operation_type = "query";
    let expected_connector_source = "jsonPlaceholder";

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(
            r#"
            telemetry:
              apollo:
                experimental_otlp_metrics_protocol: http
                metrics:
                  otlp:
                    batch_processor:
                      scheduled_delay: 100ms
                subgraph_metrics: true
            traffic_shaping:
                connector:
                    sources:
                        connectors.jsonPlaceholder:
                            timeout: 1ns
            include_subgraph_errors:
                all: true
            "#,
        )
        .supergraph(PathBuf::from_iter([
            "..",
            "apollo-router",
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(5)))
        .http_method("GET")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, _response) = router
        .execute_query(
            Query::builder()
                .header("apollographql-client-name", expected_client_name)
                .header("apollographql-client-version", expected_client_version)
                .body(json!({"query":"query ExampleQuery {posts{id}}","variables":{}}))
                .build(),
        )
        .await;

    let expected = Metric::builder()
        .name("apollo.router.operations.fetch.duration".to_string())
        .attribute("graphql.operation.name", expected_operation_name)
        .attribute("apollo.client.name", expected_client_name)
        .attribute("apollo.client.version", expected_client_version)
        .attribute("subgraph.name", expected_service)
        .attribute("graphql.operation.type", expected_operation_type)
        .attribute("has_errors", true)
        .attribute("connector.source", expected_connector_source)
        .count(1)
        .build();
    let metrics = wait_for_metric_match(&mut router, &expected, Duration::from_secs(5)).await;
    assert!(!metrics.is_empty());
    assert_metrics_contain(&metrics, expected);
    router.graceful_shutdown().await;
}

/// Assert that the given metric exists in the list of Otel requests. This is a crude attempt at
/// replicating _some_ assert_counter!() functionality since that test util can't be accessed here.
fn assert_metrics_contain(actual_metrics: &[ExportMetricsServiceRequest], expected_metric: Metric) {
    let (metric_found, observed) = metrics_contain_and_observed(actual_metrics, &expected_metric);

    assert!(
        metric_found,
        "Expected metric '{}' but no matching datapoint was found.\nInstead, actual metrics with matching name were:\n{}",
        expected_metric,
        observed
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Check whether `actual_metrics` contains a datapoint matching `expected_metric`. Returns
/// `(found, observed_datapoints_for_that_metric_name)` so callers can either assert with a
/// detailed message (see `assert_metrics_contain`) or loop polling for the metric to land
/// (see `wait_for_metric_match`).
fn metrics_contain_and_observed(
    actual_metrics: &[ExportMetricsServiceRequest],
    expected_metric: &Metric,
) -> (bool, Vec<Metric>) {
    let expected_name = &expected_metric.name;

    // Aggregate `data_points` across **all** `Metric` protos with this
    // name across all accumulated batches. Earlier code used a `.find()`
    // short-circuit that returned only the first matching proto, so
    // when the same metric name appeared in two separate
    // `ExportMetricsServiceRequest` batches (an early datapoint
    // without `has_errors`, a later one with), `wait_for_metric_match`
    // would re-evaluate the first batch's data points on every
    // iteration and never observe the later batch's enrichment — the
    // exact multi-batch case the helper exists to handle.
    let observed: Vec<Metric> = actual_metrics
        .iter()
        .flat_map(|req| &req.resource_metrics)
        .flat_map(|rm| &rm.scope_metrics)
        .flat_map(|sm| &sm.metrics)
        .filter(|m| m.name.as_str() == expected_name.as_str())
        .flat_map(|m| -> Vec<Metric> {
            match &m.data {
                Some(metric::Data::Sum(sum)) => sum
                    .data_points
                    .iter()
                    .map(|dp| Metric::from_number_datapoint(expected_name, dp))
                    .collect(),
                Some(metric::Data::Histogram(histogram)) => histogram
                    .data_points
                    .iter()
                    .map(|dp| Metric::from_histogram_datapoint(expected_name, dp))
                    .collect(),
                _ => panic!("Metric type for '{expected_name}' is not yet implemented"),
            }
        })
        .collect();

    let metric_found = observed.iter().any(|m| {
        // Only match values and attributes that are explicitly set
        expected_metric.value.is_none_or(|v| Some(v) == m.value)
            && expected_metric.sum.is_none_or(|s| Some(s) == m.sum)
            && expected_metric.count.is_none_or(|c| Some(c) == m.count)
            && m.attributes_contain(&expected_metric.attributes)
    });

    (metric_found, observed)
}

/// Deadline-bounded poll for a metric matching `expected_metric`.
///
/// `IntegrationTest::wait_for_emitted_otel_metrics` returns the first batch of metrics it
/// receives — but for connector subgraph fetches the router emits more than one batch
/// for `apollo.router.operations.fetch.duration`: an early datapoint stamped with
/// `apollo.operation.id` (no `has_errors` yet), and a subsequent datapoint with the
/// `has_errors` enrichment that the assertion expects. Returning on the first batch races
/// with the second batch arriving and yields a no-matching-datapoint flake.
///
/// Mirrors the deadline-bounded poll pattern used by
/// `verifier.rs::validate_*`, `apollo_otel_traces::get_traces`, and
/// `events.rs::EventTest::capture_logged_events` — keep draining batches into an
/// accumulator until either the expected metric arrives or the deadline expires.
/// `tests/common.rs::wait_for_emitted_otel_metrics` is intentionally not modified
/// here; the fix lives at the call site.
async fn wait_for_metric_match(
    router: &mut IntegrationTest,
    expected_metric: &Metric,
    deadline: Duration,
) -> Vec<ExportMetricsServiceRequest> {
    let start = std::time::Instant::now();
    let per_iter = Duration::from_millis(500);
    let mut accumulated: Vec<ExportMetricsServiceRequest> = Vec::new();
    loop {
        let batch = router.wait_for_emitted_otel_metrics(per_iter).await;
        accumulated.extend(batch);
        let (found, _) = metrics_contain_and_observed(&accumulated, expected_metric);
        if found || start.elapsed() >= deadline {
            return accumulated;
        }
    }
}

#[derive(Display, Clone, Debug)]
struct Metric {
    pub name: String,
    pub attributes: HashMap<String, AnyValue>,
    pub value: Option<i64>,
    pub sum: Option<f64>,
    pub count: Option<i64>,
}

#[buildstructor::buildstructor]
impl Metric {
    #[builder]
    fn new(
        name: String,
        attributes: HashMap<String, Value>,
        value: Option<i64>,
        sum: Option<f64>,
        count: Option<i64>,
    ) -> Self {
        Metric {
            name,
            attributes: attributes.into_iter().map(|(k, v)| (k, v.into())).collect(),
            value,
            sum,
            count,
        }
    }
    fn from_number_datapoint(name: &str, datapoint: &NumberDataPoint) -> Self {
        Metric {
            name: name.to_string(),
            attributes: datapoint
                .attributes
                .iter()
                .map(|kv| (kv.key.clone(), kv.value.clone().unwrap()))
                .collect::<HashMap<String, AnyValue>>(),
            value: match datapoint.value {
                Some(number_data_point::Value::AsInt(value)) => Some(value),
                _ => panic!("expected integer datapoint"),
            },
            sum: None,
            count: None,
        }
    }
    fn from_histogram_datapoint(name: &str, datapoint: &HistogramDataPoint) -> Self {
        Metric {
            name: name.to_string(),
            attributes: datapoint
                .attributes
                .iter()
                .map(|kv| (kv.key.clone(), kv.value.clone().unwrap()))
                .collect::<HashMap<String, AnyValue>>(),
            value: None,
            sum: datapoint.sum,
            count: Some(datapoint.count as i64),
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
            "name: {},\nvalue: {:?},\ncount: {:?},\nsum: {:?}, \nattributes: [",
            self.name, self.value, self.count, self.sum
        )?;
        let mut attrs: Vec<_> = self.attributes.iter().collect();
        attrs.sort_by(|a, b| a.0.cmp(b.0));
        for (i, (key, any)) in attrs.into_iter().enumerate() {
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
                    other => format!("{other:?}"),
                })
                .unwrap_or_else(|| "nil".into());
            write!(f, "\n\t{key}={value}")?;
        }
        write!(f, "\n]")
    }
}
