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

    // The two `/users-batch` POSTs have no inter-dependency (different `ids`
    // bodies, neither references the other's result), so their wire arrival
    // order is non-deterministic. Use Plan::Sequence + Plan::Parallel to
    // assert without depending on order.
    let plan = Plan::Sequence(vec![
        Plan::Fetch(Matcher::new().method("GET").path("/users")),
        Plan::Parallel(vec![
            Matcher::new()
                .method("POST")
                .path("/users-batch")
                .body(serde_json::json!({ "ids": [3,1,2,4,5] })),
            Matcher::new()
                .method("POST")
                .path("/users-batch")
                .body(serde_json::json!({ "ids": [6,7] })),
        ]),
    ]);
    plan.assert_matches(&mock_server.received_requests().await.unwrap());
}

// --- $batch DEDUPLICATION ----------------------------------------------------
//
// The router dedupes entity representations by value when it builds the
// `representations` variable for a fetch (see `Variables::new` in
// `query_planner/fetch.rs`). Connectors receive that already-deduped list as
// `$batch` and do no further dedup of their own. The tests below pin that
// contract down from the outside: what reaches the wire, and how the single
// returned entity is fanned back out to every position that referenced it.

/// The same entity referenced from several positions in the parent data
/// appears exactly once in `$batch`, and the one returned object is copied
/// back into every referencing position.
#[tokio::test]
async fn batch_dedupes_repeated_representations() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 3 },
            { "id": 1 },
            { "id": 3 },
            { "id": 2 },
            { "id": 1 },
        ])))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "Leanne Graham", "username": "Bret" },
            { "id": 2, "name": "Ervin Howell", "username": "Antonette" },
            { "id": 3, "name": "Clementine Bauch", "username": "Samantha" },
        ])))
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
            "id": 3,
            "name": "Clementine Bauch",
            "username": "Samantha"
          },
          {
            "id": 2,
            "name": "Ervin Howell",
            "username": "Antonette"
          },
          {
            "id": 1,
            "name": "Leanne Graham",
            "username": "Bret"
          }
        ]
      }
    }
    "#);

    super::req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users"),
            // Five references, three distinct ids, in order of first appearance.
            Matcher::new()
                .method("POST")
                .path("/users-batch")
                .body(json!({ "ids": [3, 1, 2] })),
        ],
    );
}

