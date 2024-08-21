use std::str::FromStr;
use std::sync::Arc;

use apollo_compiler::execution::JsonMap;
use http::header::CONTENT_TYPE;
use itertools::EitherOrBoth;
use itertools::Itertools;
use mime::APPLICATION_JSON;
use mockall::mock;
use mockall::predicate::eq;
use req_asserts::Matcher;
use serde_json_bytes::json;
use tower::ServiceExt;
use tracing_core::span::Attributes;
use tracing_core::span::Id;
use tracing_core::span::Record;
use tracing_core::Event;
use tracing_core::Metadata;
use wiremock::http::HeaderName;
use wiremock::http::HeaderValue;
use wiremock::matchers::body_json;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

use crate::json_ext::ValueExt;
use crate::plugins::connectors::tracing::CONNECT_SPAN_NAME;
use crate::router_factory::RouterSuperServiceFactory;
use crate::router_factory::YamlRouterFactory;
use crate::services::new_service::ServiceFactory;
use crate::services::router::Request;
use crate::services::supergraph;
use crate::Configuration;

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

    #[allow(dead_code)]
    fn path_template(template: &str) -> PathTemplate {
        PathTemplate(template.to_string())
    }

    use super::*;

    pub(crate) fn users() -> Mock {
        Mock::given(method("GET")).and(path("/users")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([
              {
                "id": 1,
                "name": "Leanne Graham"
              },
              {
                "id": 2,
                "name": "Ervin Howell",
              }
            ])),
        )
    }

    pub(crate) fn user_1() -> Mock {
        Mock::given(method("GET"))
            .and(path("/users/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "id": 1,
              "name": "Leanne Graham",
              "username": "Bret",
              "phone": "1-770-736-8031 x56442",
            })))
    }

    pub(crate) fn user_2() -> Mock {
        Mock::given(method("GET"))
            .and(path("/users/2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "id": 2,
              "name": "Ervin Howell",
              "username": "Antonette",
              "phone": "1-770-736-8031 x56442"
            })))
    }

    pub(crate) fn create_user() -> Mock {
        Mock::given(method("POST")).and(path("/user")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!(
              {
                "id": 3,
                "username": "New User"
              }
            )),
        )
    }

    pub(crate) fn user_1_with_pet() -> Mock {
        Mock::given(method("GET"))
            .and(path("/users/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "id": 1,
              "name": "Leanne Graham",
              "pet": {
                  "name": "Spot"
              }
            })))
    }

    pub(crate) fn commits() -> Mock {
        Mock::given(method("GET"))
            .and(path("/repos/foo/bar/commits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
              [
                {
                  "sha": "abcdef",
                  "commit": {
                    "author": {
                      "name": "Foo Bar",
                      "email": "noone@nowhere",
                      "date": "2024-07-09T01:22:33Z"
                    },
                    "message": "commit message",
                  },
                }]
            )))
    }

    pub(crate) fn posts() -> Mock {
        Mock::given(method("GET")).and(path("/posts")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([
              {
                "id": 1,
                "title": "Post 1",
                "userId": 1
              },
              {
                "id": 2,
                "title": "Post 2",
                "userId": 2
              }
            ])),
        )
    }
}

pub(crate) mod mock_subgraph {
    use super::*;

