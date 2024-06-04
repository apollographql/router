use std::sync::Arc;

use futures::StreamExt;
use http::header::CONTENT_TYPE;
use itertools::EitherOrBoth;
use itertools::Itertools;
use mime::APPLICATION_JSON;
use req_asserts::Matcher;
use tower::ServiceExt;
use wiremock::matchers::body_json;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

use crate::router_factory::RouterSuperServiceFactory;
use crate::router_factory::YamlRouterFactory;
use crate::services::new_service::ServiceFactory;
use crate::services::supergraph;

pub(crate) mod mock_api {
    struct PathTemplate(String);

    impl wiremock::Match for PathTemplate {
        fn matches(&self, request: &wiremock::Request) -> bool {
            let path = request.url.path();
            let path = path.split('/');
            let template = self.0.split('/');

            for pair in path.zip_longest(template) {
                match pair {
                    EitherOrBoth::Both(p, t) => {
                        if t.starts_with('{') && t.ends_with('}') {
                            continue;
                        }

                        if p != t {
                            return false;
                        }
                    }
                    _ => return false,
                }
            }
            true
        }
    }

    fn path_template(template: &str) -> PathTemplate {
        PathTemplate(template.to_string())
    }

    use super::*;

    pub(crate) fn hello() -> Mock {
        Mock::given(method("GET"))
            .and(path("/v1/hello"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": {
                "id": 42,
              }
            })))
    }

    pub(crate) fn hello_id() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/hello/{id}"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": {
                "id": 42,
                "field": "hello",
                "enum_value": "A"
              }
            })))
    }

    pub(crate) fn hello_id_world() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/hello/{id}/world"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": {
                "field": "world",
                "nested": {
                  "field": "hi"
                }
              }
            })))
    }

    pub(crate) fn with_arguments() -> Mock {
        Mock::given(method("GET"))
            .and(path("/v1/with-arguments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": "hello world"
            })))
    }

    pub(crate) fn mutation() -> Mock {
        Mock::given(method("POST"))
            .and(path("/v1/mutation"))
            // don't assert body here because we can do that with matchers later
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": { "success": true }
            })))
    }

    pub(crate) fn entity() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/entity/{a}/{b}"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": {
                "d": "d"
              }
            })))
    }

    pub(crate) fn entity_e() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/entity/{a}/{b}/e"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": "e"
            })))
    }

    pub(crate) fn entity_f() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/entity/{d}"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": "f"
            })))
    }

    pub(crate) fn interface_object_id() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/interface-object/{id}"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": {
                "c": "c: 1"
              }
            })))
    }

    pub(crate) fn interface_object_id_d() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/interface-object/{id}/d"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": "d: 1"
            })))
    }

    pub(crate) fn entity_interface_a() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/entity-interface/a-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": {
                "id": "a-1",
                "kind": "a",
                "a": "a1"
              }
            })))
    }

    pub(crate) fn entity_interface_b() -> Mock {
        Mock::given(method("GET"))
            .and(path_template("/v1/entity-interface/b-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": {
                "id": "b-2",
                "kind": "b",
                "b": "b2"
              }
            })))
    }

    pub(crate) fn interfaces() -> Mock {
        Mock::given(method("GET"))
            .and(path("/v1/interfaces"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": [
                {
                  "id": "i1",
                  "a": "a1",
                  "nested": {
                    "id": "ni1",
                    "a": "na1"
                  }
                },
                {
                  "id": "i2",
                  "b": "b2",
                  "nested": {
                    "id": "ni2",
                    "b": "nb2"
                  }
                }
              ]
            })))
    }

    pub(crate) fn unions() -> Mock {
        Mock::given(method("GET"))
            .and(path("/v1/unions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": [
                {
                  "z": "a1",
                  "nested": {
                    "x": "na1"
                  }
                },
                {
                  "y": "b2"
                }
              ]
            })))
    }

    pub(crate) fn shipping() -> Mock {
        Mock::given(method("GET"))
            .and(path("/v1/shipping"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "data": 100
            })))
    }

    pub(crate) async fn mount_all(server: &MockServer) {
        futures::stream::iter(vec![
            hello(),
            hello_id(),
            hello_id_world(),
            with_arguments(),
            mutation(),
            entity(),
            entity_e(),
            entity_f(),
            interface_object_id(),
            interface_object_id_d(),
            entity_interface_a(),
            entity_interface_b(),
            interfaces(),
            unions(),
            shipping(),
        ])
        .then(|mock| async { mock.mount(server).await })
        .collect::<Vec<_>>()
        .await;
    }
}

