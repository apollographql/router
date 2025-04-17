use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use http::HeaderValue;
use serde_json::json;
use std::collections::HashMap;
use tower::BoxError;

#[tokio::test(flavor = "multi_thread")]
async fn test_content_negotiation() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config("supergraph:")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query": "{ __typename }"});

    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(query.clone())
                .content_type("application/json")
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);

    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(query)
                .headers(HashMap::from([(
                    "accept".to_string(),
                    "application/json,multipart/mixed;subscriptionSpec=1.0".to_string(),
                )]))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);

    // XX(@carodewig): this is the current behavior, but is not the behavior I would expect. Even
    //  though the response type is really just json, the router returns a multipart header because
    //  the client sends it in the accept header.
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        HeaderValue::from_str("multipart/mixed;boundary=\"graphql\";subscriptionSpec=1.0").unwrap()
    );

    Ok(())
}
