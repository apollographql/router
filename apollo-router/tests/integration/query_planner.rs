use std::path::PathBuf;
use std::time::Duration;

use reqwest::Client;
use serde_json::json;

use crate::integration::IntegrationTest;
use crate::integration::common::graph_os_enabled;

mod max_evaluated_plans;

const PROMETHEUS_METRICS_CONFIG: &str = include_str!("telemetry/fixtures/prometheus.router.yaml");

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
async fn test_cooperative_cancellation_timeout() {
    let config = json!({
        "supergraph": {
            "query_planning": {
                "experimental_cooperative_cancellation": {
                    "enforce": {
                        "enabled_with_timeout_in_seconds": 0.0001
                    }
                }
            }
        },
        "telemetry": {
            "exporters": {
                "metrics": {
                    "prometheus": {
                        "enabled": true
                    }
                }
            }
        }
    });

    let slow_subgraph_response = wiremock::ResponseTemplate::new(200)
        .set_delay(Duration::from_secs(10))
        .set_body_json(json!({
            "data": {
                "topProducts": [
                    { "name": "Table", "upc": "1", "reviews": [] },
                ],
            },
        }));

    let mut router = IntegrationTest::builder()
        .config(serde_yaml::to_string(&config).unwrap())
        .responder(slow_subgraph_response)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let client = Client::new();
    let router_url = format!("http://{}/", router.bind_address());

    let query = json!({
        "query": "query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"
    });

    let response = client
        .post(&router_url)
        .header("content-type", "application/json")
        .json(&query)
        .send()
        .await
        .expect("Request should complete (with error)");

    let body: serde_json::Value = response.json().await.expect("Response should be JSON");

    assert!(body["errors"].is_array(), "Response should contain errors");
    let errors = body["errors"].as_array().unwrap();
    assert!(!errors.is_empty(), "Should have at least one error");

    let error_message = errors[0]["message"].as_str().unwrap_or("");
    assert!(
        error_message.contains("timed out")
            || error_message.contains("timeout")
            || error_message.contains("cancelled"),
        "Error should mention timeout or cancellation, got: {}",
        error_message
    );

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cooperative_cancellation_measure_mode() {
    let config = json!({
        "supergraph": {
            "query_planning": {
                "experimental_cooperative_cancellation": {
                    "measure": {
                        "enabled_with_timeout_in_seconds": 0.001
                    }
                }
            }
        },
        "telemetry": {
            "exporters": {
                "metrics": {
                    "prometheus": {
                        "enabled": true
                    }
                }
            }
        }
    });

    let slow_subgraph_response = wiremock::ResponseTemplate::new(200)
        .set_delay(Duration::from_secs(1))
        .set_body_json(json!({
            "data": {
                "topProducts": [
                    { "name": "Table", "upc": "1", "reviews": [] },
                ],
            },
        }));

    let mut router = IntegrationTest::builder()
        .config(serde_yaml::to_string(&config).unwrap())
        .responder(slow_subgraph_response)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let client = Client::new();
    let router_url = format!("http://{}/", router.bind_address());

    let query = json!({
        "query": "query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"
    });

    for _ in 0..3 {
        let _response = client
            .post(&router_url)
            .header("content-type", "application/json")
            .json(&query)
            .send()
            .await
            .expect("Request should complete");

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    router
        .assert_metrics_contains(
            "apollo_router_operations_cooperative_cancellations_total",
            None,
        )
        .await;

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cooperative_cancellation_client_disconnect() {
    let config = json!({
        "supergraph": {
            "query_planning": {
                "experimental_cooperative_cancellation": {
                    "enforce": "enabled"
                }
            }
        },
        "telemetry": {
            "exporters": {
                "metrics": {
                    "prometheus": {
                        "enabled": true
                    }
                }
            }
        }
    });

    // Create a normal subgraph response
    let subgraph_response = wiremock::ResponseTemplate::new(200).set_body_json(json!({
        "data": {
            "topProducts": [
                { "name": "Table", "upc": "1", "reviews": [] },
            ],
        },
    }));

    let mut router = IntegrationTest::builder()
        .config(serde_yaml::to_string(&config).unwrap())
        .responder(subgraph_response)
        .build()
        .await;

    // Set up lifecycle monitoring
    router
        .setup_lifecycle_monitoring()
        .await
        .expect("Failed to setup lifecycle monitoring");

    router.start().await;
    router.assert_started().await;

    let client = Client::new();
    let router_url = format!("http://{}/", router.bind_address());

    // Use a complex query to ensure query planning takes some time
    let query = json!({
        "query": "query ComplexQuery { topProducts(first: 5) { upc name price reviews { id author { id name } product { name upc } } } me { id name username reviews { id author { id name } product { name upc price reviews { id author { id name } } } } } }"
    });

    // Start a request
    let client_clone = client.clone();
    let router_url_clone = router_url.clone();
    let query_clone = query.clone();

    let request_task = tokio::spawn(async move {
        client_clone
            .post(&router_url_clone)
            .header("content-type", "application/json")
            .json(&query_clone)
            .timeout(Duration::from_millis(5)) // Very short timeout to force disconnect during planning
            .send()
            .await
    });

    // Wait for query planning to start
    router
        .wait_for_query_planning_start()
        .await
        .expect("Query planning should start");

    // Give the disconnect a moment to be detected
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Wait for query planning to end (it should be cancelled)
    router
        .wait_for_query_planning_end()
        .await
        .expect("Query planning should end");

    // Check the request result to confirm timeout
    match request_task.await {
        Ok(Err(e)) if e.is_timeout() => {
            println!("Request timed out as expected");
        }
        Ok(Ok(_)) => {
            println!("Request completed successfully (unexpected)");
        }
        Ok(Err(e)) => {
            println!("Request failed with error: {:?}", e);
        }
        Err(e) => {
            println!("Request task error: {:?}", e);
        }
    }

    // Give time for metrics to be updated
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify that cooperative cancellation metrics were recorded
    router
        .assert_metrics_contains(
            "apollo_router_operations_cooperative_cancellations_total",
            Some(Duration::from_secs(3)),
        )
        .await;

    router.graceful_shutdown().await;
}
