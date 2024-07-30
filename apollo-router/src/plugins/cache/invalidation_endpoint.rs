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
use tracing::Span;
use tracing_futures::Instrument;

use super::entity::Subgraph;
use super::invalidation::Invalidation;
use super::invalidation::InvalidationOrigin;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::plugins::cache::invalidation::InvalidationRequest;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::ListenAddr;

pub(crate) const INVALIDATION_ENDPOINT_SPAN_NAME: &str = "invalidation_endpoint";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct SubgraphInvalidationConfig {
    /// Enable the invalidation
    pub(crate) enabled: bool,
    /// Shared key needed to request the invalidation endpoint
    pub(crate) shared_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct InvalidationEndpointConfig {
    /// Specify on which path you want to listen for invalidation endpoint.
    pub(crate) path: String,
    /// Listen address on which the invalidation endpoint must listen.
    pub(crate) listen: ListenAddr,
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
                    Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
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
                            .instrument(tracing::info_span!("to_bytes"))
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
                            .to_str()
                            .inspect_err(|_err| {
                                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                            })?;
                        match body {
                            Ok(body) => {
                                Span::current().record(
                                    "invalidation.request.kinds",
                                    body.iter()
                                        .map(|i| i.kind())
                                        .collect::<Vec<&'static str>>()
                                        .join(", "),
                                );
                                let valid_shared_key =
                                    body.iter().map(|b| b.subgraph_name()).any(|subgraph_name| {
                                        valid_shared_key(&config, shared_key, subgraph_name)
                                    });
                                if !valid_shared_key {
                                    Span::current()
                                        .record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
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
                                    .instrument(tracing::info_span!("invalidate"))
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
                                    Err(err) => {
                                        Span::current()
                                            .record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                        Ok(router::Response {
                                            response: http::Response::builder()
                                                .status(StatusCode::BAD_REQUEST)
                                                .body(err.to_string().into())
                                                .map_err(BoxError::from)?,
                                            context: req.context,
                                        })
                                    }
                                }
                            }
                            Err(err) => {
                                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                Ok(router::Response {
                                    response: http::Response::builder()
                                        .status(StatusCode::BAD_REQUEST)
                                        .body(err.into())
                                        .map_err(BoxError::from)?,
                                    context: req.context,
                                })
                            }
                        }
                    }
                    _ => {
                        Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                        Ok(router::Response {
                            response: http::Response::builder()
                                .status(StatusCode::METHOD_NOT_ALLOWED)
                                .body("".into())
                                .map_err(BoxError::from)?,
                            context: req.context,
                        })
                    }
                }
            }
            .instrument(tracing::info_span!(
                INVALIDATION_ENDPOINT_SPAN_NAME,
                "invalidation.request.kinds" = ::tracing::field::Empty,
                "otel.status_code" = OTEL_STATUS_CODE_OK,
            )),
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio::sync::broadcast::Sender;
    use tokio_stream::StreamExt;
    use tower::ServiceExt;

    use super::*;
    use crate::plugins::cache::invalidation::InvalidationError;
    use crate::plugins::cache::invalidation::InvalidationTopic;
    use crate::Notify;

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key() {
        #[allow(clippy::type_complexity)]
        let mut notify: Notify<
            InvalidationTopic,
            (
                Vec<InvalidationRequest>,
                InvalidationOrigin,
                Sender<Result<u64, InvalidationError>>,
            ),
        > = Notify::new(None, None, None);
        let (handle, _b) = notify
            .create_or_subscribe(InvalidationTopic, false)
            .await
            .unwrap();
        let invalidation = Invalidation {
            enabled: true,
            handle,
        };
        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: true,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                }),
            },
            subgraphs: HashMap::new(),
        });
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "testttt")
            .body(
                serde_json::to_vec(&[
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("test"),
                    },
                    InvalidationRequest::Type {
                        subgraph: String::from("test"),
                        r#type: String::from("Test"),
                    },
                ])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_good_sub_shared_key() {
        #[allow(clippy::type_complexity)]
        let mut notify: Notify<
            InvalidationTopic,
            (
                Vec<InvalidationRequest>,
                InvalidationOrigin,
                Sender<Result<u64, InvalidationError>>,
            ),
        > = Notify::new(None, None, None);
        let (handle, _b) = notify
            .create_or_subscribe(InvalidationTopic, false)
            .await
            .unwrap();
        let h = handle.clone();

        tokio::task::spawn(async move {
            let mut handle = h.into_stream();
            let mut called = false;
            while let Some((requests, origin, response_tx)) = handle.next().await {
                called = true;
                if requests
                    != [
                        InvalidationRequest::Subgraph {
                            subgraph: String::from("test"),
                        },
                        InvalidationRequest::Type {
                            subgraph: String::from("test"),
                            r#type: String::from("Test"),
                        },
                    ]
                {
                    response_tx
                        .send(Err(InvalidationError::Custom(format!(
                            "it's not the right invalidation requests : {requests:?}"
                        ))))
                        .unwrap();
                    return;
                }
                if origin != InvalidationOrigin::Endpoint {
                    response_tx
                        .send(Err(InvalidationError::Custom(format!(
                            "it's not the right invalidation origin : {origin:?}"
                        ))))
                        .unwrap();
                    return;
                }
                response_tx.send(Ok(0)).unwrap();
            }
            assert!(called);
        });

        let invalidation = Invalidation {
            enabled: true,
            handle: handle.clone(),
        };
        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: true,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                }),
            },
            subgraphs: [(
                String::from("test"),
                Subgraph {
                    ttl: None,
                    enabled: true,
                    private_id: None,
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: String::from("test_test"),
                    }),
                },
            )]
            .into_iter()
            .collect(),
        });
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test_test")
            .body(
                serde_json::to_vec(&[
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("test"),
                    },
                    InvalidationRequest::Type {
                        subgraph: String::from("test"),
                        r#type: String::from("Test"),
                    },
                ])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(res.response.status(), StatusCode::ACCEPTED);
        let h = handle.clone();

        tokio::task::spawn(async move {
            let mut handle = h.into_stream();
            let mut called = false;
            while let Some((requests, origin, response_tx)) = handle.next().await {
                called = true;
                if requests
                    != [
                        InvalidationRequest::Subgraph {
                            subgraph: String::from("test"),
                        },
                        InvalidationRequest::Type {
                            subgraph: String::from("test"),
                            r#type: String::from("Test"),
                        },
                    ]
                {
                    response_tx
                        .send(Err(InvalidationError::Custom(format!(
                            "it's not the right invalidation requests : {requests:?}"
                        ))))
                        .unwrap();
                    return;
                }
                if origin != InvalidationOrigin::Endpoint {
                    response_tx
                        .send(Err(InvalidationError::Custom(format!(
                            "it's not the right invalidation origin : {origin:?}"
                        ))))
                        .unwrap();
                    return;
                }
                response_tx.send(Ok(0)).unwrap();
            }
            assert!(called);
        });
    }

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key_subgraph() {
        #[allow(clippy::type_complexity)]
        let mut notify: Notify<
            InvalidationTopic,
            (
                Vec<InvalidationRequest>,
                InvalidationOrigin,
                Sender<Result<u64, InvalidationError>>,
            ),
        > = Notify::new(None, None, None);
        let (handle, _b) = notify
            .create_or_subscribe(InvalidationTopic, false)
            .await
            .unwrap();
        let invalidation = Invalidation {
            enabled: true,
            handle,
        };
        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: true,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                }),
            },
            subgraphs: [(
                String::from("test"),
                Subgraph {
                    ttl: None,
                    enabled: true,
                    private_id: None,
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: String::from("test_test"),
                    }),
                },
            )]
            .into_iter()
            .collect(),
        });
        // Trying to invalidation with shared_key on subgraph test for a subgraph foo
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test_test")
            .body(
                serde_json::to_vec(&[InvalidationRequest::Subgraph {
                    subgraph: String::from("foo"),
                }])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service() {
        #[allow(clippy::type_complexity)]
        let mut notify: Notify<
            InvalidationTopic,
            (
                Vec<InvalidationRequest>,
                InvalidationOrigin,
                Sender<Result<u64, InvalidationError>>,
            ),
        > = Notify::new(None, None, None);
        let (handle, _b) = notify
            .create_or_subscribe(InvalidationTopic, false)
            .await
            .unwrap();
        let h = handle.clone();

        tokio::task::spawn(async move {
            let mut handle = h.into_stream();
            let mut called = false;
            while let Some((requests, origin, response_tx)) = handle.next().await {
                called = true;
                if requests
                    != [
                        InvalidationRequest::Subgraph {
                            subgraph: String::from("test"),
                        },
                        InvalidationRequest::Type {
                            subgraph: String::from("test"),
                            r#type: String::from("Test"),
                        },
                    ]
                {
                    response_tx
                        .send(Err(InvalidationError::Custom(format!(
                            "it's not the right invalidation requests : {requests:?}"
                        ))))
                        .unwrap();
                    return;
                }
                if origin != InvalidationOrigin::Endpoint {
                    response_tx
                        .send(Err(InvalidationError::Custom(format!(
                            "it's not the right invalidation origin : {origin:?}"
                        ))))
                        .unwrap();
                    return;
                }
                response_tx.send(Ok(2)).unwrap();
            }
            assert!(called);
        });

        let invalidation = Invalidation {
            enabled: true,
            handle,
        };
        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: true,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                }),
            },
            subgraphs: HashMap::new(),
        });
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test")
            .body(
                serde_json::to_vec(&[
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("test"),
                    },
                    InvalidationRequest::Type {
                        subgraph: String::from("test"),
                        r#type: String::from("Test"),
                    },
                ])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(res.response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &hyper::body::to_bytes(res.response.into_body())
                    .await
                    .unwrap()
            )
            .unwrap(),
            serde_json::json!({"count": 2})
        );
    }
}