    pub(crate) fn user_entity_query() -> Mock {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(json!({
              "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{c}}}",
              "variables": {"representations":[{"__typename":"User","id":1},{"__typename":"User","id":2}]}
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .set_body_json(json!({
                      "data": {
                        "_entities": [{
                          "__typename": "User",
                          "c": "1",
                        }, {
                          "__typename": "User",
                          "c": "2",
                        }]
                      }
                    })),
            )
    }
}

#[tokio::test]
async fn value_from_config() {
    let mock_server = MockServer::start().await;
    mock_api::user_1().mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { me { id name username} }",
        Default::default(),
        Some(json!({
            "preview_connectors": {
                "subgraphs": {
                    "connectors": {
                        "$config": {
                            "id": 1,
                        }
                    }
                }
            }
        })),
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "me": {
          "id": 1,
          "name": "Leanne Graham",
          "username": "Bret"
        }
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/users/1").build()],
    );
}

#[tokio::test]
async fn max_requests() {
    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;
    mock_api::user_1().mount(&mock_server).await;
    mock_api::user_2().mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { id name username } }",
        Default::default(),
        Some(json!({
          "preview_connectors": {
            "max_requests_per_operation_per_source": 2
          }
        })),
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
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
            "username": null
          }
        ]
      },
      "errors": [
        {
          "message": "Request limit exceeded",
          "path": [
            "users",
            "@"
          ],
          "extensions": {
            "service": "connectors.json http: GET /users/{$args.id!}",
            "code": "REQUEST_LIMIT_EXCEEDED"
          }
        }
      ]
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users").build(),
            Matcher::new().method("GET").path("/users/1").build(),
        ],
    );
}

#[tokio::test]
async fn source_max_requests() {
    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;
    mock_api::user_1().mount(&mock_server).await;
    mock_api::user_2().mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { id name username } }",
        Default::default(),
        Some(json!({
          "preview_connectors": {
            "subgraphs": {
              "connectors": {
                "sources": {
                  "json": {
                    "max_requests_per_operation": 2,
                  }
                }
              }
            }
          }
        })),
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
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
            "username": null
          }
        ]
      },
      "errors": [
        {
          "message": "Request limit exceeded",
          "path": [
            "users",
            "@"
          ],
          "extensions": {
            "service": "connectors.json http: GET /users/{$args.id!}",
            "code": "REQUEST_LIMIT_EXCEEDED"
          }
        }
      ]
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users").build(),
            Matcher::new().method("GET").path("/users/1").build(),
        ],
    );
}

#[tokio::test]
async fn test_root_field_plus_entity() {
    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;
    mock_api::user_1().mount(&mock_server).await;
    mock_api::user_2().mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { id name username } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
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
          }
        ]
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users").build(),
            Matcher::new().method("GET").path("/users/1").build(),
            Matcher::new().method("GET").path("/users/2").build(),
        ],
    );
}

#[tokio::test]
async fn test_root_field_plus_entity_plus_requires() {
    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;
    mock_api::user_1().mount(&mock_server).await;
    mock_api::user_2().mount(&mock_server).await;
    mock_subgraph::user_entity_query().mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { id name username d } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "users": [
          {
            "id": 1,
            "name": "Leanne Graham",
            "username": "Bret",
            "d": "1-770-736-8031 x56442"
          },
          {
            "id": 2,
            "name": "Ervin Howell",
            "username": "Antonette",
            "d": "1-770-736-8031 x56442"
          }
        ]
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users").build(),
            Matcher::new().method("POST").path("/graphql").build(),
            Matcher::new().method("GET").path("/users/1").build(),
            Matcher::new().method("GET").path("/users/2").build(),
            Matcher::new().method("GET").path("/users/1").build(),
            Matcher::new().method("GET").path("/users/2").build(),
        ],
    );
}

/// Tests that a connector can vend an entity reference like `user: { id: userId }`
#[tokio::test]
async fn test_entity_references() {
    let mock_server = MockServer::start().await;
    mock_api::posts().mount(&mock_server).await;
    mock_api::user_1().mount(&mock_server).await;
    mock_api::user_2().mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { posts { title user { name } } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "posts": [
          {
            "title": "Post 1",
            "user": {
              "name": "Leanne Graham"
            }
          },
          {
            "title": "Post 2",
            "user": {
              "name": "Ervin Howell"
            }
          }
        ]
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/posts").build(),
            Matcher::new().method("GET").path("/users/1").build(),
            Matcher::new().method("GET").path("/users/2").build(),
        ],
    );
}

#[tokio::test]
async fn basic_errors() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
          "error": "not found"
        })))
        .mount(&mock_server)
        .await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "{ users { id } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/users").build()],
    );

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": null,
      "errors": [
        {
          "message": "HTTP fetch failed from 'connectors.json http: GET /users': 404: Not Found",
          "path": [],
          "extensions": {
            "code": "SUBREQUEST_HTTP_ERROR",
            "service": "connectors.json http: GET /users",
            "reason": "404: Not Found",
            "http": {
              "status": 404
            }
          }
        }
      ]
    }
    "###);
}