pub(crate) mod mock_subgraph {
    use super::*;

    pub(crate) fn start_join() -> Mock {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(serde_json::json!({
              "query": "{startJoin{__typename a b c}}"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .set_body_json(serde_json::json!({
                      "data": {
                        "startJoin": {
                          "__typename": "EntityAcrossBoth",
                          "a": "a",
                          "b": "b",
                          "c": "c",
                        }
                      }
                    })),
            )
    }

    pub(crate) fn test_requires() -> Mock {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(serde_json::json!({
              "query": "{requires{__typename id weight}}"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .set_body_json(serde_json::json!({
                      "data": {
                        "requires": {
                          "__typename": "TestRequires",
                          "id": "123",
                          "weight": "50",
                        }
                      }
                    })),
            )
    }

    pub(crate) fn interface_object() -> Mock {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(serde_json::json!({
              "query": "{interfaceObject{__typename id ...on IOa{__typename id a}...on IOb{__typename id b}}}"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .set_body_json(serde_json::json!({
                      "data": {
                        "interfaceObject": [
                          {
                            "__typename": "IOa",
                            "id": "a-1",
                            "a": "a1",
                          },
                          {
                            "__typename": "IOb",
                            "id": "b-2",
                            "b": "b102",
                          },
                        ]
                      }
                    })),
            )
    }

    pub(crate) fn entity_interface() -> Mock {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(serde_json::json!({
              "query": "{entityInterface{__typename id c}}"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .set_body_json(serde_json::json!({
                      "data": {
                        "entityInterface": [
                          {
                            "__typename": "EntityInterface",
                            "id": "a-1",
                            "c": "c-a1",
                          },
                          {
                            "__typename": "EntityInterface",
                            "id": "b-2",
                            "c": "c-b2",
                          },
                        ]
                      }
                    })),
            )
    }
}

#[tokio::test]
async fn test_root_field_plus_entity() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;

    // @sourceField on Query.hello
    // @sourceType on Hello
    let response = execute(
        &mock_server.uri(),
        "query { hello { id field enum } }",
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "hello": {
          "id": 42,
          "field": "hello",
          "enum": "A"
        }
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/v1/hello").build(),
            Matcher::new().method("GET").path("/v1/hello/42").build(),
        ],
    );
}

#[tokio::test]
async fn test_root_field_plus_entity_field() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;

    // @sourceField on Query.hello
    // @sourceField on Hello.world
    let response = execute(
        &mock_server.uri(),
        "query { hello { __typename id world { __typename field nested { __typename field }} } }",
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/v1/hello").build(),
            Matcher::new()
                .method("GET")
                .path("/v1/hello/42/world")
                .build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "hello": {
          "__typename": "Hello",
          "id": 42,
          "world": {
            "__typename": "World",
            "field": "world",
            "nested": {
              "__typename": "Nested",
              "field": "hi"
            }
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn test_query_parameters() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;

    // @sourceField on Query.withArguments
    let response = execute(
        &mock_server.uri(),
        "query { withArguments(done: true, value: \"bye\", enum: Y) }",
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new()
            .method("GET")
            .path("/v1/with-arguments")
            .query("value=bye&done=true&enum_value=Y")
            .build()],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "withArguments": "hello world"
      }
    }
    "###);
}

#[tokio::test]
async fn test_mutation_inputs() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;

    // @sourceField on Mutation.mutation
    let response = execute(
        &mock_server.uri(),
        "mutation { mutation(input: { nums: [1,2,3], values: [{ num: 42 }]}) { success } }",
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new()
            .method("POST")
            .path("/v1/mutation")
            .body(serde_json::json!({
              "nums": [1, 2, 3],
              "values": [{
                "num": 42
              }]
            }))
            .build()],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "mutation": {
          "success": true
        }
      }
    }
    "###);
}

#[tokio::test]
async fn test_entity_join() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;

    mock_subgraph::start_join().mount(&mock_server).await;

    // Query.startJoin from subgraph
    // @sourceType on EntityAcrossBoth
    // @sourceField on EntityAcrossBoth.e (parallel)
    // @sourceField on EntityAcrossBoth.f (sequence)
    let response = execute(
        &mock_server.uri(),
        "query { startJoin { a b c d e f } }",
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("POST")
                .path("/graphql")
                .body(serde_json::json!({
                  "query": "{startJoin{__typename a b c}}"
                }))
                .build(),
            Matcher::new().method("GET").path("/v1/entity/a/b").build(),
            Matcher::new()
                .method("GET")
                .path("/v1/entity/a/b/e")
                .build(),
            Matcher::new().method("GET").path("/v1/entity/d").build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "startJoin": {
          "a": "a",
          "b": "b",
          "c": "c",
          "d": "d",
          "e": "e",
          "f": "f"
        }
      }
    }
    "###);
}

#[tokio::test]
async fn test_aliases_on_connector_fields() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;
    mock_subgraph::start_join().mount(&mock_server).await;

