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
        Some(serde_json_bytes::json!({
            "plugins": {
              "experimental.expose_query_plan": true
            }
        })),
        |req| {
            req.router_request
                .headers_mut()
                .append("Apollo-Expose-Query-Plan", "true".parse().unwrap());
        },
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
      },
      "extensions": {
        "apolloQueryPlan": {
          "object": {
            "kind": "QueryPlan",
            "node": {
              "kind": "Sequence",
              "nodes": [
                {
                  "kind": "Fetch",
                  "serviceName": "connectors.json http: GET /users",
                  "variableUsages": [],
                  "operation": "{ users { __typename id } }",
                  "operationName": null,
                  "operationKind": "query",
                  "id": null,
                  "inputRewrites": null,
                  "outputRewrites": null,
                  "contextRewrites": null,
                  "schemaAwareHash": "a6d44123ba4b1ffc978ce420274ee5ddcf270a45808f693b3215c26a6be8ef77",
                  "authorization": {
                    "is_authenticated": false,
                    "scopes": [],
                    "policies": []
                  }
                },
                {
                  "kind": "Flatten",
                  "path": [
                    "users",
                    "@"
                  ],
                  "node": {
                    "kind": "Fetch",
                    "serviceName": "[BATCH] connectors.json http: POST /users-batch",
                    "requires": [
                      {
                        "kind": "InlineFragment",
                        "typeCondition": "User",
                        "selections": [
                          {
                            "kind": "Field",
                            "name": "__typename"
                          },
                          {
                            "kind": "Field",
                            "name": "id"
                          }
                        ]
                      }
                    ],
                    "variableUsages": [],
                    "operation": "query($representations: [_Any!]!) { _entities(representations: $representations) { ... on User { name username } } }",
                    "operationName": null,
                    "operationKind": "query",
                    "id": null,
                    "inputRewrites": null,
                    "outputRewrites": null,
                    "contextRewrites": null,
                    "schemaAwareHash": "396d1e3e27938fc0e73c7b88af27da118c89e09484ad26a7f0d45d2da2c153be",
                    "authorization": {
                      "is_authenticated": false,
                      "scopes": [],
                      "policies": []
                    }
                  }
                }
              ]
            }
          },
          "text": "QueryPlan {\n  Sequence {\n    Fetch(service: \"connectors.json http: GET /users\") {\n      {\n        users {\n          __typename\n          id\n        }\n      }\n    },\n    Flatten(path: \"users.@\") {\n      Fetch(service: \"[BATCH] connectors.json http: POST /users-batch\") {\n        {\n          ... on User {\n            __typename\n            id\n          }\n        } =>\n        {\n          ... on User {\n            name\n            username\n          }\n        }\n      },\n    },\n  },\n}"
        }
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
