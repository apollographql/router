use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::req_asserts::Matcher;

#[tokio::test]
async fn progressive_override() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
        { "id": 3, "name": "Clementine Bauch" },
        { "id": 1, "name": "Leanne Graham" },
        { "id": 2, "name": "Ervin Howell" }])))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/progressive-override.graphql"),
        &mock_server.uri(),
        "query { users { id name } }",
        Default::default(),
        None,
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "users": [
          {
            "id": 3,
            "name": "Clementine Bauch"
          },
          {
            "id": 1,
            "name": "Leanne Graham"
          },
          {
            "id": 2,
            "name": "Ervin Howell"
          }
        ]
      }
    }
    "#);

    super::req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/users")],
    );
}
