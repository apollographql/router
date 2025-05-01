use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn blank_body_maps_literal() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/literal"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/plain"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { literal }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "literal": "literal test"
      }
    }
    "#);
}

#[tokio::test]
async fn blank_body_raw_value_is_null() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/plain"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { raw }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "raw": null
      }
    }
    "#);
}

#[tokio::test]
async fn text_body_maps_literal() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/literal"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("test from server", "text/plain"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { literal }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "literal": "literal test"
      }
    }
    "#);
}

#[tokio::test]
async fn text_body_maps_raw_value() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("test from server", "text/plain"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { raw }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "raw": "test from server"
      }
    }
    "#);
}

#[tokio::test]
async fn other_content_type_maps_literal() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/literal"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<b>hello</b>", "text/html"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { literal }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "literal": "literal test"
      }
    }
    "#);
}

#[tokio::test]
async fn other_content_type_maps_raw_value_as_null() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<b>hello</b>", "text/html"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { raw }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "raw": null
      }
    }
    "#);
}

#[tokio::test]
async fn should_map_json_content_type() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
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
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { users {id name username} }",
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
          }
        ]
      }
    }
    "#);
}

#[tokio::test]
async fn should_error_on_invalid_with_json_content_type() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{_...]", "application/json"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { users {id name username} }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "Response deserialization failed",
          "path": [
            "users"
          ],
          "extensions": {
            "service": "connectors",
            "http": {
              "status": 200
            },
            "connector": {
              "coordinate": "connectors:Query.users@connect[0]"
            },
            "code": "CONNECTOR_DESERIALIZE"
          }
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn should_map_json_like_content_type() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                serde_json::json!([
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
                }])
                .to_string(),
                "application/vnd.foo+json",
            ),
        )
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { users {id name username} }",
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
          }
        ]
      }
    }
    "#);
}

#[tokio::test]
async fn should_error_on_invalid_with_json_like_content_type() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{_...]", "application/vnd.foo+json"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { users {id name username} }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "Response deserialization failed",
          "path": [
            "users"
          ],
          "extensions": {
            "service": "connectors",
            "http": {
              "status": 200
            },
            "connector": {
              "coordinate": "connectors:Query.users@connect[0]"
            },
            "code": "CONNECTOR_DESERIALIZE"
          }
        }
      ]
    }
    "#);
}
