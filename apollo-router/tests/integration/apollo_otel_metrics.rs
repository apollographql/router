use crate::integration::IntegrationTest;
use apollo_router::graphql;
use apollo_router::json_ext::Path;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use prost::Message;
use serde_json_bytes::json;
use std::time::Duration;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::metrics::v1::metric::Data;
use opentelemetry_proto::tonic::metrics::v1::number_data_point;
use tower::BoxError;
use wiremock::matchers::path;
use wiremock::matchers::{method, AnyMatcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn get_router_service(
    mock_otel_collector_uri: &str,
    mock_product_subgraph_uri: Option<&str>,
) -> IntegrationTest {
    let mut builder = IntegrationTest::builder()
        .config(include_str!("fixtures/apollo_otel_metrics.router.yaml")
            .replace("<REPLACE-ME>", mock_otel_collector_uri));
    if let Some(mock_product_subgraph_uri) = mock_product_subgraph_uri {
        builder = builder.subgraph_override("products", mock_product_subgraph_uri);
    }

    let mut router = builder.build().await;

    router.start().await;
    router.assert_started().await;

    router
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_error() -> Result<(), BoxError> {
    let expected_error_code = "SUBGRAPH_CODE";

    let mock_otel_server = MockServer::start().await;
    let mock_product_subgraph = MockServer::start().await;

    let router = get_router_service(
        &mock_otel_server.uri(),
        Some(&mock_product_subgraph.uri()),
    ).await;

    // Mock an error response for the subgraph
    Mock::given(AnyMatcher)
        .respond_with(ResponseTemplate::new(200).set_body_json(
            graphql::Response::builder()
                .data(json!({"data": null}))
                .errors(vec![
                    graphql::Error::builder()
                        .message("error in subgraph layer")
                        .extension_code(expected_error_code.clone())
                        .extension("service", "my_subgraph")
                        .path(Path::from("obj/field"))
                        .build(),
                ])
                .build()))
        .expect(1)
        .mount(&mock_product_subgraph)
        .await;

    // Assert the mock Otel Collector received the correct metrics
    Mock::given(method("POST"))
        .and(path("/v1/metrics"))
        .and(move |req: &wiremock::Request| {
            // Decode the OTLP request
            let req_msg = ExportMetricsServiceRequest::decode(req.body.as_ref());
            if !req_msg.is_ok() {
                return false;
            }
            // Navigate to the metric’s first data point
            let metrics = &req_msg.unwrap().resource_metrics[0]
                .scope_metrics[0]
                .metrics;
            let gql_error_data = metrics
                .iter()
                .find(|m| m.name == "apollo.router.graphql_error")
                .expect("GraphQL Error metric not found")
                .data
                .clone()
                .expect("No data found on GraphQL Error metric");
            if let Data::Sum(s) = gql_error_data {
                let data_points = &s.data_points[0];
                if let number_data_point::Value::AsInt(count) = data_points.value.expect("value not found for GraphQL error metric") {
                    return count == 1;
                }
                let actual_error_code = data_points
                    .attributes
                    .iter()
                    .find(|kv| kv.key == "code")
                    .and_then(|kv| kv
                        .value
                        .as_ref()
                        .map(|v| v.clone().value.expect("value not found for code key"))
                    )
                    .expect("GraphQL Error metric error code attribute not found");
                if let Value::StringValue(actual_error_code_str) = actual_error_code {
                    return actual_error_code_str == expected_error_code;
                }
            }
            return false;

            // TODO figure out why "apollo.router.operations.error" isn't there
        })
        .respond_with(ResponseTemplate::new(200))
        // One or more times
        .expect(1..)
        .mount(&mock_otel_server)
        .await;

    // Hit the product subgraph
    let (_trace_id, response) = router.execute_default_query().await;
    // TODO There has to be a better way, right?!?
    // Wait for metrics to send
    tokio::time::sleep(Duration::from_millis(10000)).await;

    Ok(())
}