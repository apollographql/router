use http::header::CONTENT_TYPE;
use mime::APPLICATION_JSON;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::req_asserts::Matcher;
use super::req_asserts::Plan;

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
        include_str!("../testdata/batch.graphql"),
        &mock_server.uri(),
        "query { users { id name username } }",
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

#[tokio::test]
async fn basic_batch_query_params() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
        { "id": 3 },
        { "id": 1 },
        { "id": 2 }])))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user-details"))
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
        include_str!("../testdata/batch-query.graphql"),
        &mock_server.uri(),
        "query { users { id name username } }",
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
                .method("GET")
                .path("/user-details")
                .query("ids=3%2C1%2C2"),
        ],
    );
}

#[tokio::test]
async fn batch_missing_items() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 3 },
            { "id": 1 },
            { "id": 2 },
            { "id": 4 },
        ])))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            // 1 & 4 are not returned, so the extra fields should just null out (not be an error)
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
        include_str!("../testdata/batch.graphql"),
        &mock_server.uri(),
        "query { users { id name username } }",
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
            "name": "Clementine Bauch",
            "username": "Samantha"
          },
          {
            "id": 1,
            "name": null,
            "username": null
          },
          {
            "id": 2,
            "name": "Ervin Howell",
            "username": "Antonette"
          },
          {
            "id": 4,
            "name": null,
            "username": null
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
                .body(json!({ "ids": [3,1,2,4] })),
        ],
    );
}

#[tokio::test]
async fn connect_on_type() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
        { "id": 3 },
        { "id": 1 },
        { "id": 2 }])))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
        {
          "id": 1,
          "name": "Leanne Graham",
          "username": "Bret"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
        {
          "id": 2,
          "name": "Ervin Howell",
          "username": "Antonette"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
        {
          "id": 3,
          "name": "Clementine Bauch",
          "username": "Samantha"
        })))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/connect-on-type.graphql"),
        &mock_server.uri(),
        "query { users { id name username } }",
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

    Plan::Sequence(vec![
        Plan::Fetch(Matcher::new().method("GET").path("/users")),
        Plan::Parallel(vec![
            Matcher::new().method("GET").path("/users/1"),
            Matcher::new().method("GET").path("/users/2"),
            Matcher::new().method("GET").path("/users/3"),
        ]),
    ])
    .assert_matches(&mock_server.received_requests().await.unwrap());
}

#[tokio::test]
async fn connect_on_interface_object() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
    .and(path("/graphql"))
    .and(body_json(json!({"query": "{ users { __typename id ... on Employee { name } ... on Customer { name } } }"})))
    .respond_with(
        ResponseTemplate::new(200)
            .insert_header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .set_body_json(json!({
              "data": {
                "users": [{
                  "__typename": "Employee",
                  "id": "1",
                  "name": "Alice",
                }, {
                  "__typename": "Customer",
                  "id": "2",
                  "name": "Bob"
                }, {
                  "__typename": "Customer",
                  "id": "3",
                  "name": "Charlie"
                }]
              }
            })),
    ).mount(&mock_server).await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
        {
          "id": "1",
          "favoriteColor": "red"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
        {
          "id": "2",
          "favoriteColor": "green"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
        {
          "id": "3",
          "favoriteColor": "blue"
        })))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/connect-on-interface-object.graphql"),
        &mock_server.uri(),
        "{ users { id favoriteColor ... on Employee { name } ... on Customer { name } } }",
        Default::default(),
        None,
        |_| {},
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "users": [
          {
            "id": "1",
            "favoriteColor": "red",
            "name": "Alice"
          },
          {
            "id": "2",
            "favoriteColor": "green",
            "name": "Bob"
          },
          {
            "id": "3",
            "favoriteColor": "blue",
            "name": "Charlie"
          }
        ]
      }
    }
    "###);

    Plan::Sequence(vec![
        Plan::Fetch(Matcher::new().method("POST").path("/graphql")),
        Plan::Parallel(vec![
            Matcher::new().method("GET").path("/users/1"),
            Matcher::new().method("GET").path("/users/2"),
            Matcher::new().method("GET").path("/users/3"),
        ]),
    ])
    .assert_matches(&mock_server.received_requests().await.unwrap());
}

#[tokio::test]
async fn batch_with_max_size_under_batch_size() {
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
        include_str!("../testdata/batch-max-size.graphql"),
        &mock_server.uri(),
        "query { users { id name username } }",
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

#[tokio::test]
async fn batch_with_max_size_over_batch_size() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
        { "id": 3 },
        { "id": 1 },
        { "id": 2 },
        { "id": 4 },
        { "id": 5 },
        { "id": 6 },
        { "id": 7 },
        ])))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .and(body_json(json!({ "ids": [3,1,2,4,5] })))
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
        },
        {
          "id": 4,
          "name": "John Doe",
          "username": "jdoe"
        },
        {
          "id": 5,
          "name": "John Wick",
          "username": "jwick"
        },
        ])))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .and(body_json(json!({ "ids": [6,7] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
        {
          "id": 6,
          "name": "Jack Reacher",
          "username": "reacher"
        },
        {
          "id": 7,
          "name": "James Bond",
          "username": "jbond"
        }
        ])))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/batch-max-size.graphql"),
        &mock_server.uri(),
        "query { users { id name username } }",
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
          },
          {
            "id": 4,
            "name": "John Doe",
            "username": "jdoe"
          },
          {
            "id": 5,
            "name": "John Wick",
            "username": "jwick"
          },
          {
            "id": 6,
            "name": "Jack Reacher",
            "username": "reacher"
          },
          {
            "id": 7,
            "name": "James Bond",
            "username": "jbond"
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
                .body(serde_json::json!({ "ids": [3,1,2,4,5] })),
            Matcher::new()
                .method("POST")
                .path("/users-batch")
                .body(serde_json::json!({ "ids": [6,7] })),
        ],
    );
}
