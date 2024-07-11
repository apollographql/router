use std::sync::Arc;
use std::task::Poll;

use bytes::Buf;
use futures::future::BoxFuture;
use http::header::AUTHORIZATION;
use http::Method;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use tower::BoxError;
use tower::Service;
use tracing_futures::Instrument;

use super::entity::Subgraph;
use super::invalidation::Invalidation;
use super::invalidation::InvalidationOrigin;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::plugins::cache::invalidation::InvalidationRequest;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::ListenAddr;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct InvalidationConfig {
    pub(crate) enabled: bool,
    /// Listen address on which the callback must listen (default: 127.0.0.1:4000)
    pub(crate) listen: Option<ListenAddr>,
    /// Specify on which path you want to listen for callbacks (default: /callback)
    pub(crate) path: Option<String>,

    pub(crate) shared_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InvalidationEndpointConfig {
    /// This one will be skipped if used in specific subgraph entry
    pub(crate) path: String,
    pub(crate) listen: ListenAddr,
}

impl TryFrom<InvalidationConfig> for InvalidationEndpointConfig {
    type Error = BoxError;

    fn try_from(config: InvalidationConfig) -> Result<Self, Self::Error> {
        let path = config
            .path
            .clone()
            .unwrap_or_else(|| "/invalidation".to_string());
        let path = path.trim_end_matches('/');

        let cfg = Self {
            path: path.to_string(),
            listen: config
                .listen
                .unwrap_or_else(crate::plugins::subscription::default_listen_addr),
        };

        Ok(cfg)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum InvalidationType {
    EntityType,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InvalidationKey {
    pub(crate) id: String,
    pub(crate) field: String,
}

#[derive(Clone)]
pub(crate) struct InvalidationService {
    config: Arc<SubgraphConfiguration<Subgraph>>,
    invalidation: Invalidation,
}

impl InvalidationService {
    pub(crate) fn new(
        config: Arc<SubgraphConfiguration<Subgraph>>,
        invalidation: Invalidation,
    ) -> Self {
        Self {
            config,
            invalidation,
        }
    }
}

impl Service<router::Request> for InvalidationService {
    type Response = router::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        let mut invalidation = self.invalidation.clone();
        let config = self.config.clone();
        Box::pin(
            async move {
                let (parts, body) = req.router_request.into_parts();
                if !parts.headers.contains_key(AUTHORIZATION) {
                    return Ok(router::Response {
                        response: http::Response::builder()
                            .status(StatusCode::UNAUTHORIZED)
                            .body("Missing authorization header".into())
                            .map_err(BoxError::from)?,
                        context: req.context,
                    });
                }
                match parts.method {
                    Method::POST => {
                        let body = Into::<RouterBody>::into(body)
                            .to_bytes()
                            .await
                            .map_err(|e| format!("failed to get the request body: {e}"))
                            .and_then(|bytes| {
                                serde_json::from_reader::<_, Vec<InvalidationRequest>>(
                                    bytes.reader(),
                                )
                                .map_err(|err| {
                                    format!(
                                        "failed to deserialize the request body into JSON: {err}"
                                    )
                                })
                            });
                        let shared_key = parts
                            .headers
                            .get(AUTHORIZATION)
                            .ok_or("cannot find authorization header")?
                            .to_str()?;
                        match body {
                            Ok(body) => {
                                let valid_shared_key =
                                    body.iter().map(|b| b.subgraph_name()).any(|subgraph_name| {
                                        valid_shared_key(&config, shared_key, subgraph_name)
                                    });
                                if !valid_shared_key {
                                    return Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::UNAUTHORIZED)
                                            .body("Invalid authorization header".into())
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    });
                                }
                                match invalidation
                                    .invalidate(InvalidationOrigin::Endpoint, body)
                                    .await
                                {
                                    Ok(count) => Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::ACCEPTED)
                                            .body(
                                                serde_json::to_string(&json!({
                                                    "count": count
                                                }))?
                                                .into(),
                                            )
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    }),
                                    Err(err) => Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::BAD_REQUEST)
                                            .body(err.to_string().into())
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    }),
                                }
                            }
                            Err(err) => Ok(router::Response {
                                response: http::Response::builder()
                                    .status(StatusCode::BAD_REQUEST)
                                    .body(err.into())
                                    .map_err(BoxError::from)?,
                                context: req.context,
                            }),
                        }
                    }
                    _ => Ok(router::Response {
                        response: http::Response::builder()
                            .status(StatusCode::METHOD_NOT_ALLOWED)
                            .body("".into())
                            .map_err(BoxError::from)?,
                        context: req.context,
                    }),
                }
            }
            .instrument(tracing::info_span!("invalidation_endpoint")),
        )
    }
}

fn valid_shared_key(
    config: &SubgraphConfiguration<Subgraph>,
    shared_key: &str,
    subgraph_name: &str,
) -> bool {
    config
        .all
        .invalidation
        .as_ref()
        .map(|i| i.shared_key == shared_key)
        .unwrap_or_default()
        || config
            .subgraphs
            .get(subgraph_name)
            .and_then(|s| s.invalidation.as_ref())
            .map(|i| i.shared_key == shared_key)
            .unwrap_or_default()
}
