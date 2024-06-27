use std::sync::Arc;

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
              "d": "1-770-736-8031 x56442"
            })))
    }
}

pub(crate) mod mock_subgraph {
    use super::*;

    pub(crate) fn user_entity_query() -> Mock {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(serde_json::json!({
              "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{c}}}",
              "variables": {"representations":[{"__typename":"User"}]}
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .set_body_json(serde_json::json!({
                      "data": {
                        "_entities": [{
                          "__typename": "User",
                          "c": "1",
                        }]
                      }
                    })),
            )
    }
}

#[tokio::test]
async fn test_root_field() {
    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;
    mock_api::user_1().mount(&mock_server).await;
    mock_api::user_2().mount(&mock_server).await;

    let response = execute(
        &mock_server.uri(),
        "query { users { id name username } }",
        None,
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
#[ignore] // TODO remove after the tests are wired up
async fn test_root_field_plus_entity_plus_requires() {
    let mock_server = MockServer::start().await;
    mock_api::users().mount(&mock_server).await;
    mock_api::user_1().mount(&mock_server).await;
    mock_api::user_2().mount(&mock_server).await;
    mock_subgraph::user_entity_query().mount(&mock_server).await;

    let response = execute(
        &mock_server.uri(),
        "query { users { id name username d } }",
        None,
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
            Matcher::new().method("GET").path("/users/1").build(),
            Matcher::new().method("GET").path("/users/2").build(),
            Matcher::new().method("POST").path("/graphql").build(),
            Matcher::new().method("GET").path("/users/1").build(),
        ],
    );
}

#[tokio::test]
#[ignore] // TODO remove after the tests are wired up
async fn basic_errors() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
          "error": "not found"
        })))
        .mount(&mock_server)
        .await;

    let response = execute(&mock_server.uri(), "{ users { id } }", None).await;

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
            "connector": "connectors.json http: Get /users",
            "code": "404"
          }
        }
      ]
    }
    "###);
}

const SCHEMA: &str = include_str!("./testdata/steelthread.graphql");

async fn execute(uri: &str, query: &str, config: Option<serde_json::Value>) -> serde_json::Value {
    let connector_uri = format!("{}/", uri);
    let subgraph_uri = format!("{}/graphql", uri);

    // we cannot use Testharness because the subgraph connectors are actually extracted in YamlRouterFactory
    let mut factory = YamlRouterFactory;

    let mut config = config.unwrap_or(serde_json::json!({
      "include_subgraph_errors": { "all": true },
    }));

    // TODO: implement override_url handling
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
            SCHEMA
                .to_string()
                .replace("https://jsonplaceholder.typicode.com/", uri),
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
