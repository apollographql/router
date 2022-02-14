use crate::layer::ConfigurableLayer;
use crate::{register_layer, SubgraphRequest};
use http::header::HeaderName;
use http::HeaderValue;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use std::str::FromStr;
use std::task::{Context, Poll};
use tower::{BoxError, Layer, Service};

register_layer!("headers", "insert", InsertLayer);
register_layer!("headers", "remove", RemoveLayer);
register_layer!("headers", "propagate", PropagateLayer);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RemoveConfig {
    Name(String),
    Regex(String),
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

impl Default for InsertLayer {
    fn default() -> Self {
        Self {
            name: HeaderName::from_static("name"),
            value: HeaderValue::from_static("value"),
        }
    }
}

impl ConfigurableLayer for InsertLayer {
    type Config = InsertConfig;

    fn configure(mut self, configuration: Self::Config) -> Result<Self, BoxError> {
        self.name = HeaderName::from_str(configuration.name.as_str())?;
        self.value = HeaderValue::from_str(configuration.value.as_str())?;
        Ok(self)
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

#[derive(Default)]
struct RemoveLayer {
    name: Option<HeaderName>,
    regex: Option<Regex>,
}

impl ConfigurableLayer for RemoveLayer {
    type Config = RemoveConfig;

    fn configure(mut self, configuration: Self::Config) -> Result<Self, BoxError> {
        match configuration {
            RemoveConfig::Name(name) => {
                self.name = Some(name.try_into()?);
            }
            RemoveConfig::Regex(regex) => self.regex = Some(Regex::new(regex.as_str())?),
        }
        Ok(self)
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
                .filter(|(name, _)| regex.is_match(name.as_str()))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            for name in matching_headers {
                headers.remove(name);
            }
        }
        self.inner.call(req)
    }
}

#[derive(Default)]
struct PropagateLayer {
    name: Option<HeaderName>,
    regex: Option<Regex>,
    default_value: Option<HeaderValue>,
}

impl ConfigurableLayer for PropagateLayer {
    type Config = PropagateConfig;

    fn configure(mut self, configuration: Self::Config) -> Result<Self, BoxError> {
        match configuration {
            PropagateConfig::Named {
                name,
                default_value,
            } => {
                self.name = Some(name.try_into()?);
                if let Some(default_value) = &default_value {
                    self.default_value = Some(default_value.try_into()?);
                }
            }
            PropagateConfig::Matching { regex } => self.regex = Some(Regex::new(regex.as_str())?),
        }

        Ok(self)
    }
}

impl<S> Layer<S> for PropagateLayer {
    type Service = RemoveService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RemoveService {
            inner,
            name: self.name.clone(),
            regex: self.regex.clone(),
        }
    }
}

struct PropagateService<S> {
    inner: S,
    name: Option<HeaderName>,
    regex: Option<Regex>,
    default_value: Option<HeaderValue>,
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
        if let Some(name) = &self.name {
            if let Some(value) = req
                .context
                .request
                .headers()
                .get(name)
                .or_else(|| self.default_value.as_ref())
            {
                req.http_request.headers_mut().insert(name, value.clone());
            }
        } else if let Some(regex) = &self.regex {
            req.context
                .request
                .headers()
                .iter()
                .filter(|(name, _)| regex.is_match(name.as_str()))
                .for_each(|(name, value)| {
                    req.http_request.headers_mut().insert(name, value.clone());
                });
        } else {
            for (name, value) in req.context.request.headers() {
                req.http_request.headers_mut().insert(name, value.clone());
            }
        }
        self.inner.call(req)
    }
}

#[cfg(test)]
mod test {
    // TODO This is currently not possible because of the way that context is structured.
    // use crate::headers::{InsertConfig, InsertLayer};
    // use crate::layer::ConfigurableLayer;
    // use crate::plugin_utils::MockSubgraphService;
    // use crate::{http_compat, Context, Query, Request, SubgraphRequest};
    // use mockall::predicate::eq;
    // use std::sync::Arc;
    // use tower::Layer;
    // use tower_service::Service;
    //
    // #[tokio::test]
    // async fn test_insert() {
    //     let context_request = http::Request::builder()
    //         .method("GET")
    //         .header("A", "B")
    //         .body(Request::builder().query("query").build())
    //         .unwrap()
    //         .into();
    //     let expected = SubgraphRequest {
    //         http_request: http::Request::builder()
    //             .method("GET")
    //             .header("A", "B")
    //             .body(Request::builder().query("query").build())
    //             .unwrap()
    //             .into(),
    //         context: Context::new().with_request(Arc::new(context_request)),
    //     };
    //
    //     let mut mock = MockSubgraphService::new();
    //     mock.expect_call().times(1).with(eq(expected));
    //
    //     let mut service = InsertLayer::default()
    //         .configure(InsertConfig {
    //             name: "A".to_string(),
    //             value: "B".to_string(),
    //         })?
    //         .layer(mock);
    //
    //     service.call(expected).await;
    // }
}
