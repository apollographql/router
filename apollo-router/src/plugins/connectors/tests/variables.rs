use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn test_env_var() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!("hi")))
        .mount(&mock_server)
        .await;

    unsafe {
        std::env::set_var(
            "CONNECTORS_TESTS_VARIABLES_TEST_ENV_VAR", // unique to this test
            "environment variable value",
        )
    };

    let response = super::execute(
        &include_str!("../testdata/env-var.graphql")
            .replace("http://localhost", &mock_server.uri()),
        &mock_server.uri(),
        "query { f { greeting fromEnv } }",
        Default::default(),
        None,
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "f": {
          "greeting": "hi",
          "fromEnv": "environment variable value"
        }
      }
    }
    "###);

    unsafe { std::env::remove_var("CONNECTORS_TESTS_VARIABLES_TEST_ENV_VAR") };
}
