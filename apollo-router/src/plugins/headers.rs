use std::collections::HashMap;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use access_json::JSONQuery;
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
use serde_json::Value;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_header_value;
use crate::plugin::serde::deserialize_json_query;
use crate::plugin::serde::deserialize_option_header_name;
use crate::plugin::serde::deserialize_option_header_value;
use crate::plugin::serde::deserialize_regex;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::services::SubgraphRequest;

register_plugin!("apollo", "headers", Headers);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HeadersLocation {
    /// Propagate/Insert/Remove headers from request
    request: Vec<Operation>,
    // Propagate/Insert/Remove headers from response
    // response: Option<Operation>
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Insert(Insert),
    Remove(Remove),
    Propagate(Propagate),
}

schemar_fn!(remove_named, String, "Remove a header given a header name");
schemar_fn!(
    remove_matching,
    String,
    "Remove a header given a regex matching against the header name"
);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Remove header
enum Remove {
    #[schemars(schema_with = "remove_named")]
    #[serde(deserialize_with = "deserialize_header_name")]
    /// Remove a header given a header name
    Named(HeaderName),

    #[schemars(schema_with = "remove_matching")]
    #[serde(deserialize_with = "deserialize_regex")]
    /// Remove a header given a regex matching header name
    Matching(Regex),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Insert header
enum Insert {
    /// Insert static header
    Static(InsertStatic),
    /// Insert header with a value coming from context key (works only for a string in the context)
    FromContext(InsertFromContext),
    /// Insert header with a value coming from body
    FromBody(InsertFromBody),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert static header
struct InsertStatic {
    /// The name of the header
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,

    /// The value for the header
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_value")]
    value: HeaderValue,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert header with a value coming from context key
struct InsertFromContext {
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    /// Specify header name
    name: HeaderName,
    /// Specify context key to fetch value
    from_context: String,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert header with a value coming from body
struct InsertFromBody {
    /// The target header name
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,

    /// The path in the request body
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_json_query")]
    path: JSONQuery,

    /// The default if the path in the body did not resolve to an element
    #[schemars(with = "Option<String>", default)]
    #[serde(deserialize_with = "deserialize_option_header_value")]
    default: Option<HeaderValue>,
}

schemar_fn!(
    propagate_matching,
    String,
    "Remove a header given a regex matching header name"
);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Propagate header
enum Propagate {
    /// Propagate header given a header name
    Named {
        /// The source header name
        #[schemars(with = "String")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,

        /// An optional target header name
        #[schemars(with = "Option<String>", default)]
        #[serde(deserialize_with = "deserialize_option_header_name", default)]
        rename: Option<HeaderName>,

        /// Default value for the header.
        #[schemars(with = "Option<String>", default)]
        #[serde(deserialize_with = "deserialize_option_header_value", default)]
        default: Option<HeaderValue>,
    },
    /// Propagate header given a regex to match header name
    Matching {
        /// The regex on header name
        #[schemars(schema_with = "propagate_matching")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },
}

/// Configuration for header propagation
#[derive(Clone, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct Config {
    /// Rules to apply to all subgraphs
    all: Option<HeadersLocation>,
    /// Rules to specific subgraphs
    subgraphs: HashMap<String, HeadersLocation>,
}

struct Headers {
    all_operations: Arc<Vec<Operation>>,
    subgraph_operations: HashMap<String, Arc<Vec<Operation>>>,
}

#[async_trait::async_trait]
impl Plugin for Headers {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let operations: Vec<Operation> = init
            .config
            .all
            .as_ref()
            .map(|a| a.request.clone())
            .unwrap_or_default();
        let subgraph_operations = init
            .config
            .subgraphs
            .iter()
            .map(|(subgraph_name, op)| {
                let mut operations = operations.clone();
                operations.append(&mut op.request.clone());
                (subgraph_name.clone(), Arc::new(operations))
            })
            .collect();

