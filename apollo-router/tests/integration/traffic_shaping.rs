use std::time::Duration;

use insta::assert_yaml_snapshot;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_timeout() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
                traffic_shaping:
                    router:
                        timeout: 10ms
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 504);
    assert_yaml_snapshot!(response.text().await?);
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_timeout() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
                include_subgraph_errors:
                    all: true
                traffic_shaping:
                    all:
                        timeout: 10ms
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    assert_yaml_snapshot!(response.text().await?);
    router.graceful_shutdown().await;
    Ok(())
}
