use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
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
async fn basic_batch_query() {
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
