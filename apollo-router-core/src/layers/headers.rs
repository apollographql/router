use std::task::{Context, Poll};

use crate::layer::ConfigurableLayer;
use crate::{register_layer, SubgraphRequest};
use http::header::{
    HeaderName, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST, PROXY_AUTHENTICATE,
    PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING, UPGRADE,
};
use http::HeaderValue;
use mockall::lazy_static;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::{BoxError, Layer, Service};

register_layer!("headers", "insert", InsertLayer);
register_layer!("headers", "remove", RemoveLayer);
register_layer!("headers", "propagate", PropagateLayer);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RemoveConfig {
    Name(String),
    Matching {
        #[serde(default = "default_regex")]
        regex: String,
    },
}

#[derive(Clone, JsonSchema, Deserialize)]
struct InsertConfig {
    name: String,
    value: String,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PropagateConfig {
    Matching {
        #[serde(default = "default_regex")]
        regex: String,
    },
    Named {
        name: String,
        rename: Option<String>,
        default_value: Option<String>,
    },
}

fn default_regex() -> String {
    ".*".to_string()
}

struct InsertLayer {
    name: HeaderName,
    value: HeaderValue,
}

impl ConfigurableLayer for InsertLayer {
    type Config = InsertConfig;
    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(Self {
            name: configuration.name.try_into()?,
            value: configuration.value.try_into()?,
        })
    }
}

impl<S> Layer<S> for InsertLayer {
    type Service = InsertService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        InsertService {
            inner,
            name: self.name.clone(),
            value: self.value.clone(),
        }
    }
}

struct InsertService<S> {
    inner: S,
    name: HeaderName,
    value: HeaderValue,
}

impl<S> Service<SubgraphRequest> for InsertService<S>
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
        req.http_request
            .headers_mut()
            .insert(&self.name, self.value.clone());
        self.inner.call(req)
    }
}

struct RemoveLayer {
    name: Option<HeaderName>,
    regex: Option<Regex>,
}

impl ConfigurableLayer for RemoveLayer {
    type Config = RemoveConfig;

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(match configuration {
            RemoveConfig::Name(name) => Self {
                name: Some(name.try_into()?),
                regex: None,
            },
            RemoveConfig::Matching { regex } => Self {
                name: None,
                regex: Some(Regex::new(regex.as_str())?),
            },
        })
    }
}

impl<S> Layer<S> for RemoveLayer {
    type Service = RemoveService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RemoveService {
            inner,
            name: self.name.clone(),
            regex: self.regex.clone(),
        }
    }
}

struct RemoveService<S> {
    inner: S,
    name: Option<HeaderName>,
    regex: Option<Regex>,
}

impl<S> Service<SubgraphRequest> for RemoveService<S>
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
        let headers = req.http_request.headers_mut();
        if let Some(name) = &self.name {
            headers.remove(name);
        } else if let Some(regex) = &self.regex {
            let matching_headers = headers
                .iter()
                .filter_map(|(name, _)| regex.is_match(name.as_str()).then(|| name.clone()))
                .filter(|name| !RESERVED_HEADERS.contains(name))
                .collect::<Vec<_>>();
            for name in matching_headers {
                headers.remove(name);
            }
        }
        self.inner.call(req)
    }
}

struct PropagateLayer {
    name: Option<HeaderName>,
    rename: Option<HeaderName>,
    regex: Option<Regex>,
    default_value: Option<HeaderValue>,
}

impl ConfigurableLayer for PropagateLayer {
    type Config = PropagateConfig;

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(match configuration {
            PropagateConfig::Named {
                name,
                rename,
                default_value,
            } => Self {
                name: Some(name.try_into()?),
                rename: rename.map(|a| a.as_str().try_into()).transpose()?,
                regex: None,
                default_value: default_value.map(|a| a.as_str().try_into()).transpose()?,
            },
            PropagateConfig::Matching { regex } => Self {
                name: None,
                rename: None,
                regex: Some(Regex::new(regex.as_str())?),
                default_value: None,
            },
        })
    }
}

impl<S> Layer<S> for PropagateLayer {
    type Service = PropagateService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PropagateService {
            inner,
            name: self.name.clone(),
            rename: self.rename.clone(),
            regex: self.regex.clone(),
            default_value: self.default_value.clone(),
        }
    }
}