    // @sourceField on Query
    let response = execute(
        &mock_server.uri(),
        r#"
        query {
          alias1: hello { id }
          alias2: hello { id }
          startJoin { a b c alias3: e alias4: e }
        }
        "#,
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/v1/hello").build(),
            Matcher::new().method("GET").path("/v1/hello").build(),
            Matcher::new()
                .method("POST")
                .path("/graphql")
                .body(serde_json::json!({
                  "query": "{startJoin{__typename a b c}}"
                }))
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/entity/a/b/e")
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/entity/a/b/e")
                .build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "alias1": {
          "id": 42
        },
        "alias2": {
          "id": 42
        },
        "startJoin": {
          "a": "a",
          "b": "b",
          "c": "c",
          "alias3": "e",
          "alias4": "e"
        }
      }
    }
    "###);
}

#[tokio::test]
async fn test_response_formatting_aliases() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;
    mock_subgraph::start_join().mount(&mock_server).await;

    // @sourceField on Query
    // @sourceField on Hello.world
    // @sourceType on Hello
    let response = execute(
        &mock_server.uri(),
        r#"
        query {
          hello {
            a: id
            b: field
            c: world {
              d: field
              e: nested {
                f: field
              }
            }
          }
        }
        "#,
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/v1/hello").build(),
            Matcher::new()
                .method("GET")
                .path("/v1/hello/42/world")
                .build(),
            Matcher::new().method("GET").path("/v1/hello/42").build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "hello": {
          "a": 42,
          "b": "hello",
          "c": {
            "d": "world",
            "e": {
              "f": "hi"
            }
          }
        }
      }
    }
    "###);
}

#[tokio::test]
#[ignore] // TODO
async fn test_interface_object() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;
    mock_subgraph::interface_object().mount(&mock_server).await;

    // @sourceType on TestingInterfaceObject
    // @sourceField on TestingInterfaceObject.d
    let response = execute(
        &mock_server.uri(),
        r#"
        query {
          interfaceObject {
            __typename
            id
            ... on IOa {
              __typename
              a
              c
              d
            }
            ... on IOb {
              __typename
              b
              c
              d
            }
          }
        }
        "#,
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "interfaceObject": [
          {
            "__typename": "IOa",
            "id": "a-1",
            "a": "a1",
            "c": "c: 1",
            "d": "d: 1"
          },
          {
            "__typename": "IOb",
            "id": "b-2",
            "b": "b102",
            "c": "c: 1",
            "d": "d: 1"
          }
        ]
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("POST")
                .path("/graphql")
                .body(serde_json::json!({
                  "query": "{interfaceObject{__typename id ...on IOa{__typename id a}...on IOb{__typename id b}}}"
                }))
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/interface-object/a-1")
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/interface-object/b-2")
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/interface-object/a-1/d")
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/interface-object/b-2/d")
                .build(),
        ],
    );
}

#[tokio::test]
async fn test_entity_interface() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;
    mock_subgraph::entity_interface().mount(&mock_server).await;

    // @sourceType on EntityInterface
    let response = execute(
        &mock_server.uri(),
        r#"
        query {
          entityInterface {
            __typename
            id
            ... on EIa {
              a
            }
            ... on EIb {
              b
            }
            c
          }
        }
        "#,
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("POST")
                .path("/graphql")
                .body(serde_json::json!({
                  "query": "{entityInterface{__typename id c}}"
                }))
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/entity-interface/a-1")
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/entity-interface/b-2")
                .build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "entityInterface": [
          {
            "__typename": "EIa",
            "id": "a-1",
            "a": "a1",
            "c": "c-a1"
          },
          {
            "__typename": "EIb",
            "id": "b-2",
            "b": "b2",
            "c": "c-b2"
          }
        ]
      }
    }
    "###);
}

