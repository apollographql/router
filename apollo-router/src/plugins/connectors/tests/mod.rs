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
use crate::metrics::FutureMetricsExt;
use crate::plugins::telemetry::consts::CONNECT_SPAN_NAME;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::router_factory::RouterSuperServiceFactory;
use crate::router_factory::YamlRouterFactory;
use crate::services::new_service::ServiceFactory;
use crate::services::router::Request;
use crate::services::supergraph;
use crate::Configuration;

mod mock_api;
mod quickstart;
#[allow(dead_code)]
mod req_asserts;

const STEEL_THREAD_SCHEMA: &str = include_str!("../testdata/steelthread.graphql");
const MUTATION_SCHEMA: &str = include_str!("../testdata/mutation.graphql");
const NULLABILITY_SCHEMA: &str = include_str!("../testdata/nullability.graphql");
const SELECTION_SCHEMA: &str = include_str!("../testdata/selection.graphql");
const NO_SOURCES_SCHEMA: &str = include_str!("../testdata/connector-without-source.graphql");
const QUICKSTART_SCHEMA: &str = include_str!("../testdata/quickstart.graphql");
const INTERFACE_OBJECT_SCHEMA: &str = include_str!("../testdata/interface-object.graphql");
const VARIABLES_SCHEMA: &str = include_str!("../testdata/variables.graphql");

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
        vec![Matcher::new().method("GET").path("/users/1")],
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
            1
          ],
          "extensions": {
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.user@connect[0]"
            },
            "code": "REQUEST_LIMIT_EXCEEDED"
          }
        }
      ]
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users"),
            Matcher::new().method("GET").path("/users/1"),
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
            1
          ],
          "extensions": {
            "service": "connectors",
            "connector": {
              "coordinate": "connectors:Query.user@connect[0]"
            },
            "code": "REQUEST_LIMIT_EXCEEDED"
          }
        }
      ]
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users"),
            Matcher::new().method("GET").path("/users/1"),
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
        "query { users { __typename id name username } }",
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
            "__typename": "User",
            "id": 1,
            "name": "Leanne Graham",
            "username": "Bret"
          },
          {
            "__typename": "User",
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
            Matcher::new().method("GET").path("/users"),
            Matcher::new().method("GET").path("/users/1"),
            Matcher::new().method("GET").path("/users/2"),
        ],
    );
}

#[tokio::test]
async fn test_root_field_plus_entity_plus_requires() {
    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;
    mock_api::user_1().mount(&mock_server).await;
    mock_api::user_2().mount(&mock_server).await;
    Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(json!({
              "query": "query($representations: [_Any!]!) { _entities(representations: $representations) { ... on User { c } } }",
              "variables": {"representations":[{"__typename":"User","id":1},{"__typename":"User","id":2}]}
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(wiremock::http::HeaderName::from_string(CONTENT_TYPE.to_string()).unwrap(), APPLICATION_JSON.essence_str())
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
            ).mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { __typename id name username d } }",
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
            "__typename": "User",
            "id": 1,
            "name": "Leanne Graham",
            "username": "Bret",
            "d": "1-770-736-8031 x56442"
          },
          {
            "__typename": "User",
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
            Matcher::new().method("GET").path("/users"),
            Matcher::new().method("GET").path("/users/1"),
            Matcher::new().method("GET").path("/users/2"),
            Matcher::new().method("POST").path("/graphql"),
            Matcher::new().method("GET").path("/users/1"),
            Matcher::new().method("GET").path("/users/2"),
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
            Matcher::new().method("GET").path("/posts"),
            Matcher::new().method("GET").path("/users/1"),
            Matcher::new().method("GET").path("/users/2"),
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
    Mock::given(method("GET"))
        .and(path("/posts"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([{ "id": "1", "userId": "1" }])),
        )
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "bad"})))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/1/nicknames"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "bad"})))
        .mount(&mock_server)
        .await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "{ users { id } posts { id user { name nickname } } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "users": null,
        "posts": [
          {
            "id": "1",
            "user": {
              "name": null,
              "nickname": null
            }
          }
        ]
      },
      "errors": [
        {
          "message": "Request failed",
          "path": [
            "users"
          ],
          "extensions": {
            "service": "connectors",
            "http": {
              "status": 404
            },
            "connector": {
              "coordinate": "connectors:Query.users@connect[0]"
            },
            "code": "CONNECTOR_FETCH"
          }
        },
        {
          "message": "Request failed",
          "path": [
            "posts",
            0,
            "user"
          ],
          "extensions": {
            "service": "connectors",
            "http": {
              "status": 400
            },
            "connector": {
              "coordinate": "connectors:Query.user@connect[0]"
            },
            "code": "CONNECTOR_FETCH"
          }
        },
        {
          "message": "Request failed",
          "path": [
            "posts",
            0,
            "user",
            "nickname"
          ],
          "extensions": {
            "service": "connectors",
            "http": {
              "status": 400
            },
            "connector": {
              "coordinate": "connectors:User.nickname@connect[0]"
            },
            "code": "CONNECTOR_FETCH"
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
            request
                .context
                .insert("val", String::from("val-from-request-context"))
                .unwrap();
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
            .header(
                HeaderName::from_str("x-context-value-source").unwrap(),
                HeaderValue::from_str("before val-from-request-context after").unwrap(),
            )
            .header(
                HeaderName::from_str("x-context-value-connect").unwrap(),
                HeaderValue::from_str("before val-from-request-context after").unwrap(),
            )
            .path("/users")],
    );
}

