use encoding_rs::WINDOWS_1252;
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
        None,
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
        None,
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
        None,
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
        None,
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
async fn text_body_maps_with_non_utf8_charset() {
    let (body, ..) = WINDOWS_1252.encode("test from server");
    let body = body.into_owned();
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(body, "text/plain; charset=windows-1252"),
        )
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { raw }",
        Default::default(),
        None,
        |_| {},
        None,
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
async fn text_body_maps_with_non_utf8_charset_using_invalid_utf8_bytes() {
    let bytes = [0x80]; // valid in Windows-1252 (e.g., €), invalid in UTF-8
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(bytes, "text/plain; charset=windows-1252"),
        )
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { raw }",
        Default::default(),
        None,
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "raw": "€"
      }
    }
    "#);
}

#[tokio::test]
async fn text_body_errors_on_invalid_chars_in_charset() {
    let bytes = [0xC0, 0xAF]; // invalid UTF-8 sequence
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(bytes, "text/plain; charset=utf-8"))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/content-type.graphql"),
        &mock_server.uri(),
        "query { raw }",
        Default::default(),
        None,
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "The server returned data in an unexpected format.",
          "path": [
            "raw"
          ],
          "extensions": {
            "code": "CONNECTOR_RESPONSE_INVALID",
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.raw[0]"
            },
            "http": {
              "status": 200
            }
          }
        }
      ]
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
        None,
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
        None,
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
        None,
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
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "The server returned data in an unexpected format.",
          "path": [
            "users"
          ],
          "extensions": {
            "code": "CONNECTOR_RESPONSE_INVALID",
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.users[0]"
            },
            "http": {
              "status": 200
            }
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
        None,
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
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "The server returned data in an unexpected format.",
          "path": [
            "users"
          ],
          "extensions": {
            "code": "CONNECTOR_RESPONSE_INVALID",
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.users[0]"
            },
            "http": {
              "status": 200
            }
          }
        }
      ]
    }
    "#);
}