#[tokio::test]
#[ignore] // TODO
async fn test_interfaces() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;
    mock_subgraph::interface_object().mount(&mock_server).await;

    // @sourceField on Query.interfaces
    let response = execute(
        &mock_server.uri(),
        r#"
        query {
          interfaces {
            __typename
            id
            ... on Ia {
              a
            }
            ... on Ib {
              b
            }
            nested {
              __typename
              id
              ... on NIa {
                a
              }
              ... on NIb {
                b
              }
            }
          }
        }
        "#,
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/v1/interfaces").build()],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "interfaces": [
          {
            "__typename": "Ia",
            "id": "i1",
            "a": "a1",
            "nested": {
              "__typename": "NIa",
              "id": "ni1",
              "a": "na1"
            }
          },
          {
            "__typename": "Ib",
            "id": "i2",
            "b": "b2",
            "nested": {
              "__typename": "NIb",
              "id": "ni2",
              "b": "nb2"
            }
          }
        ]
      }
    }
    "###);
}

#[tokio::test]
async fn test_unions() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;
    mock_subgraph::interface_object().mount(&mock_server).await;

    // @sourceField on Query.unions
    let response = execute(
        &mock_server.uri(),
        r#"
        query {
          unions {
            __typename
            ... on UnionA {
              z
              nested {
                __typename
                ... on NestedUnionC {
                  x
                }
                ... on NestedUnionD {
                  w
                }
              }
            }
            ... on UnionB {
              y
            }
          }
        }
        "#,
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/v1/unions").build()],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "unions": [
          {
            "__typename": "UnionA",
            "z": "a1",
            "nested": {
              "__typename": "NestedUnionC",
              "x": "na1"
            }
          },
          {
            "__typename": "UnionB",
            "y": "b2"
          }
        ]
      }
    }
    "###);
}

#[tokio::test]
async fn basic_errors() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/hello"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
          "error": "not found"
        })))
        .mount(&mock_server)
        .await;

    // @sourceField on Query
    let response = execute(&mock_server.uri(), "{ hello { id } }", None).await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/v1/hello").build()],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": null,
      "errors": [
        {
          "message": "http error: 404 Not Found",
          "extensions": {
            "connector": "kitchen-sink.a: GET /hello",
            "code": "404"
          }
        }
      ]
    }
    "###);
}

// TODO: fix when we refactor
#[tokio::test]
async fn test_requires() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;
    mock_subgraph::test_requires().mount(&mock_server).await;

    // @sourceField on TestRequires.shippingCost
    let response = execute(
        &mock_server.uri(),
        "query { requires { shippingCost } }",
        None,
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "requires": {
          "shippingCost": 100
        }
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("POST")
                .path("/graphql")
                .body(serde_json::json!({
                  "query": "{requires{__typename id weight}}"
                }))
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/shipping")
                .query("weight=50")
                .build(),
        ],
    );
}

#[tokio::test]
#[ignore] // Composition doesn't currently allow adding a sourceField on a non-entity type.
async fn test_internal_dependencies() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/internal_dependency"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "data": {
            "a": 42,
            "b": 108,
          }
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/internal_dependency/c"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "data": 150
        })))
        .mount(&mock_server)
        .await;

    // @sourceField on Query.internal_dependency
    // @sourceField on TestInternalDependency.c
    let response = execute(
        &mock_server.uri(),
        "query { internal_dependencies { c } }",
        None,
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("GET")
                .path("/v1/internal_dependency")
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/internal_dependency/c")
                .query("a=42&b=108")
                .build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "internal_dependencies": {
          "c": 150
        }
      }
    }
    "###);
}

#[tokio::test]
async fn test_simple_header_propagation() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;

    // @sourceField on Query.hello
    // @sourceType on Hello
    let response = execute(
        &mock_server.uri(),
        "query { hello { id field enum } }",
        Some(serde_json::json!({
          "include_subgraph_errors": { "all": true },
          "headers": {
            "subgraphs": {
              "kitchen-sink": {
                "request": [
                  {
                    "propagate": {
                      "named": "x-client-header"
                    }
                  },
                  {
                    "insert": {
                      "name": "x-api-key",
                      "value": "abcd1234"
                    }
                  }
                ]
              }
            }
          }
        })),
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("GET")
                .path("/v1/hello")
                .header("x-api-key".into(), "abcd1234".parse().unwrap())
                .header(
                    "x-client-header".into(),
                    "client-header-value".parse().unwrap(),
                )
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/hello/42")
                .header("x-api-key".into(), "abcd1234".parse().unwrap())
                .header(
                    "x-client-header".into(),
                    "client-header-value".parse().unwrap(),
                )
                .build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "hello": {
          "id": 42,
          "field": "hello",
          "enum": "A"
        }
      }
    }
    "###);
}

