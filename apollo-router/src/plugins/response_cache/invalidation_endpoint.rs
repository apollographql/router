use std::sync::Arc;
use std::task::Poll;

use bytes::Buf;
use futures::future::BoxFuture;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::header::AUTHORIZATION;
use http::header::CONTENT_TYPE;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use tower::BoxError;
use tower::Service;
use tracing::Span;
use tracing_futures::Instrument;

use super::invalidation::Invalidation;
use super::plugin::Subgraph;
use crate::ListenAddr;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;
use crate::services::router;

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
        const APPLICATION_JSON_HEADER_VALUE: HeaderValue =
            HeaderValue::from_static("application/json");
        let invalidation = self.invalidation.clone();
        let config = self.config.clone();
        Box::pin(
            async move {
                let (parts, body) = req.router_request.into_parts();
                if !parts.headers.contains_key(AUTHORIZATION) {
                    Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                    return router::Response::error_builder()
                        .status_code(StatusCode::UNAUTHORIZED)
                        .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                        .error(
                            graphql::Error::builder()
                                .message(String::from("Missing authorization header"))
                                .extension_code(StatusCode::UNAUTHORIZED.to_string())
                                .build(),
                        )
                        .context(req.context)
                        .build();
                }
                match parts.method {
                    Method::POST => {
                        let body = router::body::into_bytes(body)
                            .instrument(tracing::info_span!("into_bytes"))
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
                                let shared_key_is_valid = body
                                    .iter()
                                    .flat_map(|b| b.subgraph_names())
                                    .all(|subgraph_name| {
                                        validate_shared_key(&config, shared_key, &subgraph_name)
                                    });
                                if !shared_key_is_valid {
                                    Span::current()
                                        .record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                    return router::Response::error_builder()
                                        .status_code(StatusCode::UNAUTHORIZED)
                                        .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                        .error(
                                            graphql::Error::builder()
                                                .message(String::from(
                                                    "Invalid authorization header",
                                                ))
                                                .extension_code(
                                                    StatusCode::UNAUTHORIZED.to_string(),
                                                )
                                                .build(),
                                        )
                                        .context(req.context)
                                        .build();
                                }
                                match invalidation
                                    .invalidate(body)
                                    .instrument(tracing::info_span!("invalidate"))
                                    .await
                                {
                                    Ok(count) => router::Response::http_response_builder()
                                        .response(
                                            http::Response::builder()
                                                .status(StatusCode::ACCEPTED)
                                                .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                                .body(router::body::from_bytes(
                                                    serde_json::to_string(&json!({
                                                        "count": count
                                                    }))?,
                                                ))
                                                .map_err(BoxError::from)?,
                                        )
                                        .context(req.context)
                                        .build(),
                                    Err(err) => {
                                        Span::current()
                                            .record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                        router::Response::error_builder()
                                            .status_code(StatusCode::BAD_REQUEST)
                                            .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                            .error(
                                                graphql::Error::builder()
                                                    .message(err.to_string())
                                                    .extension_code(
                                                        StatusCode::BAD_REQUEST.to_string(),
                                                    )
                                                    .build(),
                                            )
                                            .context(req.context)
                                            .build()
                                    }
                                }
                            }
                            Err(err) => {
                                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                router::Response::error_builder()
                                    .status_code(StatusCode::BAD_REQUEST)
                                    .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                    .error(
                                        graphql::Error::builder()
                                            .message(err)
                                            .extension_code(StatusCode::BAD_REQUEST.to_string())
                                            .build(),
                                    )
                                    .context(req.context)
                                    .build()
                            }
                        }
                    }
                    _ => {
                        Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                        router::Response::error_builder()
                            .status_code(StatusCode::METHOD_NOT_ALLOWED)
                            .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                            .error(
                                graphql::Error::builder()
                                    .message("".to_string())
                                    .extension_code(StatusCode::METHOD_NOT_ALLOWED.to_string())
                                    .build(),
                            )
                            .context(req.context)
                            .build()
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

fn validate_shared_key(
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

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
mod tests {
    use std::collections::HashMap;

    use tower::ServiceExt;

    use super::*;
    use crate::plugins::response_cache::plugin::Storage;
    use crate::plugins::response_cache::postgres::PostgresCacheConfig;
    use crate::plugins::response_cache::postgres::PostgresCacheStorage;
    use crate::plugins::response_cache::postgres::default_batch_size;
    use crate::plugins::response_cache::postgres::default_cleanup_interval;
    use crate::plugins::response_cache::postgres::default_pool_size;

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key() {
        let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
            tls: Default::default(),
            cleanup_interval: default_cleanup_interval(),
            url: "postgres://127.0.0.1".parse().unwrap(),
            username: None,
            password: None,
            idle_timeout: std::time::Duration::from_secs(5),
            acquire_timeout: std::time::Duration::from_millis(500),
            required_to_start: true,
            pool_size: default_pool_size(),
            batch_size: default_batch_size(),
            namespace: Some(String::from("test_invalidation_service_bad_shared_key")),
        })
        .await
        .unwrap();
        let storage = Arc::new(Storage {
            all: Some(Arc::new(pg_cache.into())),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                postgres: None,
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
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key_subgraph() {
        let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
            tls: Default::default(),
            cleanup_interval: default_cleanup_interval(),
            url: "postgres://127.0.0.1".parse().unwrap(),
            username: None,
            password: None,
            idle_timeout: std::time::Duration::from_secs(5),
            acquire_timeout: std::time::Duration::from_millis(500),
            required_to_start: true,
            pool_size: default_pool_size(),
            batch_size: default_batch_size(),
            namespace: Some(String::from(
                "test_invalidation_service_bad_shared_key_subgraph",
            )),
        })
        .await
        .unwrap();
        let storage = Arc::new(Storage {
            all: Some(Arc::new(pg_cache.into())),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                postgres: None,
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
                    enabled: Some(true),
                    postgres: None,
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
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key_subgraphs() {
        let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
            tls: Default::default(),
            cleanup_interval: default_cleanup_interval(),
            url: "postgres://127.0.0.1".parse().unwrap(),
            username: None,
            password: None,
            idle_timeout: std::time::Duration::from_secs(5),
            acquire_timeout: std::time::Duration::from_millis(500),
            required_to_start: true,
            pool_size: default_pool_size(),
            batch_size: default_batch_size(),
            namespace: Some(String::from(
                "test_invalidation_service_bad_shared_key_subgraphs",
            )),
        })
        .await
        .unwrap();
        let storage = Arc::new(Storage {
            all: Some(Arc::new(pg_cache.into())),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                postgres: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                }),
            },
            subgraphs: [
                (
                    String::from("foor"),
                    Subgraph {
                        ttl: None,
                        enabled: Some(true),
                        postgres: None,
                        private_id: None,
                        invalidation: Some(SubgraphInvalidationConfig {
                            enabled: true,
                            shared_key: String::from("test_test"),
                        }),
                    },
                ),
                (
                    String::from("bar"),
                    Subgraph {
                        ttl: None,
                        enabled: Some(true),
                        postgres: None,
                        private_id: None,
                        invalidation: Some(SubgraphInvalidationConfig {
                            enabled: true,
                            shared_key: String::from("test_test_bis"),
                        }),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        });
        // Trying to invalidation with shared_key on subgraph test for a subgraph foo
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test_test")
            .body(
                serde_json::to_vec(&[
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("foo"),
                    },
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("bar"),
                    },
                ])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_good_shared_key_subgraphs() {
        let pg_cache = PostgresCacheStorage::new(&PostgresCacheConfig {
            tls: Default::default(),
            cleanup_interval: default_cleanup_interval(),
            url: "postgres://127.0.0.1".parse().unwrap(),
            username: None,
            password: None,
            idle_timeout: std::time::Duration::from_secs(5),
            acquire_timeout: std::time::Duration::from_millis(500),
            required_to_start: true,
            pool_size: default_pool_size(),
            batch_size: default_batch_size(),
            namespace: Some(String::from(
                "test_invalidation_service_good_shared_key_subgraphs",
            )),
        })
        .await
        .unwrap();
        let storage = Arc::new(Storage {
            all: Some(Arc::new(pg_cache.into())),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                postgres: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                }),
            },
            subgraphs: [
                (
                    String::from("foor"),
                    Subgraph {
                        ttl: None,
                        enabled: Some(true),
                        postgres: None,
                        private_id: None,
                        invalidation: Some(SubgraphInvalidationConfig {
                            enabled: true,
                            shared_key: String::from("test_test"),
                        }),
                    },
                ),
                (
                    String::from("bar"),
                    Subgraph {
                        ttl: None,
                        enabled: Some(true),
                        postgres: None,
                        private_id: None,
                        invalidation: Some(SubgraphInvalidationConfig {
                            enabled: true,
                            shared_key: String::from("test_test_bis"),
                        }),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        });
        // Trying to invalidation with shared_key on subgraph test for a subgraph foo
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test")
            .body(
                serde_json::to_vec(&[
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("foo"),
                    },
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("bar"),
                    },
                ])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert!(res.response.status() != StatusCode::UNAUTHORIZED);
    }
}