#[tokio::test]
async fn test_headers() {
    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;

    execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { id } }",
        Default::default(),
        Some(json!({
            "preview_connectors": {
                "subgraphs": {
                    "connectors": {
                        "$config": {
                          "source": {
                            "val": "val-from-config-source"
                          },
                          "connect": {
                            "val": "val-from-config-connect"
                          },
                        }
                    }
                }
            }
        })),
        |request| {
            let headers = request.router_request.headers_mut();
            headers.insert("x-rename-source", "renamed-by-source".parse().unwrap());
            headers.insert("x-rename-connect", "renamed-by-connect".parse().unwrap());
            headers.insert("x-forward", "forwarded".parse().unwrap());
            headers.append("x-forward", "forwarded-again".parse().unwrap());
        },
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new()
            .method("GET")
            .header(
                HeaderName::from_str("x-forward").unwrap(),
                HeaderValue::from_str("forwarded").unwrap(),
            )
            .header(
                HeaderName::from_str("x-forward").unwrap(),
                HeaderValue::from_str("forwarded-again").unwrap(),
            )
            .header(
                HeaderName::from_str("x-new-name").unwrap(),
                HeaderValue::from_str("renamed-by-connect").unwrap(),
            )
            .header(
                HeaderName::from_str("x-insert").unwrap(),
                HeaderValue::from_str("inserted").unwrap(),
            )
            .header(
                HeaderName::from_str("x-insert-multi-value").unwrap(),
                HeaderValue::from_str("first").unwrap(),
            )
            .header(
                HeaderName::from_str("x-insert-multi-value").unwrap(),
                HeaderValue::from_str("second").unwrap(),
            )
            .header(
                HeaderName::from_str("x-config-variable-source").unwrap(),
                HeaderValue::from_str("before val-from-config-source after").unwrap(),
            )
            .header(
                HeaderName::from_str("x-config-variable-connect").unwrap(),
                HeaderValue::from_str("before val-from-config-connect after").unwrap(),
            )
            .path("/users")
            .build()],
    );
}

mock! {
    Subscriber {}
    impl tracing_core::Subscriber for Subscriber {
        fn enabled<'a>(&self, metadata: &Metadata<'a>) -> bool;
        fn new_span<'a>(&self, span: &Attributes<'a>) -> Id;
        fn record<'a>(&self, span: &Id, values: &Record<'a>);
        fn record_follows_from(&self, span: &Id, follows: &Id);
        fn event_enabled<'a>(&self, event: &Event<'a>) -> bool;
        fn event<'a>(&self, event: &Event<'a>);
        fn enter(&self, span: &Id);
        fn exit(&self, span: &Id);
    }
}

#[tokio::test]
async fn test_tracing_connect_span() {
    let mut mock_subscriber = MockSubscriber::new();
    mock_subscriber.expect_event_enabled().returning(|_| false);
    mock_subscriber.expect_record().returning(|_, _| {});
    mock_subscriber
        .expect_enabled()
        .returning(|metadata| metadata.name() == CONNECT_SPAN_NAME);
    mock_subscriber.expect_new_span().returning(|attributes| {
        if attributes.metadata().name() == CONNECT_SPAN_NAME {
            assert!(attributes.fields().field("apollo.connector.type").is_some());
            assert!(attributes
                .fields()
                .field("apollo.connector.detail")
                .is_some());
            assert!(attributes
                .fields()
                .field("apollo.connector.field.name")
                .is_some());
            assert!(attributes
                .fields()
                .field("apollo.connector.selection")
                .is_some());
            assert!(attributes
                .fields()
                .field("apollo.connector.source.name")
                .is_some());
            assert!(attributes
                .fields()
                .field("apollo.connector.source.detail")
                .is_some());
            Id::from_u64(1)
        } else {
            panic!("unexpected span: {}", attributes.metadata().name());
        }
    });
    mock_subscriber
        .expect_enter()
        .with(eq(Id::from_u64(1)))
        .returning(|_| {});
    mock_subscriber
        .expect_exit()
        .with(eq(Id::from_u64(1)))
        .returning(|_| {});
    let _guard = tracing::subscriber::set_default(mock_subscriber);

    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;

    execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { id } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;
}