#[tokio::test]
async fn test_directive_header_propagation() {
    let mock_server = MockServer::start().await;
    mock_api::mount_all(&mock_server).await;

    // @sourceField on Query.helloWithHeaders (from the "with_headers" sourceAPI)
    // @sourceType on Hello
    let response = execute(
        &mock_server.uri(),
        "query { helloWithHeaders { id field enum } }",
        Some(serde_json::json!({
          "include_subgraph_errors": { "all": true },
          "headers": {
            "subgraphs": {
              "kitchen-sink": {
                "request": [
                  {
                    "propagate": {
                      "named": "x-client-header"
                    }
                  },
                  {
                    "insert": {
                      "name": "x-propagate",
                      "value": "propagated"
                    }
                  },
                  {
                    "insert": {
                      "name": "x-rename",
                      "value": "renamed"
                    }
                  },
                  {
                    "insert": {
                      "name": "x-ignore",
                      "value": "ignored"
                    }
                  }
                ]
              }
            }
          }
        })),
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("GET")
                .path("/v1/hello")
                .header("x-propagate".into(), "propagated".parse().unwrap())
                .header("x-new-name".into(), "renamed".parse().unwrap())
                .header("x-insert".into(), "inserted".parse().unwrap())
                .build(),
            Matcher::new()
                .method("GET")
                .path("/v1/hello/42")
                // these are the passthrough headers because this connector uses the "a" api\
                .header(
                    "x-client-header".into(),
                    "client-header-value".parse().unwrap(),
                )
                .header("x-propagate".into(), "propagated".parse().unwrap())
                .header("x-rename".into(), "renamed".parse().unwrap())
                .header("x-ignore".into(), "ignored".parse().unwrap())
                .build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "helloWithHeaders": {
          "id": 42,
          "field": "hello",
          "enum": "A"
        }
      }
    }
    "###);
}

#[tokio::test]
async fn test_request_deduping() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/hellos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "data": [{
            "id": 1,
            "relatedId": 123
          }, {
            "id": 2,
            "relatedId": 234
          }, {
            "id": 3,
            "relatedId": 234
          },
          {
            "id": 4,
            "relatedId": 123
          }]
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/related/123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "data": {"id": 123, "field": "related 1"}
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/related/234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "data": {"id": 234, "field": "related 2"}
        })))
        .mount(&mock_server)
        .await;

    let response = execute(
        &mock_server.uri(),
        "query { hellos { id related { id field } } }",
        None,
    )
    .await;

    assert_eq!(response.as_object().unwrap().get("errors"), None);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/v1/hellos").build(),
            // Only two not four!
            Matcher::new().method("GET").path("/v1/related/123").build(),
            Matcher::new().method("GET").path("/v1/related/234").build(),
        ],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "hellos": [
          {
            "id": 1,
            "related": {
              "id": 123,
              "field": "related 1"
            }
          },
          {
            "id": 2,
            "related": {
              "id": 234,
              "field": "related 2"
            }
          },
          {
            "id": 3,
            "related": {
              "id": 234,
              "field": "related 2"
            }
          },
          {
            "id": 4,
            "related": {
              "id": 123,
              "field": "related 1"
            }
          }
        ]
      }
    }
    "###);
}

const SCHEMA: &str = include_str!("./test_supergraph.graphql");

