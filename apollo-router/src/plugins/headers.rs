use std::collections::HashMap;
use std::task::Context;
use std::task::Poll;

use http::header::HeaderName;
use http::header::CONNECTION;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::header::HOST;
use http::header::PROXY_AUTHENTICATE;
use http::header::PROXY_AUTHORIZATION;
use http::header::TE;
use http::header::TRAILER;
use http::header::TRANSFER_ENCODING;
use http::header::UPGRADE;
use http::HeaderValue;
use lazy_static::lazy_static;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::util::BoxService;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_header_value;
use crate::plugin::serde::deserialize_option_header_name;
use crate::plugin::serde::deserialize_option_header_value;
use crate::plugin::serde::deserialize_regex;
use crate::plugin::Plugin;
use crate::register_plugin;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

register_plugin!("apollo", "headers", Headers);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Insert(Insert),
    Remove(Remove),
    Propagate(Propagate),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Remove {
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    Named(HeaderName),

    #[schemars(schema_with = "string_schema")]
    #[serde(deserialize_with = "deserialize_regex")]
    Matching(Regex),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Insert {
    #[schemars(schema_with = "string_schema")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,
    #[schemars(schema_with = "string_schema")]
    #[serde(deserialize_with = "deserialize_header_value")]
    value: HeaderValue,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
enum Propagate {
    Named {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,
        #[schemars(schema_with = "option_string_schema", default)]
        #[serde(deserialize_with = "deserialize_option_header_name", default)]
        rename: Option<HeaderName>,
        #[schemars(schema_with = "option_string_schema", default)]
        #[serde(deserialize_with = "deserialize_option_header_value", default)]
        default: Option<HeaderValue>,
    },
    Matching {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Config {
    #[serde(default)]
    all: Vec<Operation>,
    #[serde(default)]
    subgraphs: HashMap<String, Vec<Operation>>,
}

struct Headers {
    config: Config,
}

fn string_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    String::json_schema(gen)
}

fn option_string_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    Option::<String>::json_schema(gen)
}

#[async_trait::async_trait]
impl Plugin for Headers {
    type Config = Config;

    async fn new(config: Self::Config) -> Result<Self, BoxError> {
        Ok(Headers { config })
    }
    fn subgraph_service(
        &self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        let mut operations = self.config.all.clone();
        if let Some(subgraph_operations) = self.config.subgraphs.get(name) {
            operations.append(&mut subgraph_operations.clone())
        }

        ServiceBuilder::new()
            .layer(HeadersLayer::new(operations))
            .service(service)
            .boxed()
    }
}

struct HeadersLayer {
    operations: Vec<Operation>,
}

impl HeadersLayer {
    fn new(operations: Vec<Operation>) -> Self {
        Self { operations }
    }
}

impl<S> Layer<S> for HeadersLayer {
    type Service = HeadersService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HeadersService {
            inner,
            operations: self.operations.clone(),
        }
    }
}
struct HeadersService<S> {
    inner: S,
    operations: Vec<Operation>,
}

lazy_static! {
    // Headers from https://datatracker.ietf.org/doc/html/rfc2616#section-13.5.1
    // These are not propagated by default using a regex match as they will not make sense for the
    // second hop.
    // In addition because our requests are not regular proxy requests content-type, content-length
    // and host are also in the exclude list.
    static ref RESERVED_HEADERS: Vec<HeaderName> = [
        CONNECTION,
        PROXY_AUTHENTICATE,
        PROXY_AUTHORIZATION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
        CONTENT_LENGTH,
        CONTENT_TYPE,
        HOST,
        HeaderName::from_static("keep-alive")
    ]
    .into();
}

impl<S> Service<SubgraphRequest> for HeadersService<S>
where
    S: Service<SubgraphRequest>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: SubgraphRequest) -> Self::Future {
        for operation in &self.operations {
            match operation {
                Operation::Insert(config) => {
                    req.subgraph_request
                        .headers_mut()
                        .insert(&config.name, config.value.clone());
                }
                Operation::Remove(Remove::Named(name)) => {
                    req.subgraph_request.headers_mut().remove(name);
                }
                Operation::Remove(Remove::Matching(matching)) => {
                    let headers = req.subgraph_request.headers_mut();
                    let matching_headers = headers
                        .iter()
                        .filter_map(|(name, _)| {
                            matching.is_match(name.as_str()).then(|| name.clone())
                        })
                        .filter(|name| !RESERVED_HEADERS.contains(name))
                        .collect::<Vec<_>>();
                    for name in matching_headers {
                        headers.remove(name);
                    }
                }
                Operation::Propagate(Propagate::Named {
                    named,
                    rename,
                    default,
                }) => {
                    let headers = req.subgraph_request.headers_mut();
                    let value = req.originating_request.headers().get(named);
                    if let Some(value) = value.or(default.as_ref()) {
                        headers.insert(rename.as_ref().unwrap_or(named), value.clone());
                    }
                }
                Operation::Propagate(Propagate::Matching { matching }) => {
                    let headers = req.subgraph_request.headers_mut();
                    req.originating_request
                        .headers()
                        .iter()
                        .filter(|(name, _)| matching.is_match(name.as_str()))
                        .filter(|(name, _)| !RESERVED_HEADERS.contains(name))
                        .for_each(|(name, value)| {
                            headers.insert(name, value.clone());
                        });
                }
            }
        }
        self.inner.call(req)
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use std::str::FromStr;
    use std::sync::Arc;

    use tower::BoxError;

    use super::*;
    use crate::graphql::Request;
    use crate::graphql::Response;
    use crate::http_ext;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugins::headers::Config;
    use crate::plugins::headers::HeadersLayer;
    use crate::query_planner::fetch::OperationKind;
    use crate::Context;
    use crate::SubgraphRequest;
    use crate::SubgraphResponse;

    #[test]
    fn test_subgraph_config() {
        serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            - insert:
                name: "test"
                value: "test"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_insert_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            - insert:
                name: "test"
                value: "test"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_remove_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            - remove:
                named: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            - remove:
                matching: "d.*"
        "#,
        )
        .unwrap();

        assert!(serde_yaml::from_str::<Config>(
            r#"
        all:
            - remove:
                matching: "d.*["
        "#,
        )
        .is_err());
    }

    #[test]
    fn test_propagate_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            - propagate:
                named: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            - propagate:
                named: "test"
                rename: "bif"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            - propagate:
                named: "test"
                rename: "bif"
                default: "bof"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            - propagate:
                matching: "d.*"
        "#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_insert() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("c", "d"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Insert(Insert {
            name: "c".try_into()?,
            value: "d".try_into()?,
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_exact() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac"), ("ab", "vab")]))
            .returning(example_response);

        let mut service =
            HeadersLayer::new(vec![Operation::Remove(Remove::Named("aa".try_into()?))])
                .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_matching() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac")]))
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Remove(Remove::Matching(
            Regex::from_str("a[ab]")?,
        ))])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_matching() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("da", "vda"),
                    ("db", "vdb"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Propagate(Propagate::Matching {
            matching: Regex::from_str("d[ab]")?,
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("da", "vda"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Propagate(Propagate::Named {
            named: "da".try_into()?,
            rename: None,
            default: None,
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact_rename() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("ea", "vda"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Propagate(Propagate::Named {
            named: "da".try_into()?,
            rename: Some("ea".try_into()?),
            default: None,
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact_default() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("ea", "defaulted"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Propagate(Propagate::Named {
            named: "ea".try_into()?,
            rename: None,
            default: Some("defaulted".try_into()?),
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    fn example_response(_: SubgraphRequest) -> Result<SubgraphResponse, BoxError> {
        Ok(SubgraphResponse::new_from_response(
            http::Response::builder()
                .body(Response::builder().build())
                .unwrap()
                .into(),
            Context::new(),
        ))
    }

    fn example_request() -> SubgraphRequest {
        SubgraphRequest {
            originating_request: Arc::new(
                http_ext::Request::fake_builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(Request::builder().query("query").build())
                    .build()
                    .expect("expecting valid request"),
            ),
            subgraph_request: http_ext::Request::fake_builder()
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .build()
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        }
    }

    impl SubgraphRequest {
        pub fn assert_headers(&self, headers: Vec<(&'static str, &'static str)>) -> bool {
            let mut headers = headers.clone();
            headers.push((HOST.as_str(), "rhost"));
            headers.push((CONTENT_LENGTH.as_str(), "22"));
            headers.push((CONTENT_TYPE.as_str(), "graphql"));
            let actual_headers = self
                .subgraph_request
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
                .collect::<HashSet<_>>();
            assert_eq!(actual_headers, headers.into_iter().collect::<HashSet<_>>());

            true
        }
    }
}
