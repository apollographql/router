use crate::integration::IntegrationTest;
use apollo_router::graphql;
use apollo_router::json_ext::Path;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::metrics::v1::metric::Data;
use opentelemetry_proto::tonic::metrics::v1::number_data_point;
use prost::Message;
use serde_json_bytes::json;
use std::time::Duration;
use tower::BoxError;
use wiremock::matchers::path;
use wiremock::matchers::{method, AnyMatcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn get_router_service(
    mock_otel_collector_uri: &str,
    mock_product_subgraph_uri: Option<&str>,
) -> IntegrationTest {
    let mut config_value: serde_yaml::Value = serde_yaml::from_str(include_str!("fixtures/apollo_otel_metrics.router.yaml"))
        .expect("config file invalid yaml");
    config_value["telemetry"]["exporters"]["metrics"]["otlp"]["endpoint"] = mock_otel_collector_uri.into();
    let mut builder = IntegrationTest::builder()
        .config(serde_yaml::to_string(&config_value).expect("invalid router config"));

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

    let mut router = get_router_service(
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
        .and(move |req: &wiremock::Request| matches_expected_metric(req, "apollo.router.graphql_error", 1, &expected_error_code))
        .respond_with(ResponseTemplate::new(200))
        // One or more times
        .expect(1..)
        .mount(&mock_otel_server)
        .await;

    // Wiremock doesn't currently support mocking GRPC services, so we cannot test for
    // apollo.router.operations.error as it is a private_realtime metric which does not currently
    // have controls for protocol. When that is added we can override the
    // experimental_otlp_endpoint to point at the mock and add a new matcher similar to above.

    // Hit the product subgraph
    let (_trace_id, response) = router.execute_default_query().await;
    // TODO There has to be a better way, right?!?
    // Wait for metrics to send
    tokio::time::sleep(Duration::from_millis(10000)).await;

    Ok(())
}

/// Matches a Wiremock request for the GraphQL error metric with the expected code.
fn matches_expected_metric(req: &wiremock::Request, expected_metric_name:&str, expected_sum: i64, expected_error_code: &str) -> bool {
    // Decode the OTLP request
    let req_msg = match ExportMetricsServiceRequest::decode(req.body.as_ref()) {
        Ok(m) => m,
        Err(e) => {
            let _temp = e.to_string();
            return false
        },
    };
    // Locate the GraphQL error metric and extract its sum data
    let sum = match req_msg.resource_metrics.get(0)
        .and_then(|rm| rm.scope_metrics.get(0))
        .and_then(|sm| sm.metrics.iter().find(|m| m.name == expected_metric_name))
        .and_then(|m| m.data.clone())
        .and_then(|data| if let Data::Sum(s) = data { Some(s) } else { None })
    {
        Some(s) => s,
        None => return false,
    };
    // Check the first data point's count or code attribute
    if let Some(dp) = sum.data_points.get(0) {
        // Count is correct
        if let Some(number_data_point::Value::AsInt(count)) = dp.value.clone() {
            if count != expected_sum {
                return false;
            }
        }
        // AND Error code attribute is correct
        if let Some(kv) = dp.attributes.iter().find(|kv| kv.key == "code") {
            if let Some(any) = &kv.value {
                if  let Some(Value::StringValue(actual_error_code)) = any.value.clone()
                {
                    return actual_error_code == expected_error_code;
                }
            }
        }
    }
    false
}