/// Dedup happens before `batch.maxSize` chunking, so chunk boundaries are
/// computed over distinct keys and a key never appears in two chunks of the
/// same fetch.
#[tokio::test]
async fn batch_dedupes_before_max_size_chunking() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 3 },
            { "id": 1 },
            { "id": 3 },
            { "id": 2 },
            { "id": 1 },
            { "id": 4 },
            { "id": 5 },
            { "id": 4 },
            { "id": 6 },
            { "id": 7 },
            { "id": 6 },
            { "id": 3 },
        ])))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .and(body_json(json!({ "ids": [3, 1, 2, 4, 5] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "Leanne Graham", "username": "Bret" },
            { "id": 2, "name": "Ervin Howell", "username": "Antonette" },
            { "id": 3, "name": "Clementine Bauch", "username": "Samantha" },
            { "id": 4, "name": "John Doe", "username": "jdoe" },
            { "id": 5, "name": "John Wick", "username": "jwick" },
        ])))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .and(body_json(json!({ "ids": [6, 7] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 6, "name": "Jack Reacher", "username": "reacher" },
            { "id": 7, "name": "James Bond", "username": "jbond" },
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
            "id": 3,
            "name": "Clementine Bauch",
            "username": "Samantha"
          },
          {
            "id": 2,
            "name": "Ervin Howell",
            "username": "Antonette"
          },
          {
            "id": 1,
            "name": "Leanne Graham",
            "username": "Bret"
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
            "id": 4,
            "name": "John Doe",
            "username": "jdoe"
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
          },
          {
            "id": 6,
            "name": "Jack Reacher",
            "username": "reacher"
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

    // Twelve references collapse to seven distinct ids, which `maxSize: 5`
    // splits into a chunk of five and a chunk of two. The two POSTs are
    // independent, so assert them order-free.
    let plan = Plan::Sequence(vec![
        Plan::Fetch(Matcher::new().method("GET").path("/users")),
        Plan::Parallel(vec![
            Matcher::new()
                .method("POST")
                .path("/users-batch")
                .body(json!({ "ids": [3, 1, 2, 4, 5] })),
            Matcher::new()
                .method("POST")
                .path("/users-batch")
                .body(json!({ "ids": [6, 7] })),
        ]),
    ]);
    plan.assert_matches(&mock_server.received_requests().await.unwrap());
}

/// Dedup is by whole representation, not by any one scalar. With a compound
/// key, two representations that share an `id` but differ in `region` are
/// distinct entities, so `$batch.id` legitimately repeats and each entity is
/// matched back by its full key.
#[tokio::test]
async fn batch_compound_key_keeps_representations_that_share_a_scalar() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "region": "A" },
            { "id": 1, "region": "B" },
            { "id": 1, "region": "A" },
        ])))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            // Returned in the opposite order from the request, to show that
            // matching is by key value rather than by position.
            { "id": 1, "region": "B", "name": "Alice from B" },
            { "id": 1, "region": "A", "name": "Alice from A" },
        ])))
        .mount(&mock_server)
        .await;

    let response = super::execute(
        include_str!("../testdata/batch-compound-key.graphql"),
        &mock_server.uri(),
        "query { users { id region name } }",
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
            "region": "A",
            "name": "Alice from A"
          },
          {
            "id": 1,
            "region": "B",
            "name": "Alice from B"
          },
          {
            "id": 1,
            "region": "A",
            "name": "Alice from A"
          }
        ]
      }
    }
    "#);

    super::req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users"),
            // Three references, two distinct (id, region) pairs. The `id`
            // scalar repeats because the representations differ in `region`.
            Matcher::new()
                .method("POST")
                .path("/users-batch")
                .body(json!({ "ids": [1, 1], "regions": ["A", "B"] })),
        ],
    );
}

/// Response alignment is by key value, never by position. The API may return
/// the batch in any order, include objects that were not requested, and omit
/// some that were. Each requested representation is matched to the returned
/// object carrying the same key; representations with no match null out.
#[tokio::test]
async fn batch_response_matched_by_key_not_position() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 3 },
            { "id": 1 },
            { "id": 3 },
            { "id": 2 },
            { "id": 4 },
        ])))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/users-batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            // Shuffled relative to the request, with an object nobody asked
            // for (99) and one that was asked for missing (4).
            { "id": 2, "name": "Ervin Howell", "username": "Antonette" },
            { "id": 99, "name": "Not Requested", "username": "extra" },
            { "id": 3, "name": "Clementine Bauch", "username": "Samantha" },
            { "id": 1, "name": "Leanne Graham", "username": "Bret" },
        ])))
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
            "id": 3,
            "name": "Clementine Bauch",
            "username": "Samantha"
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
                .body(json!({ "ids": [3, 1, 2, 4] })),
        ],
    );
}

/// The canonical statement of the dedup contract, in the shape the question
/// is usually asked: given input keys `[1, 1, 1, 2]`, the outbound request
/// asks for only the distinct keys (`?ids=1,2`), and results are fanned back
/// out to every original position.
#[tokio::test]
async fn batch_query_params_request_only_distinct_keys() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1 },
            { "id": 1 },
            { "id": 1 },
            { "id": 2 },
        ])))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user-details"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "Leanne Graham", "username": "Bret" },
            { "id": 2, "name": "Ervin Howell", "username": "Antonette" },
        ])))
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
            "id": 1,
            "name": "Leanne Graham",
            "username": "Bret"
          },
          {
            "id": 1,
            "name": "Leanne Graham",
            "username": "Bret"
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
            // `?ids=1,2`, URL-encoded: four references, two distinct keys.
            Matcher::new()
                .method("GET")
                .path("/user-details")
                .query("ids=1%2C2"),
        ],
    );
}