#[tokio::test]
async fn test_args_and_this_in_header() {
    let mock_server = MockServer::start().await;
    mock_api::user_2().mount(&mock_server).await;
    mock_api::user_2_nicknames().mount(&mock_server).await;

    execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { user(id: 2){ id nickname } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new()
                .method("GET")
                .header(
                    HeaderName::from_str("x-from-args").unwrap(),
                    HeaderValue::from_str("before 2 after").unwrap(),
                )
                .path("/users/2"),
            Matcher::new()
                .method("GET")
                .header(
                    HeaderName::from_str("x-from-this").unwrap(),
                    HeaderValue::from_str("before 2 after").unwrap(),
                )
                .path("/users/2/nicknames"),
        ],
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
            assert!(attributes.fields().field(OTEL_STATUS_CODE).is_some());
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
async fn test_operation_counter() {
    async {
        let mock_server = MockServer::start().await;
        mock_api::users().mount(&mock_server).await;
        execute(
            STEEL_THREAD_SCHEMA,
            &mock_server.uri(),
            "query { users { id name username } }",
            Default::default(),
            None,
            |_| {},
        )
        .await;
        req_asserts::matches(
            &mock_server.received_requests().await.unwrap(),
            vec![
                Matcher::new().method("GET").path("/users"),
                Matcher::new().method("GET").path("/users/1"),
                Matcher::new().method("GET").path("/users/2"),
            ],
        );
        assert_counter!(
            "apollo.router.operations.connectors",
            3,
            connector.type = "http",
            subgraph.name = "connectors"
        );
    }
    .with_metrics()
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
            .path("/user")],
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
        vec![Matcher::new().method("GET").path("/repos/foo/bar/commits")],
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
        vec![Matcher::new().method("GET").path("/users/1")],
    );
}

#[tokio::test]
async fn test_default_argument_values() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/default-args"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!("hello")))
        .mount(&mock_server)
        .await;

    let response = execute(
        NULLABILITY_SCHEMA,
        &mock_server.uri(),
        "query { defaultArgs }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "defaultArgs": "hello"
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new()
            .method("POST")
            .path("/default-args")
            .body(serde_json::json!({
              "str": "default",
              "int": 42,
              "float": 1.23,
              "bool": true,
              "arr": ["default"],
            }))],
    );
}