#[tokio::test]
async fn test_mutation() {
    let mock_server = MockServer::start().await;
    mock_api::create_user().mount(&mock_server).await;

    let response = execute(
        MUTATION_SCHEMA,
        &mock_server.uri(),
        "mutation CreateUser($name: String!) {
            createUser(name: $name) {
                id
                name
            }
        }",
        serde_json_bytes::json!({ "name": "New User" })
            .as_object()
            .unwrap()
            .clone(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "createUser": {
          "id": 3,
          "name": "New User"
        }
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new()
            .method("POST")
            .body(serde_json::json!({ "username": "New User" }))
            .path("/user")
            .build()],
    );
}

#[tokio::test]
async fn test_selection_set() {
    let mock_server = MockServer::start().await;
    mock_api::commits().mount(&mock_server).await;

    let response = execute(
        SELECTION_SCHEMA,
        &mock_server.uri(),
        "query Commits($owner: String!, $repo: String!, $skipInlineFragment: Boolean!,
                             $skipNamedFragment: Boolean!, $skipField: Boolean!) {
              commits(owner: $owner, repo: $repo) {
                __typename
                commit {
                __typename
                  from_path_alias: name_from_path
                  ...CommitDetails @skip(if: $skipNamedFragment)
                }
              }
            }

            fragment CommitDetails on CommitDetail {
              by {
                __typename
                user: name @skip(if: $skipField)
                name
                ...on CommitAuthor @skip(if: $skipInlineFragment) {
                  address: email
                  owner
                }
                owner_not_fragment: owner
              }
            }",
        serde_json_bytes::json!({
        "owner": "foo",
        "repo": "bar",
        "skipField": false,
        "skipInlineFragment": false,
        "skipNamedFragment": false
        })
        .as_object()
        .unwrap()
        .clone(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "commits": [
          {
            "__typename": "Commit",
            "commit": {
              "__typename": "CommitDetail",
              "from_path_alias": "Foo Bar",
              "by": {
                "__typename": "CommitAuthor",
                "user": "Foo Bar",
                "name": "Foo Bar",
                "address": "noone@nowhere",
                "owner": "foo",
                "owner_not_fragment": "foo"
              }
            }
          }
        ]
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new()
            .method("GET")
            .path("/repos/foo/bar/commits")
            .build()],
    );
}

#[tokio::test]
async fn test_nullability() {
    let mock_server = MockServer::start().await;
    mock_api::user_1_with_pet().mount(&mock_server).await;

    let response = execute(
        NULLABILITY_SCHEMA,
        &mock_server.uri(),
        "query { user(id: 1) { id name occupation address { zip } pet { species } } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "user": {
          "id": 1,
          "name": "Leanne Graham",
          "occupation": null,
          "address": null,
          "pet": {
            "species": null
          }
        }
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/users/1").build()],
    );
}

#[tokio::test]
async fn test_no_source() {
    let mock_server = MockServer::start().await;
    mock_api::user_1().mount(&mock_server).await;
    let uri = mock_server.uri();

    let response = execute(
        &NO_SOURCES_SCHEMA.replace("http://localhost", &uri),
        &uri,
        "query { user(id: 1) { id name }}",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "user": {
          "id": 1,
          "name": "Leanne Graham"
        }
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/users/1").build()],
    );
}

mod quickstart_tests {
    use super::*;

    macro_rules! map {
        ($($tt:tt)*) => {
          serde_json_bytes::json!($($tt)*).as_object().unwrap().clone()
        };
    }

