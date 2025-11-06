use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn only_source_no_error() {
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

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                }
            }
        }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { only_source { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "only_source": [
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
async fn only_source_with_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "error": {
                "message": "Something blew up!",
                "code": "BIG_BOOM"
            }
        })))
        .mount(&mock_server)
        .await;

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                }
            }
        }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { only_source { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "Something blew up!",
          "path": [
            "only_source"
          ],
          "extensions": {
            "code": "BIG_BOOM",
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.only_source[0]"
            },
            "http": {
              "status": 500
            },
            "status": 500,
            "fromSource": "a"
          }
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn only_connect_no_error() {
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

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                }
            }
        }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { only_connect { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "only_connect": [
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
async fn only_connect_with_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "error": {
                "message": "Something blew up!",
                "code": "BIG_BOOM"
            }
        })))
        .mount(&mock_server)
        .await;

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                }
            }
        }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { only_connect { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "Something blew up!",
          "path": [
            "only_connect"
          ],
          "extensions": {
            "code": "BIG_BOOM",
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.only_connect[0]"
            },
            "http": {
              "status": 500
            },
            "status": 500
          }
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn both_source_and_connect_no_error() {
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

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                }
            }
        }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { both_source_and_connect { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "both_source_and_connect": [
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
async fn both_source_and_connect_with_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "error": {
                "message": "Something blew up!",
                "code": "BIG_BOOM"
            }
        })))
        .mount(&mock_server)
        .await;

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                }
            }
        }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { both_source_and_connect { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    // Note that status 500 is NOT included in extensions because connect is overriding source
    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "Something blew up!",
          "path": [
            "both_source_and_connect"
          ],
          "extensions": {
            "code": "BIG_BOOM",
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.both_source_and_connect[0]"
            },
            "http": {
              "status": 500
            },
            "status": 500,
            "fromSource": "a",
            "fromConnect": "b"
          }
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn partial_source_and_partial_connect() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "error": {
                "message": "Something blew up!",
                "code": "BIG_BOOM"
            }
        })))
        .mount(&mock_server)
        .await;

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                },
                "connectors.withpartialconfig": {
                    "override_url": connector_uri
                }
            }
        }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { partial_source_and_partial_connect { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    // Note that status 500 is NOT included in extensions because connect is overriding source
    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "Something blew up!",
          "path": [
            "partial_source_and_partial_connect"
          ],
          "extensions": {
            "code": "BIG_BOOM",
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.partial_source_and_partial_connect[0]"
            },
            "http": {
              "status": 500
            },
            "status": 500,
            "fromSource": "a"
          }
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn redact_errors_when_include_subgraph_errors_disabled() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "error": {
                "message": "Something blew up!",
                "code": "BIG_BOOM"
            }
        })))
        .mount(&mock_server)
        .await;

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                }
            }
        },
        "include_subgraph_errors": {
          "subgraphs": {
              "connectors": false
          }
      }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { only_source { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "Subgraph errors redacted",
          "path": [
            "only_source"
          ]
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn does_not_redact_errors_when_include_subgraph_errors_enabled() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "error": {
                "message": "Something blew up!",
                "code": "BIG_BOOM"
            }
        })))
        .mount(&mock_server)
        .await;

    let connector_uri = format!("{}/", &mock_server.uri());
    let override_config = json!({
        "connectors": {
            "sources": {
                "connectors.withconfig": {
                    "override_url": connector_uri
                },
                "connectors.withoutconfig": {
                    "override_url": connector_uri
                }
            }
        },
        "include_subgraph_errors": {
          "subgraphs": {
              "connectors": true
          }
      }
    });

    let response = super::execute(
        include_str!("../testdata/errors.graphql"),
        &mock_server.uri(),
        "query { only_source { id name username } }",
        Default::default(),
        Some(override_config.into()),
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": null,
      "errors": [
        {
          "message": "Something blew up!",
          "path": [
            "only_source"
          ],
          "extensions": {
            "code": "BIG_BOOM",
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.only_source[0]"
            },
            "http": {
              "status": 500
            },
            "status": 500,
            "fromSource": "a"
          }
        }
      ]
    }
    "#);
}
