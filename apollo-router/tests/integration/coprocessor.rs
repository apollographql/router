use insta::assert_yaml_snapshot;
use tower::BoxError;

use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn test_error_not_propagated_to_client() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/broken_coprocessor.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 500);
    assert_yaml_snapshot!(response.text().await?);
    router.assert_log_contains("INTERNAL_SERVER_ERROR").await;
    router.graceful_shutdown().await;

    Ok(())
}