#[tokio::test]
async fn test_default_argument_overrides() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/default-args"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!("hello")))
        .mount(&mock_server)
        .await;

    let response = execute(
        NULLABILITY_SCHEMA,
        &mock_server.uri(),
        "query { defaultArgs(str: \"hi\" int: 108 float: 9.87 bool: false arr: [\"hi again\"]) }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "defaultArgs": "hello"
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new()
            .method("POST")
            .path("/default-args")
            .body(serde_json::json!({
              "str": "hi",
              "int": 108,
              "float": 9.87,
              "bool": false,
              "arr": ["hi again"],
            }))],
    );
}

#[tokio::test]
async fn test_form_encoding() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/posts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 1 })))
        .mount(&mock_server)
        .await;
    let uri = mock_server.uri();

    let response = execute(
        include_str!("../testdata/form-encoding.graphql"),
        &uri,
        "mutation {
          post(
            input: {
              int: 1
              str: \"s\"
              bool: true
              id: \"id\"

              intArr: [1, 2]
              strArr: [\"a\", \"b\"]
              boolArr: [true, false]
              idArr: [\"id1\", \"id2\"]

              obj: {
                a: 1
                b: \"b\"
                c: true
                nested: {
                    d: 1
                    e: \"e\"
                    f: true
                  }
              }
              objArr: [
                {
                  a: 1
                  b: \"b\"
                  c: true
                  nested: {
                    d: 1
                    e: \"e\"
                    f: true
                  }
                },
                {
                  a: 2
                  b: \"bb\"
                  c: false
                  nested: {
                    d: 1
                    e: \"e\"
                    f: true
                  }
                }
              ]
            }
          )
          { id }
        }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "post": {
          "id": 1
        }
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("POST").path("/posts")],
    );

    let reqs = mock_server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&reqs[0].body).to_string();
    assert_eq!(body, "int=1&str=s&bool=true&id=id&intArr%5B0%5D=1&intArr%5B1%5D=2&strArr%5B0%5D=a&strArr%5B1%5D=b&boolArr%5B0%5D=true&boolArr%5B1%5D=false&idArr%5B0%5D=id1&idArr%5B1%5D=id2&obj%5Ba%5D=1&obj%5Bb%5D=b&obj%5Bc%5D=true&obj%5Bnested%5D%5Bd%5D=1&obj%5Bnested%5D%5Be%5D=e&obj%5Bnested%5D%5Bf%5D=true&objArr%5B0%5D%5Ba%5D=1&objArr%5B0%5D%5Bb%5D=b&objArr%5B0%5D%5Bc%5D=true&objArr%5B0%5D%5Bnested%5D%5Bd%5D=1&objArr%5B0%5D%5Bnested%5D%5Be%5D=e&objArr%5B0%5D%5Bnested%5D%5Bf%5D=true&objArr%5B1%5D%5Ba%5D=2&objArr%5B1%5D%5Bb%5D=bb&objArr%5B1%5D%5Bc%5D=false&objArr%5B1%5D%5Bnested%5D%5Bd%5D=1&objArr%5B1%5D%5Bnested%5D%5Be%5D=e&objArr%5B1%5D%5Bnested%5D%5Bf%5D=true");
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
        vec![Matcher::new().method("GET").path("/users/1")],
    );
}

#[tokio::test]
async fn error_not_redacted() {
    let mock_server = MockServer::start().await;
    mock_api::users_error().mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { id name username } }",
        Default::default(),
        Some(json!({
            "include_subgraph_errors": {
                "subgraphs": {
                    "connectors": true
                }
            }
        })),
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "users": null
      },
      "errors": [
        {
          "message": "Request failed",
          "path": [
            "users"
          ],
          "extensions": {
            "service": "connectors",
            "http": {
              "status": 404
            },
            "connector": {
              "coordinate": "connectors:Query.users@connect[0]"
            },
            "code": "CONNECTOR_FETCH"
          }
        }
      ]
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/users")],
    );
}

