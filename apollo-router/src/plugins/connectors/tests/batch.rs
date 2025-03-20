use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::req_asserts::Matcher;

const BATCH_SDL: &str = include_str!("../testdata/batch.graphql");

#[tokio::test]
async fn basic_batch() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
        { "id": 3 },
        { "id": 1 },
        { "id": 2 }])))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
        {
          "id": 1,
          "name": "Leanne Graham",
          "username": "Bret"
        },
        {
          "id": 2,
          "name": "Ervin Howell",
          "username": "Antonette"
        },
        {
          "id": 3,
          "name": "Clementine Bauch",
          "username": "Samantha"
        }])))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        BATCH_SDL,
        &mock_server.uri(),
        "query { users { id name username } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "users": [
          {
            "id": 3,
            "name": "Clementine Bauch",
            "username": "Samantha"
          },
          {
            "id": 1,
            "name": "Leanne Graham",
            "username": "Bret"
          },
          {
            "id": 2,
            "name": "Ervin Howell",
            "username": "Antonette"
          }
        ]
      }
    }
    "#);

    super::req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users"),
            Matcher::new()
                .method("POST")
                .path("/users-batch")
                .body(serde_json::json!({ "ids": [3,1,2] })),
        ],
    );
}
