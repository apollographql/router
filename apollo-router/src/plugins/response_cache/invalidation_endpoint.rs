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

use super::plugin::ConnectorCacheConfiguration;

#[derive(Clone)]
pub(crate) struct InvalidationService {
    config: Arc<SubgraphConfiguration<Subgraph>>,
    connector_config: Arc<ConnectorCacheConfiguration>,
    invalidation: Invalidation,
}

impl InvalidationService {
    pub(crate) fn new(
        config: Arc<SubgraphConfiguration<Subgraph>>,
        connector_config: Arc<ConnectorCacheConfiguration>,
        invalidation: Invalidation,
    ) -> Self {
        Self {
            config,
            connector_config,
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
        let connector_config = self.connector_config.clone();
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
                                let shared_key_is_valid = body.iter().all(|req| {
                                    if req.is_connector() {
                                        validate_connector_shared_key(
                                            &connector_config,
                                            shared_key,
                                            req,
                                        )
                                    } else {
                                        req.subgraph_names().iter().all(|name| {
                                            validate_shared_key(&config, shared_key, name)
                                                || validate_connector_shared_key_by_source(
                                                    &connector_config,
                                                    shared_key,
                                                    name,
                                                )
                                        })
                                    }
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

fn validate_connector_shared_key(
    config: &ConnectorCacheConfiguration,
    shared_key: &str,
    request: &InvalidationRequest,
) -> bool {
    let source_name = match request {
        InvalidationRequest::ConnectorSource { source }
        | InvalidationRequest::ConnectorType { source, .. } => source,
        _ => return false,
    };

    config
        .all
        .invalidation
        .as_ref()
        .map(|i| i.shared_key == shared_key)
        .unwrap_or_default()
        || config
            .sources
            .get(source_name)
            .and_then(|s| s.invalidation.as_ref())
            .map(|i| i.shared_key == shared_key)
            .unwrap_or_default()
}

/// Validate shared key for a connector source by name.
/// Used for CacheTag requests where the `subgraphs` field may contain connector source names.
fn validate_connector_shared_key_by_source(
    config: &ConnectorCacheConfiguration,
    shared_key: &str,
    source_name: &str,
) -> bool {
    config
        .all
        .invalidation
        .as_ref()
        .map(|i| i.shared_key == shared_key)
        .unwrap_or_default()
        || config
            .sources
            .get(source_name)
            .and_then(|s| s.invalidation.as_ref())
            .map(|i| i.shared_key == shared_key)
            .unwrap_or_default()
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

    use tokio::sync::broadcast;
    use tower::ServiceExt;

    use super::*;
    use crate::plugins::response_cache::plugin::ConnectorCacheSource;
    use crate::plugins::response_cache::plugin::StorageInterface;
    use crate::plugins::response_cache::storage::redis::Config;
    use crate::plugins::response_cache::storage::redis::Storage;
    use crate::services::router::body;

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key() {
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_bad_shared_key"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                }),
            },
            subgraphs: HashMap::new(),
        });
        let service = InvalidationService::new(
            config,
            Arc::new(ConnectorCacheConfiguration::default()),
            invalidation,
        );
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
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_bad_shared_key_subgraph"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
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
                    redis: None,
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
        let service = InvalidationService::new(
            config,
            Arc::new(ConnectorCacheConfiguration::default()),
            invalidation,
        );
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
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_bad_shared_key_subgraphs"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
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
                        redis: None,
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
                        redis: None,
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
        let service = InvalidationService::new(
            config,
            Arc::new(ConnectorCacheConfiguration::default()),
            invalidation,
        );
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
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_good_shared_key_subgraphs"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
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
                        redis: None,
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
                        redis: None,
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
        let service = InvalidationService::new(
            config,
            Arc::new(ConnectorCacheConfiguration::default()),
            invalidation,
        );
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

    #[tokio::test]
    async fn test_invalidation_service_deny_unknown_fields() {
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_good_shared_key_subgraphs"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                }),
            },
            subgraphs: HashMap::new(),
        });
        // Trying to invalidation with shared_key on subgraph test for a subgraph foo
        let service = InvalidationService::new(
            config,
            Arc::new(ConnectorCacheConfiguration::default()),
            invalidation,
        );
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test")
            .body(
                serde_json::to_vec(&[serde_json::json!({
                    "kind": "type",
                    "subgraph": "foo",
                    "type": "User",
                    "key": {
                        "id": "1"
                    }
                })])
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
        assert_eq!(res.response.status(), StatusCode::BAD_REQUEST);
        let response_body_str = body::into_string(res.response.into_body()).await.unwrap();
        assert!(
            response_body_str
                .contains("failed to deserialize the request body into JSON: unknown field")
        );
    }

    #[test]
    fn validate_connector_shared_key_all_config() {
        let config = ConnectorCacheConfiguration {
            all: ConnectorCacheSource {
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: "my_secret".to_string(),
                }),
                ..Default::default()
            },
            sources: HashMap::new(),
        };
        let req = InvalidationRequest::ConnectorSource {
            source: "any_source".to_string(),
        };
        assert!(validate_connector_shared_key(&config, "my_secret", &req));
    }

    #[test]
    fn validate_connector_shared_key_source_specific() {
        let config = ConnectorCacheConfiguration {
            all: ConnectorCacheSource::default(),
            sources: [(
                "mysubgraph.my_api".to_string(),
                ConnectorCacheSource {
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: "source_secret".to_string(),
                    }),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        };
        let req = InvalidationRequest::ConnectorSource {
            source: "mysubgraph.my_api".to_string(),
        };
        assert!(validate_connector_shared_key(
            &config,
            "source_secret",
            &req
        ));
    }

    #[test]
    fn validate_connector_shared_key_mismatch() {
        let config = ConnectorCacheConfiguration {
            all: ConnectorCacheSource {
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: "correct_key".to_string(),
                }),
                ..Default::default()
            },
            sources: HashMap::new(),
        };
        let req = InvalidationRequest::ConnectorSource {
            source: "any_source".to_string(),
        };
        assert!(!validate_connector_shared_key(&config, "wrong_key", &req));
    }

    #[test]
    fn validate_connector_shared_key_non_connector() {
        let config = ConnectorCacheConfiguration {
            all: ConnectorCacheSource {
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: "my_secret".to_string(),
                }),
                ..Default::default()
            },
            sources: HashMap::new(),
        };
        let req = InvalidationRequest::Subgraph {
            subgraph: "test".to_string(),
        };
        assert!(!validate_connector_shared_key(&config, "my_secret", &req));
    }

    #[test]
    fn validate_connector_shared_key_by_source_all() {
        let config = ConnectorCacheConfiguration {
            all: ConnectorCacheSource {
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: "all_key".to_string(),
                }),
                ..Default::default()
            },
            sources: HashMap::new(),
        };
        assert!(validate_connector_shared_key_by_source(
            &config,
            "all_key",
            "unknown_source"
        ));
    }

    #[test]
    fn validate_connector_shared_key_by_source_specific() {
        let config = ConnectorCacheConfiguration {
            all: ConnectorCacheSource::default(),
            sources: [(
                "mysubgraph.my_api".to_string(),
                ConnectorCacheSource {
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: "source_key".to_string(),
                    }),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        };
        assert!(validate_connector_shared_key_by_source(
            &config,
            "source_key",
            "mysubgraph.my_api"
        ));
    }

    #[test]
    fn validate_connector_shared_key_by_source_no_config() {
        let config = ConnectorCacheConfiguration::default();
        assert!(!validate_connector_shared_key_by_source(
            &config,
            "any_key",
            "any_source"
        ));
    }
}