        Ok(Headers {
            all_operations: Arc::new(operations),
            subgraph_operations,
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        ServiceBuilder::new()
            .layer(HeadersLayer::new(
                self.subgraph_operations
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| self.all_operations.clone()),
            ))
            .service(service)
            .boxed()
    }
}

struct HeadersLayer {
    operations: Arc<Vec<Operation>>,
}

impl HeadersLayer {
    fn new(operations: Arc<Vec<Operation>>) -> Self {
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
    operations: Arc<Vec<Operation>>,
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
        for operation in &*self.operations {
            match operation {
                Operation::Insert(insert_config) => match insert_config {
                    Insert::Static(static_insert) => {
                        req.subgraph_request
                            .headers_mut()
                            .insert(&static_insert.name, static_insert.value.clone());
                    }
                    Insert::FromContext(insert_from_context) => {
                        if let Some(val) = req
                            .context
                            .get::<_, String>(&insert_from_context.from_context)
                            .ok()
                            .flatten()
                        {
                            match HeaderValue::from_str(&val) {
                                Ok(header_value) => {
                                    req.subgraph_request
                                        .headers_mut()
                                        .insert(&insert_from_context.name, header_value);
                                }
                                Err(err) => {
                                    tracing::error!("cannot convert from the context into a header value for header name '{}': {:?}", insert_from_context.name, err);
                                }
                            }
                        }
                    }
                    Insert::FromBody(from_body) => {
                        let output = from_body
                            .path
                            .execute(req.supergraph_request.body())
                            .ok()
                            .flatten();
                        if let Some(val) = output {
                            let header_value = if let Value::String(val_str) = val {
                                val_str
                            } else {
                                val.to_string()
                            };
                            match HeaderValue::from_str(&header_value) {
                                Ok(header_value) => {
                                    req.subgraph_request
                                        .headers_mut()
                                        .insert(&from_body.name, header_value);
                                }
                                Err(err) => {
                                    tracing::error!("cannot convert from the body into a header value for header name '{}': {:?}", from_body.name, err);
                                }
                            }
                        } else if let Some(default_val) = &from_body.default {
                            req.subgraph_request
                                .headers_mut()
                                .insert(&from_body.name, default_val.clone());
                        }
                    }
                },
                Operation::Remove(Remove::Named(name)) => {
                    req.subgraph_request.headers_mut().remove(name);
                }
                Operation::Remove(Remove::Matching(matching)) => {
                    let headers = req.subgraph_request.headers_mut();
                    let new_headers = headers
                        .drain()
                        .filter_map(|(name, value)| {
                            name.and_then(|name| {
                                (RESERVED_HEADERS.contains(&name)
                                    || !matching.is_match(name.as_str()))
                                .then_some((name, value))
                            })
                        })
                        .collect();

                    let _ = std::mem::replace(headers, new_headers);
                }
                Operation::Propagate(Propagate::Named {
                    named,
                    rename,
                    default,
                }) => {
                    let headers = req.subgraph_request.headers_mut();
                    let value = req.supergraph_request.headers().get(named);
                    if let Some(value) = value.or(default.as_ref()) {
                        headers.insert(rename.as_ref().unwrap_or(named), value.clone());
                    }
                }
                Operation::Propagate(Propagate::Matching { matching }) => {
                    let headers = req.subgraph_request.headers_mut();
                    req.supergraph_request
                        .headers()
                        .iter()
                        .filter(|(name, _)| {
                            !RESERVED_HEADERS.contains(name) && matching.is_match(name.as_str())
                        })
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
    use crate::plugin::test::MockSubgraphService;
    use crate::plugins::headers::Config;
    use crate::plugins::headers::HeadersLayer;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::Context;

    #[test]
    fn test_subgraph_config() {
        serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            request:
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
            request:
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
            request:
                - remove:
                    named: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                - remove:
                    matching: "d.*"
        "#,
        )
        .unwrap();

        assert!(serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
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
            request:
                - propagate:
                    named: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                - propagate:
                    named: "test"
                    rename: "bif"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
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
            request:
                - propagate:
                    matching: "d.*"
        "#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_insert_static() -> Result<(), BoxError> {
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

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::Static(
            InsertStatic {
                name: "c".try_into()?,
                value: "d".try_into()?,
            },
        ))]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_from_context() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("header_from_context", "my_value_from_context"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(
            Insert::FromContext(InsertFromContext {
                name: "header_from_context".try_into()?,
                from_context: "my_key".to_string(),
            }),
        )]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_from_request_body() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("header_from_request", "my_operation_name"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JSONQuery::parse(".operationName")?,
                default: None,
            },
        ))]))
        .layer(mock);

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

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Named(
            "aa".try_into()?,
        ))]))
        .layer(mock);

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

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Matching(
            Regex::from_str("a[ab]")?,
        ))]))
        .layer(mock);

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

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Matching {
                matching: Regex::from_str("d[ab]")?,
            })]))
            .layer(mock);

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

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: None,
                default: None,
            })]))
            .layer(mock);

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

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ea".try_into()?),
                default: None,
            })]))
            .layer(mock);

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

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "ea".try_into()?,
                rename: None,
                default: Some("defaulted".try_into()?),
            })]))
            .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    fn example_response(_: SubgraphRequest) -> Result<SubgraphResponse, BoxError> {
        Ok(SubgraphResponse::new_from_response(
            http::Response::default(),
            Context::new(),
        ))
    }

    fn example_request() -> SubgraphRequest {
        let ctx = Context::new();
        ctx.insert("my_key", "my_value_from_context".to_string())
            .unwrap();
        SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: ctx,
            subscription_stream: None,
            connection_closed_signal: None,
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
