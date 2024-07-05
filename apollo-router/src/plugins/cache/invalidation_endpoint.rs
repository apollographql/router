use std::sync::Arc;
use std::task::Poll;

use bytes::Buf;
use futures::future::BoxFuture;
use http::Method;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::Service;
use tracing_futures::Instrument;

use super::entity::Subgraph;
use super::invalidation::Invalidation;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::plugins::cache::invalidation::InvalidationRequest;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::ListenAddr;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct InvalidationConfig {
    pub(crate) enabled: bool,
    /// This one will be skipped if used in specific subgraph entry
    #[schemars(with = "Option<String>")]
    pub(crate) endpoint: Option<url::Url>,
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

    fn try_from(value: InvalidationConfig) -> Result<Self, Self::Error> {
        let endpoint = match value.endpoint {
            Some(e) => e,
            None => {
                return Err(BoxError::from(
                    "endpoint value must be set for invalidation cache",
                ))
            }
        };

        let cfg = Self {
            path: endpoint.path().to_string(),
            listen: ListenAddr::SocketAddr(endpoint.authority().parse()?),
        };

        dbg!(&cfg);
        Ok(cfg)
    }
}

// #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// #[serde(rename_all = "camelCase")]
// pub(crate) struct InvalidationPayload {
//     /// required, kind of invalidation event. Can be "Subgraph", "Type", "Key" or "Tag"
//     pub(crate) kind: InvalidationKind,
//     /// optional, invalidate entries from specific subgraph
//     pub(crate) subgraph: Option<String>,
//     #[serde(rename = "type")]
//     pub(crate) type_field: Option<InvalidationType>,
//     /// optional, invalidate entries indexed by this key object
//     pub(crate) key: Option<InvalidationKey>,
//     /// optional, invalidate entries containing types or field marked with the tag
//     pub(crate) tag: Option<String>,
//     /// optional, used to mark an entry as stale if the router is configured with `stale-while-revalidate`
//     #[serde(rename = "mark-stale", default)]
//     pub(crate) mark_stale: bool,
// }

// #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
// #[serde(rename_all = "camelCase")]
// pub(crate) enum InvalidationKind {
//     Type,
//     Subgraph,
//     Key,
//     Tag,
// }

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
    // TODO: will be useful when checking the shared_key
    #[allow(dead_code)]
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
        let invalidation = self.invalidation.clone();
        Box::pin(
            async move {
                let (parts, body) = req.router_request.into_parts();
                // TODO: check the shared_key
                match parts.method {
                    Method::POST => {
                        let body = Into::<RouterBody>::into(body)
                            .to_bytes()
                            .await
                            .map_err(|e| format!("failed to get the request body: {e}"))
                            .and_then(|bytes| {
                                serde_json::from_reader::<_, InvalidationRequest>(bytes.reader())
                                    .map_err(|err| {
                                        format!(
                                        "failed to deserialize the request body into JSON: {err}"
                                    )
                                    })
                            });
                        match body {
                            Ok(body) => invalidation.handle_request(&body).await,
                            Err(err) => {
                                return Ok(router::Response {
                                    response: http::Response::builder()
                                        .status(StatusCode::BAD_REQUEST)
                                        .body(err.into())
                                        .map_err(BoxError::from)?,
                                    context: req.context,
                                });
                            }
                        }

                        Ok(router::Response {
                            response: http::Response::builder()
                                .status(StatusCode::ACCEPTED)
                                .body("".into())
                                .map_err(BoxError::from)?,
                            context: req.context,
                        })
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
