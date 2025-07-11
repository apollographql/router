use crate::integration::IntegrationTest;
use apollo_router::graphql;
use apollo_router::json_ext::Path;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use prost::Message;
use serde_json_bytes::json;
use std::path::PathBuf;
use std::time::Duration;
use serde_json::Value;
use tower::BoxError;
use wiremock::matchers::path;
use wiremock::matchers::{method, AnyMatcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn get_router_service(
    mock_otel_collector_uri: &str,
    mock_product_subgraph_uri: Option<&str>
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
    let mock_otel_server = MockServer::start().await;
    let mock_product_subgraph = MockServer::start().await;

    let router = get_router_service(
        &mock_otel_server.uri(),
        Some(&mock_product_subgraph.uri())
    ).await;

    // Mock an error response for the subgraph
    Mock::given(AnyMatcher)
        .respond_with(ResponseTemplate::new(200).set_body_json(
            graphql::Response::builder()
                .data(json!({"data": null}))
                .errors(vec![
                    graphql::Error::builder()
                        .message("error in subgraph layer")
                        .extension_code("SUBGRAPH_CODE")
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
            // TODO assert match the request with correct values
            let message = ExportMetricsServiceRequest::decode(req.body.as_ref());
            message.is_ok()
        })
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_otel_server)
        .await;

    // Hit the product subgraph
    let (_trace_id, response) = router.execute_default_query().await;
    // TODO There has to be a better way, right?!?
    // Wait for metrics to send
    tokio::time::sleep(Duration::from_millis(100)).await;

    Ok(())
}