#[tokio::test]
async fn error_redacted() {
    let mock_server = MockServer::start().await;
    mock_api::users_error().mount(&mock_server).await;

    let response = execute(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { users { id name username } }",
        Default::default(),
        Some(json!({
            "include_subgraph_errors": {
                "subgraphs": {
                    "connectors": false
                }
            }
        })),
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "users": null
      },
      "errors": [
        {
          "message": "Subgraph errors redacted",
          "path": [
            "users"
          ]
        }
      ]
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/users")],
    );
}

#[tokio::test]
async fn test_interface_object() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/itfs"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([{ "id": 1, "c": 10 }, { "id": 2, "c": 11 }])),
        )
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/itfs/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "id": 1, "c": 10, "d": 20 })),
        )
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/itfs/2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "id": 1, "c": 11, "d": 21 })),
        )
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/itfs/1/e"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!("e1")))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/itfs/2/e"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!("e2")))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "data": {
            "_entities": [{
              "__typename": "T1",
              "a": "a"
            }, {
              "__typename": "T2",
              "b": "b"
            }]
          }
        })))
        .mount(&mock_server)
        .await;

    let response = execute(
        INTERFACE_OBJECT_SCHEMA,
        &mock_server.uri(),
        "query { itfs { __typename id c d e ... on T1 { a } ... on T2 { b } } }",
        Default::default(),
        None,
        |_| {},
    )
    .await;

    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "itfs": [
          {
            "__typename": "T1",
            "id": 1,
            "c": 10,
            "d": 20,
            "e": "e1",
            "a": "a"
          },
          {
            "__typename": "T2",
            "id": 2,
            "c": 11,
            "d": 21,
            "e": "e2",
            "b": "b"
          }
        ]
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
          Matcher::new().method("GET").path("/itfs"),
          Matcher::new().method("GET").path("/itfs/1/e"),
          Matcher::new().method("GET").path("/itfs/2/e"),
          Matcher::new().method("GET").path("/itfs/1"),
          Matcher::new().method("GET").path("/itfs/2"),
          Matcher::new()
            .method("POST")
            .path("/graphql")
            .body(serde_json::json!({
              "query": r#"query($representations: [_Any!]!) { _entities(representations: $representations) { ..._generated_onItf3_0 } } fragment _generated_onItf3_0 on Itf { __typename ... on T1 { a } ... on T2 { b } }"#,
              "variables": {
                "representations": [
                  { "__typename": "Itf", "id": 1 },
                  { "__typename": "Itf", "id": 2 }
                ]
              }
            })),
        ],
    );
}

#[tokio::test]
async fn test_sources_in_context() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/coprocessor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "control": "continue",
          "version": 1,
          "stage": "ExecutionRequest"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/posts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "userId": 1, "id": 1, "title": "title", "body": "body" },
            { "userId": 1, "id": 2, "title": "title", "body": "body" }]
        )))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "id": 1,
          "name": "Leanne Graham",
          "username": "Bret"
        })))
        .mount(&mock_server)
        .await;
    let uri = mock_server.uri();

    let _ = execute(
        &QUICKSTART_SCHEMA.replace("https://jsonplaceholder.typicode.com", &mock_server.uri()),
        &uri,
        "query Posts { posts { id body title author { name username } } }",
        Default::default(),
        Some(json!({
          "preview_connectors": {
            "expose_sources_in_context": true
          },
          "coprocessor": {
            "url": format!("{}/coprocessor", mock_server.uri()),
            "execution": {
              "request": {
                "context": true
              }
            }
          }
        })),
        |_| {},
    )
    .await;

    let requests = &mock_server.received_requests().await.unwrap();
    let coprocessor_request = requests.first().unwrap();
    let body = coprocessor_request
        .body_json::<serde_json_bytes::Value>()
        .unwrap();
    pretty_assertions::assert_eq!(
        body.get("context")
            .unwrap()
            .as_object()
            .unwrap()
            .get("entries")
            .unwrap()
            .as_object()
            .unwrap()
            .get("apollo_connectors::sources_in_query_plan")
            .unwrap(),
        &serde_json_bytes::json!([
          { "subgraph_name": "connectors", "source_name": "jsonPlaceholder" }
        ])
    );
}

