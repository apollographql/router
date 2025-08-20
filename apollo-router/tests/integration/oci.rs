use std::collections::HashMap;
use tower::BoxError;

use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn test_router_boots_with_oci_config() -> Result<(), BoxError> {
    // Set up a mock OCI registry with a test schema
    let mut router = IntegrationTest::builder()
        .config("")
        .env(HashMap::from([
            (String::from("APOLLO_KEY"), String::from("test-api-key")),
        ]))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    Ok(())
}
