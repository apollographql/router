use std::str::FromStr;
use std::sync::Arc;

use apollo_compiler::execution::JsonMap;
use http::header::CONTENT_TYPE;
use itertools::EitherOrBoth;
use itertools::Itertools;
use mime::APPLICATION_JSON;
use req_asserts::Matcher;
use tower::ServiceExt;
use tracing_fluent_assertions::AssertionRegistry;
use tracing_fluent_assertions::AssertionsLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;
use wiremock::http::HeaderName;
use wiremock::http::HeaderValue;
use wiremock::matchers::body_json;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

use crate::plugins::connectors::tracing::CONNECT_SPAN_NAME;
use crate::router_factory::RouterSuperServiceFactory;
use crate::router_factory::YamlRouterFactory;
use crate::services::new_service::ServiceFactory;
use crate::services::router::Request;
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
}

pub(crate) mod mock_subgraph {
    use super::*;

    pub(crate) fn user_entity_query() -> Mock {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(serde_json::json!({
              "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{c}}}",
              "variables": {"representations":[{"__typename":"User","id":1},{"__typename":"User","id":2}]}
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .set_body_json(serde_json::json!({
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
          "message": "http error: 404 Not Found",
          "extensions": {
            "connector": "connectors.json http: GET /users",
            "code": "404"
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
        None,
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
            .path("/users")
            .build()],
    );
}

#[tokio::test]
async fn test_tracing_connect_span() {
    let assertion_registry = AssertionRegistry::default();
    let base_subscriber = Registry::default();
    let subscriber = base_subscriber.with(AssertionsLayer::new(&assertion_registry));
    let _guard = tracing::subscriber::set_default(subscriber);

    let found_connector_span = assertion_registry
        .build()
        .with_name(CONNECT_SPAN_NAME)
        .with_span_field("apollo.connector.type")
        .with_span_field("apollo.connector.detail")
        .with_span_field("apollo.connector.field.name")
        .with_span_field("apollo.connector.selection")
        .with_span_field("apollo.connector.source.name")
        .with_span_field("apollo.connector.source.detail")
        .was_entered()
        .was_exited()
        .finalize();

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

    found_connector_span.assert();
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
                commit {
                  from_path_alias: name_from_path
                  ...CommitDetails @skip(if: $skipNamedFragment)
                }
              }
            }

            fragment CommitDetails on CommitDetail {
              by {
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
            "commit": {
              "from_path_alias": "Foo Bar",
              "by": {
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

const STEEL_THREAD_SCHEMA: &str = include_str!("./testdata/steelthread.graphql");
const MUTATION_SCHEMA: &str = include_str!("./testdata/mutation.graphql");
const NULLABILITY_SCHEMA: &str = include_str!("./testdata/nullability.graphql");
const SELECTION_SCHEMA: &str = include_str!("./testdata/selection.graphql");
const NO_SOURCES_SCHEMA: &str = include_str!("./testdata/connector-without-source.graphql");

async fn execute(
    schema: &str,
    uri: &str,
    query: &str,
    variables: JsonMap,
    config: Option<serde_json::Value>,
    mut request_mutator: impl FnMut(&mut Request),
) -> serde_json::Value {
    let connector_uri = format!("{}/", uri);
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
            "connectors": {
              "json": {
                "override_url": connector_uri
              }
            }
          }
        }),
    );

    config_object.insert(
        "override_subgraph_url".to_string(),
        serde_json::json!({
          "graphql": subgraph_uri
        }),
    );

    let router_creator = factory
        .create(
            false,
            Arc::new(serde_json::from_value(config).unwrap()),
            schema.to_string(),
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