async fn execute(uri: &str, query: &str, config: Option<serde_json::Value>) -> serde_json::Value {
    let connector_uri = format!("{}/v1/", uri);
    let subgraph_uri = format!("{}/graphql", uri);

    // we cannot use Testharness because the subgraph connectors are actually extracted in YamlRouterFactory
    let mut factory = YamlRouterFactory;

    let mut config = config.unwrap_or(serde_json::json!({
      "include_subgraph_errors": { "all": true },
    }));

    let config_object = config.as_object_mut().unwrap();
    config_object.insert(
        "preview_connectors".to_string(),
        serde_json::json!({
          "subgraphs": {
            "kitchen-sink": {
              "a": {
                "override_url": connector_uri
              },
              "with_headers": {
                "override_url": connector_uri
              }
            }
          }
        }),
    );

    config_object.insert(
        "override_subgraph_url".to_string(),
        serde_json::json!({
          "normal": subgraph_uri
        }),
    );

    let router_creator = factory
        .create(
            false,
            Arc::new(serde_json::from_value(config).unwrap()),
            SCHEMA.to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    let service = router_creator.create();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .header("x-client-header", "client-header-value")
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .unwrap();

    serde_json::from_slice(&response).unwrap()
}

mod req_asserts {
    use std::collections::HashMap;
    use std::collections::HashSet;

    use itertools::Itertools;
    use wiremock::http::HeaderName;
    use wiremock::http::HeaderValue;
    use wiremock::http::HeaderValues;

    #[derive(Clone)]
    pub(crate) struct Matcher {
        method: Option<String>,
        path: Option<String>,
        query: Option<String>,
        body: Option<serde_json::Value>,
        headers: HashMap<HeaderName, HeaderValues>,
    }

    impl Matcher {
        pub(crate) fn new() -> Self {
            Self {
                method: None,
                path: None,
                query: None,
                body: None,
                headers: Default::default(),
            }
        }

        pub(crate) fn method(&mut self, method: &str) -> &mut Self {
            self.method = Some(method.to_string());
            self
        }

        pub(crate) fn path(&mut self, path: &str) -> &mut Self {
            self.path = Some(path.to_string());
            self
        }

        pub(crate) fn query(&mut self, query: &str) -> &mut Self {
            self.query = Some(query.to_string());
            self
        }

        pub(crate) fn body(&mut self, body: serde_json::Value) -> &mut Self {
            self.body = Some(body);
            self
        }

        pub(crate) fn header(&mut self, name: HeaderName, value: HeaderValue) -> &mut Self {
            let values = self.headers.entry(name).or_insert(Vec::new().into());
            values.append(&mut Vec::from([value]).into());
            self
        }

        pub(crate) fn build(&mut self) -> Self {
            self.clone()
        }

        fn matches(&self, request: &wiremock::Request, index: usize) {
            if let Some(method) = self.method.as_ref() {
                assert_eq!(
                    method,
                    &request.method.to_string(),
                    "[Request {}]: Expected method {}, got {}",
                    index,
                    method,
                    request.method
                )
            }

            if let Some(path) = self.path.as_ref() {
                assert_eq!(
                    path,
                    request.url.path(),
                    "[Request {}]: Expected path {}, got {}",
                    index,
                    path,
                    request.url.path()
                )
            }

            if let Some(query) = self.query.as_ref() {
                assert_eq!(
                    query,
                    request.url.query().unwrap_or_default(),
                    "[Request {}]: Expected query {}, got {}",
                    index,
                    query,
                    request.url.query().unwrap_or_default()
                )
            }

            if let Some(body) = self.body.as_ref() {
                assert_eq!(
                    body,
                    &request.body_json::<serde_json::Value>().unwrap(),
                    "[Request {}]: incorrect body",
                    index,
                )
            }

            for (name, expected) in self.headers.iter() {
                match request.headers.get(name) {
                    Some(actual) => {
                        let expected: HashSet<String> =
                            expected.iter().map(|v| v.as_str().to_owned()).collect();
                        let actual: HashSet<String> =
                            actual.iter().map(|v| v.as_str().to_owned()).collect();
                        assert_eq!(
                            expected,
                            actual,
                            "[Request {}]: expected header {} to be [{}], was [{}]",
                            index,
                            name,
                            expected.iter().join(", "),
                            actual.iter().join(", ")
                        );
                    }
                    None => {
                        panic!("[Request {}]: expected header {}, was missing", index, name);
                    }
                }
            }
        }
    }

    pub(crate) fn matches(received: &[wiremock::Request], matchers: Vec<Matcher>) {
        assert_eq!(
            received.len(),
            matchers.len(),
            "Expected {} requests, recorded {}",
            matchers.len(),
            received.len()
        );
        for (i, (request, matcher)) in received.iter().zip(matchers.iter()).enumerate() {
            matcher.matches(request, i);
        }
    }
}
