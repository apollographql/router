use serde_json_bytes::json;
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
        .and(path("/api/v1/users/required/literal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!("hi")))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/url-properties.graphql"),
        &mock_server.uri(),
        "query { f(req: \"required\", repeated: [1,2,3]) }",
        Default::default(),
        Some(json!({
            "connectors": {
                "sources": {
                    "connectors.json": {
                        "$config": {
                            "host": mock_server.address().ip(),
                            "port": mock_server.address().port(),
                        }
                    }
                }
            }
        })),
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "f": "hi"
      }
    }
    "#);

    super::req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("GET")
                .path("/api/v1/users/required/literal")
                .query("q=1&repeated=1&repeated=2&repeated=3"),
        ],
    );
}
