use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::req_asserts::Matcher;

#[tokio::test]
async fn url_properties() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/required/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!("hi")))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        &include_str!("../testdata/url-properties.graphql")
            .replace("http://localhost", &mock_server.uri()),
        &mock_server.uri(),
        "query { f(req: \"required\", repeated: [1,2,3]) }",
        Default::default(),
        None,
        |_| {},
        None,
    )
    .await;

    super::req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("GET")
                .path("/api/v1/users/required/")
                .query("q=1&repeated=1&repeated=2&repeated=3"),
        ],
    );

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "f": "hi"
      }
    }
    "#);
}
