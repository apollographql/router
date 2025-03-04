use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

use super::req_asserts::Matcher;

const BATCH_HACK: &str = include_str!("../testdata/batch-hack.graphql");

#[tokio::test]
async fn value_from_config() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/users"))
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
        BATCH_HACK,
        &mock_server.uri(),
        "query { users(ids: [3,1,2]) { id name username } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "users": [{
          "id": 3,
          "name": "Clementine Bauch",
          "username": "Samantha"
        }, {
          "id": 1,
          "name": "Leanne Graham",
          "username": "Bret"
        }, {
          "id": 2,
          "name": "Ervin Howell",
          "username": "Antonette"
        }]
      }
    }
    "###);

    super::req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new()
            .method("GET")
            .path("/users")
            .body(serde_json::json!({ "ids": [3,1,2] }))],
    );
}