struct PropagateService<S> {
    inner: S,
    name: Option<HeaderName>,
    rename: Option<HeaderName>,
    regex: Option<Regex>,
    default_value: Option<HeaderValue>,
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

impl<S> Service<SubgraphRequest> for PropagateService<S>
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
        let headers = req.http_request.headers_mut();
        if let Some(name) = &self.name {
            let value = req.context.request.headers().get(name);
            if let Some(value) = value.or(self.default_value.as_ref()) {
                headers.insert(self.rename.as_ref().unwrap_or(name), value.clone());
            }
        } else if let Some(regex) = &self.regex {
            req.context
                .request
                .headers()
                .iter()
                .filter(|(name, _)| regex.is_match(name.as_str()))
                .filter(|(name, _)| !RESERVED_HEADERS.contains(name))
                .for_each(|(name, value)| {
                    headers.insert(name, value.clone());
                });
        } else {
            for (name, value) in req.context.request.headers() {
                headers.insert(name, value.clone());
            }
        }
        self.inner.call(req)
    }
}

#[cfg(test)]
mod test {
    use crate::fetch::OperationKind;
    use crate::headers::{
        InsertConfig, InsertLayer, PropagateConfig, PropagateLayer, RemoveConfig, RemoveLayer,
    };
    use crate::layer::ConfigurableLayer;
    use crate::plugin_utils::MockSubgraphService;
    use crate::{Context, Request, Response, SubgraphRequest, SubgraphResponse};
    use http::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST};
    use std::collections::HashSet;
    use std::sync::Arc;
    use tower::{BoxError, Layer};
    use tower::{Service, ServiceExt};

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

        let mut service = InsertLayer::new(InsertConfig {
            name: "c".to_string(),
            value: "d".to_string(),
        })?
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
            RemoveLayer::new(RemoveConfig::Name("aa".to_string()))?.layer(mock.build());

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

        let mut service = RemoveLayer::new(RemoveConfig::Matching {
            regex: "a[ab]".to_string(),
        })?
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

        let mut service = PropagateLayer::new(PropagateConfig::Matching {
            regex: "d[ab]".to_string(),
        })?
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

        let mut service = PropagateLayer::new(PropagateConfig::Named {
            name: "da".to_string(),
            rename: None,
            default_value: None,
        })?
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

        let mut service = PropagateLayer::new(PropagateConfig::Named {
            name: "da".to_string(),
            rename: Some("ea".to_string()),
            default_value: None,
        })?
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

        let mut service = PropagateLayer::new(PropagateConfig::Named {
            name: "ea".to_string(),
            rename: None,
            default_value: Some("defaulted".to_string()),
        })?
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    fn example_response(_: SubgraphRequest) -> Result<SubgraphResponse, BoxError> {
        Ok(SubgraphResponse {
            response: http::Response::builder()
                .body(Response::builder().build())
                .unwrap()
                .into(),
            context: example_originating_request(),
        })
    }

    fn example_originating_request() -> Context {
        Context::new().with_request(Arc::new(
            http::Request::builder()
                .method("GET")
                .header("da", "vda")
                .header("db", "vdb")
                .header("dc", "vdc")
                .header(HOST, "host")
                .header(CONTENT_LENGTH, "2")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .unwrap()
                .into(),
        ))
    }

    fn example_request() -> SubgraphRequest {
        SubgraphRequest {
            http_request: http::Request::builder()
                .method("GET")
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .unwrap()
                .into(),
            operation_kind: OperationKind::Query,
            context: example_originating_request(),
        }
    }

    impl SubgraphRequest {
        fn assert_headers(&self, headers: Vec<(&'static str, &'static str)>) -> bool {
            let mut headers = headers.clone();
            headers.push((HOST.as_str(), "rhost"));
            headers.push((CONTENT_LENGTH.as_str(), "22"));
            headers.push((CONTENT_TYPE.as_str(), "graphql"));
            let actual_headers = self
                .http_request
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
                .collect::<HashSet<_>>();
            assert_eq!(actual_headers, headers.into_iter().collect::<HashSet<_>>());

            true
        }
    }
}
