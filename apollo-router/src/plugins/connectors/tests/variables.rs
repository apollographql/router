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

/// `$config` values substituted from `${env.*}` reach a connector's request body with the type
/// YAML gives their text, and an empty variable arrives as `null`.
#[tokio::test]
async fn expanded_config_values_keep_their_yaml_type_in_the_request_body() {
    let mock_server = MockServer::start().await;
    super::mock_api::create_user().mount(&mock_server).await;

    let expansion = crate::configuration::expansion::Expansion::builder()
        .supported_mode("env")
        .mocked_env_var("TIMEOUT", "50")
        .mocked_env_var("ENABLED", "false")
        .mocked_env_var("API_KEY", "")
        .build();
    let config = crate::configuration::parse_configuration(
        &format!(
            "connectors:\n  sources:\n    connectors.json:\n      override_url: {}/\n      $config:\n        timeout: ${{env.TIMEOUT}}\n        enabled: ${{env.ENABLED}}\n        apiKey: ${{env.API_KEY}}\n",
            mock_server.uri()
        ),
        expansion,
        crate::configuration::Migration::None,
    )
    .expect("the connector config is valid");
    let schema = super::MUTATION_SCHEMA.replace(
        r#"body: "username: $args.name""#,
        r#"body: "timeout: $config.timeout\nenabled: $config.enabled\napiKey: $config.apiKey""#,
    );

    super::execute_with_configuration(
        &schema,
        "mutation { createUser(name: \"New User\") { success } }",
        Default::default(),
        config,
        |_| {},
        None,
    )
    .await;

    let requests = mock_server.received_requests().await.unwrap();
    let body: serde_json::Value = requests[0].body_json().unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "timeout": 50, "enabled": false, "apiKey": null })
    );
}