#[tokio::test]
async fn test_variables() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/coprocessor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "control": "continue",
          "version": 1,
          "stage": "SupergraphRequest",
          "context": {
            "entries": {
              "value": "B"
            }
          }
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/f"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&mock_server)
        .await;
    let uri = mock_server.uri();

    let response = execute(
        &VARIABLES_SCHEMA.replace("http://localhost:4001/", &mock_server.uri()),
        &uri,
        "{ f(arg: \"A\") { arg context config sibling status extra f(arg: \"A\") { arg context config sibling status } } }",
        Default::default(),
        Some(json!({
          "preview_connectors": {
            "subgraphs": {
              "connectors": {
                "$config": {
                  "value": "C"
                }
              }
            }
          },
          "coprocessor": {
            "url": format!("{}/coprocessor", mock_server.uri()),
            "supergraph": {
              "request": {
                "context": true
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
        "f": {
          "arg": "A",
          "context": "B",
          "config": "C",
          "sibling": "D",
          "status": 200,
          "extra": {
            "arg": "A",
            "context": "B",
            "config": "C",
            "status": 200
          },
          "f": {
            "arg": "A",
            "context": "B",
            "config": "C",
            "sibling": "D",
            "status": 200
          }
        }
      }
    }
    "###);

    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("POST").path("/coprocessor"),
            Matcher::new()
                .method("POST")
                .path("/f")
                .query("arg=A&context=B&config=C")
                .header("x-source-context".into(), "B".try_into().unwrap())
                .header("x-source-config".into(), "C".try_into().unwrap())
                .header("x-connect-arg".into(), "A".try_into().unwrap())
                .header("x-connect-context".into(), "B".try_into().unwrap())
                .header("x-connect-config".into(), "C".try_into().unwrap())
                .body(serde_json::json!({ "arg": "A", "context": "B", "config": "C" }))
                ,
            Matcher::new()
                .method("POST")
                .path("/f")
                .query("arg=A&context=B&config=C&sibling=D")
                .header("x-source-context".into(), "B".try_into().unwrap())
                .header("x-source-config".into(), "C".try_into().unwrap())
                .header("x-connect-arg".into(), "A".try_into().unwrap())
                .header("x-connect-context".into(), "B".try_into().unwrap())
                .header("x-connect-config".into(), "C".try_into().unwrap())
                .header("x-connect-sibling".into(), "D".try_into().unwrap())
                .body(serde_json::json!({ "arg": "A", "context": "B", "config": "C", "sibling": "D" }))
                ,
        ],
    );
}

mod quickstart_tests {
    use http::Uri;

    use super::*;
    use crate::test_harness::http_snapshot::SnapshotServer;

    const SNAPSHOT_DIR: &str = "./src/plugins/connectors/testdata/quickstart_api_snapshots/";

    macro_rules! map {
        ($($tt:tt)*) => {
          serde_json_bytes::json!($($tt)*).as_object().unwrap().clone()
        };
    }

    async fn execute(
        query: &str,
        variables: JsonMap,
        snapshot_file_name: &str,
    ) -> serde_json::Value {
        let snapshot_path = [SNAPSHOT_DIR, snapshot_file_name, ".json"].concat();

        let server = SnapshotServer::spawn(
            snapshot_path,
            Uri::from_str("https://jsonPlaceholder.typicode.com/").unwrap(),
            true,
            false,
            Some(vec![CONTENT_TYPE.to_string()]),
        )
        .await;

        super::execute(
            &QUICKSTART_SCHEMA.replace("https://jsonplaceholder.typicode.com", &server.uri()),
            &server.uri(),
            query,
            variables,
            None,
            |_| {},
        )
        .await
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

        let response = execute(query, Default::default(), "query_1").await;

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

        let response = execute(query, map!({ "postId": "1" }), "query_2").await;

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

        let response = execute(query, map!({ "postId": "1" }), "query_3").await;

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

        let response = execute(query, map!({ "userId": "1" }), "query_4").await;

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
    }
}

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