    async fn execute(query: &str, variables: JsonMap) -> (serde_json::Value, MockServer) {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/posts")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                  "userId": 1,
                  "id": 1,
                  "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
                  "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto"
                },
                {
                  "userId": 1,
                  "id": 2,
                  "title": "qui est esse",
                  "body": "est rerum tempore vitae\nsequi sint nihil reprehenderit dolor beatae ea dolores neque\nfugiat blanditiis voluptate porro vel nihil molestiae ut reiciendis\nqui aperiam non debitis possimus qui neque nisi nulla"
                }]
            )),
        ).mount(&mock_server).await;
        Mock::given(method("GET")).and(path("/posts/1")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {
                  "userId": 1,
                  "id": 1,
                  "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
                  "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto"
                }
            )),
        ).mount(&mock_server).await;
        Mock::given(method("GET")).and(path("/posts/2")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                  "userId": 1,
                  "id": 2,
                  "title": "qui est esse",
                  "body": "est rerum tempore vitae\nsequi sint nihil reprehenderit dolor beatae ea dolores neque\nfugiat blanditiis voluptate porro vel nihil molestiae ut reiciendis\nqui aperiam non debitis possimus qui neque nisi nulla"
                }
            )),
        ).mount(&mock_server).await;
        Mock::given(method("GET"))
            .and(path("/users/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "id": 1,
              "name": "Leanne Graham",
              "username": "Bret",
              "email": "Sincere@april.biz",
              "address": {
                "street": "Kulas Light",
                "suite": "Apt. 556",
                "city": "Gwenborough",
                "zipcode": "92998-3874",
                "geo": {
                  "lat": "-37.3159",
                  "lng": "81.1496"
                }
              },
              "phone": "1-770-736-8031 x56442",
              "website": "hildegard.org",
              "company": {
                "name": "Romaguera-Crona",
                "catchPhrase": "Multi-layered client-server neural-net",
                "bs": "harness real-time e-markets"
              }
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET")).and(path("/users/1/posts")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                  "userId": 1,
                  "id": 1,
                  "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
                  "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto"
                },
                {
                  "userId": 1,
                  "id": 2,
                  "title": "qui est esse",
                  "body": "est rerum tempore vitae\nsequi sint nihil reprehenderit dolor beatae ea dolores neque\nfugiat blanditiis voluptate porro vel nihil molestiae ut reiciendis\nqui aperiam non debitis possimus qui neque nisi nulla"
                }]
            )),
        ).mount(&mock_server).await;

        let res = super::execute(
            &QUICKSTART_SCHEMA.replace("https://jsonplaceholder.typicode.com", &mock_server.uri()),
            &mock_server.uri(),
            query,
            variables,
            None,
            |_| {},
        )
        .await;

        (res, mock_server)
    }

    #[tokio::test]
    async fn query_1() {
        let query = r#"
          query Posts {
            posts {
              id
              body
              title
            }
          }
        "#;

        let (response, server) = execute(query, Default::default()).await;

        insta::assert_json_snapshot!(response, @r###"
        {
          "data": {
            "posts": [
              {
                "id": 1,
                "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto",
                "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit"
              },
              {
                "id": 2,
                "body": "est rerum tempore vitae\nsequi sint nihil reprehenderit dolor beatae ea dolores neque\nfugiat blanditiis voluptate porro vel nihil molestiae ut reiciendis\nqui aperiam non debitis possimus qui neque nisi nulla",
                "title": "qui est esse"
              }
            ]
          }
        }
        "###);

        req_asserts::matches(
            &server.received_requests().await.unwrap(),
            vec![Matcher::new().method("GET").path("/posts").build()],
        );
    }

    #[tokio::test]
    async fn query_2() {
        let query = r#"
          query Post($postId: ID!) {
            post(id: $postId) {
              id
              title
              body
            }
          }
        "#;

        let (response, server) = execute(query, map!({ "postId": "1" })).await;

        insta::assert_json_snapshot!(response, @r###"
        {
          "data": {
            "post": {
              "id": 1,
              "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
              "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto"
            }
          }
        }
        "###);

        req_asserts::matches(
            &server.received_requests().await.unwrap(),
            vec![Matcher::new().method("GET").path("/posts/1").build()],
        );
    }

    #[tokio::test]
    async fn query_3() {
        let query = r#"
          query PostWithAuthor($postId: ID!) {
            post(id: $postId) {
              id
              title
              body
              author {
                id
                name
              }
            }
          }
      "#;

        let (response, server) = execute(query, map!({ "postId": "1" })).await;

        insta::assert_json_snapshot!(response, @r###"
        {
          "data": {
            "post": {
              "id": 1,
              "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
              "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto",
              "author": {
                "id": 1,
                "name": "Leanne Graham"
              }
            }
          }
        }
        "###);

        req_asserts::matches(
            &server.received_requests().await.unwrap(),
            vec![
                Matcher::new().method("GET").path("/posts/1").build(),
                Matcher::new().method("GET").path("/users/1").build(),
            ],
        );
    }

    #[tokio::test]
    async fn query_4() {
        let query = r#"
          query PostsForUser($userId: ID!) {
            user(id: $userId) {
              id
              name
              posts {
                id
                title
                author {
                  id
                  name
                }
              }
            }
          }
      "#;

        let (response, server) = execute(query, map!({ "userId": "1" })).await;

        insta::assert_json_snapshot!(response, @r###"
        {
          "data": {
            "user": {
              "id": 1,
              "name": "Leanne Graham",
              "posts": [
                {
                  "id": 1,
                  "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
                  "author": {
                    "id": 1,
                    "name": "Leanne Graham"
                  }
                },
                {
                  "id": 2,
                  "title": "qui est esse",
                  "author": {
                    "id": 1,
                    "name": "Leanne Graham"
                  }
                }
              ]
            }
          }
        }
        "###);

        req_asserts::matches(
            &server.received_requests().await.unwrap(),
            vec![
                Matcher::new().method("GET").path("/users/1").build(),
                Matcher::new().method("GET").path("/users/1/posts").build(),
                Matcher::new().method("GET").path("/posts/1").build(),
                Matcher::new().method("GET").path("/posts/2").build(),
                Matcher::new().method("GET").path("/users/1").build(),
            ],
        );
    }
}

const STEEL_THREAD_SCHEMA: &str = include_str!("./testdata/steelthread.graphql");
const MUTATION_SCHEMA: &str = include_str!("./testdata/mutation.graphql");
const NULLABILITY_SCHEMA: &str = include_str!("./testdata/nullability.graphql");
const SELECTION_SCHEMA: &str = include_str!("./testdata/selection.graphql");
const NO_SOURCES_SCHEMA: &str = include_str!("./testdata/connector-without-source.graphql");
const QUICKSTART_SCHEMA: &str = include_str!("./testdata/quickstart.graphql");

async fn execute(
    schema: &str,
    uri: &str,
    query: &str,
    variables: JsonMap,
    config: Option<serde_json_bytes::Value>,
    mut request_mutator: impl FnMut(&mut Request),
) -> serde_json::Value {
    let connector_uri = format!("{}/", uri);
    let subgraph_uri = format!("{}/graphql", uri);

    // we cannot use Testharness because the subgraph connectors are actually extracted in YamlRouterFactory
    let mut factory = YamlRouterFactory;

    let common_config = json!({
        "include_subgraph_errors": { "all": true },
        "override_subgraph_url": {"graphql": subgraph_uri},
        "preview_connectors": {
            "subgraphs": {
                "connectors": {
                    "sources": {
                        "json": {
                            "override_url": connector_uri
                        }
                    }
                }
            }
        }
    });
    let config = if let Some(mut config) = config {
        config.deep_merge(common_config);
        config
    } else {
        common_config
    };
    let config: Configuration = serde_json_bytes::from_value(config).unwrap();

    let router_creator = factory
        .create(
            false,
            Arc::new(config.clone()),
            Arc::new(crate::spec::Schema::parse(schema, &config).unwrap()),
            None,
            None,
        )
        .await
        .unwrap();
    let service = router_creator.create();

    let mut request = supergraph::Request::fake_builder()
        .query(query)
        .variables(variables)
        .header("x-client-header", "client-header-value")
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    request_mutator(&mut request);

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

#[allow(dead_code)]
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
