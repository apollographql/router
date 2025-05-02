use std::collections::HashMap;

use http::HeaderValue;
use serde_json::json;
use tower::BoxError;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

#[tokio::test(flavor = "multi_thread")]
async fn test_content_negotiation() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config("supergraph:")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query": "{ __typename }"});
    for accept_header in [
        "application/json",
        "application/json,multipart/mixed;subscriptionSpec=1.0",
    ] {
        let (_, response) = router
            .execute_query(
                Query::builder()
                    .body(query.clone())
                    .headers(HashMap::from([(
                        "accept".to_string(),
                        accept_header.to_string(),
                    )]))
                    .build(),
            )
            .await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            HeaderValue::from_str("application/json").unwrap()
        );
    }

    Ok(())
}
