use std::sync::Arc;

use axum::Extension;
use axum::Router;
use axum::body::Body;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::any;
use bytes::Buf;
use http::Method;
use http::StatusCode;
use http::header::AUTHORIZATION;
use http::header::CONTENT_TYPE;
use mime::APPLICATION_JSON;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use tracing::Span;
use tracing_futures::Instrument;

use super::entity::Subgraph;
use super::invalidation::Invalidation;
use super::invalidation::InvalidationOrigin;
use crate::ListenAddr;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::plugins::cache::invalidation::InvalidationRequest;
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
    #[serde(default = "default_scan_count")]
    /// Number of keys to return at once from a redis SCAN command
    pub(crate) scan_count: u32,
    #[serde(default = "concurrent_requests_count")]
    /// Number of concurrent invalidation requests
    pub(crate) concurrent_requests: u32,
}

fn default_scan_count() -> u32 {
    1000
}

fn concurrent_requests_count() -> u32 {
    10
}

#[derive(Clone)]
pub(crate) struct InvalidationState {
    config: Arc<SubgraphConfiguration<Subgraph>>,
    invalidation: Invalidation,
}

pub(crate) fn invalidation_router(
    config: Arc<SubgraphConfiguration<Subgraph>>,
    invalidation: Invalidation,
) -> Router {
    let state = InvalidationState {
        config,
        invalidation,
    };
    Router::new()
        .route("/", any(handle_invalidation))
        .layer(Extension(state))
}

async fn handle_invalidation(
    Extension(state): Extension<InvalidationState>,
    req: http::Request<Body>,
) -> Response {
    handle_invalidation_inner(state, req)
        .instrument(tracing::info_span!(
            INVALIDATION_ENDPOINT_SPAN_NAME,
            "invalidation.request.kinds" = ::tracing::field::Empty,
            "otel.status_code" = OTEL_STATUS_CODE_OK,
        ))
        .await
}

async fn handle_invalidation_inner(state: InvalidationState, req: http::Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    if !parts.headers.contains_key(AUTHORIZATION) {
        Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
        return error_response(StatusCode::UNAUTHORIZED, "Missing authorization header");
    }
    match parts.method {
        Method::POST => {
            let body = router::body::into_bytes(body)
                .instrument(tracing::info_span!("into_bytes"))
                .await
                .map_err(|e| format!("failed to get the request body: {e}"))
                .and_then(|bytes| {
                    serde_json::from_reader::<_, Vec<InvalidationRequest>>(bytes.reader()).map_err(
                        |err| format!("failed to deserialize the request body into JSON: {err}"),
                    )
                });
            let shared_key = match parts
                .headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
            {
                Some(key) => key.to_owned(),
                None => {
                    Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                    return error_response(
                        StatusCode::UNAUTHORIZED,
                        "Invalid authorization header encoding",
                    );
                }
            };
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
                            valid_shared_key(&state.config, &shared_key, subgraph_name)
                        });
                    if !valid_shared_key {
                        Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                        return error_response(
                            StatusCode::UNAUTHORIZED,
                            "Invalid authorization header",
                        );
                    }
                    match state
                        .invalidation
                        .invalidate(InvalidationOrigin::Endpoint, body)
                        .instrument(tracing::info_span!("invalidate"))
                        .await
                    {
                        Ok(count) => {
                            let body =
                                serde_json::to_string(&json!({"count": count})).unwrap_or_default();
                            (
                                StatusCode::ACCEPTED,
                                [(CONTENT_TYPE, APPLICATION_JSON.essence_str())],
                                body,
                            )
                                .into_response()
                        }
                        Err(err) => {
                            Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                            error_response(StatusCode::BAD_REQUEST, &err.to_string())
                        }
                    }
                }
                Err(err) => {
                    Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                    error_response(StatusCode::BAD_REQUEST, &err)
                }
            }
        }
        _ => {
            Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            error_response(StatusCode::METHOD_NOT_ALLOWED, "")
        }
    }
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::to_string(&serde_json::json!({
        "errors": [{
            "message": message,
            "extensions": {
                "code": status.to_string()
            }
        }]
    }))
    .unwrap_or_default();
    (
        status,
        [(CONTENT_TYPE, APPLICATION_JSON.essence_str())],
        body,
    )
        .into_response()
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

    use axum::body::Body;
    use http::header::AUTHORIZATION;
    use tower::Service;
    use tower::ServiceExt;

    use super::*;
    use crate::cache::redis::RedisCacheStorage;
    use crate::plugins::cache::entity::Storage;
    use crate::plugins::cache::tests::MockStore;

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key() {
        let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
            .await
            .unwrap();
        let storage = Arc::new(Storage {
            all: Some(redis_cache),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone(), 1000, 10).await.unwrap();

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
        let mut router = invalidation_router(config, invalidation);
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri("/")
            .header(AUTHORIZATION, "testttt")
            .body(Body::from(
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
            ))
            .unwrap();
        let res = router
            .as_service()
            .ready()
            .await
            .unwrap()
            .call(req)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key_subgraph() {
        let redis_cache = RedisCacheStorage::from_mocks(Arc::new(MockStore::new()))
            .await
            .unwrap();
        let storage = Arc::new(Storage {
            all: Some(redis_cache),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone(), 1000, 10).await.unwrap();

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
        let mut router = invalidation_router(config, invalidation);
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri("/")
            .header(AUTHORIZATION, "test_test")
            .body(Body::from(
                serde_json::to_vec(&[InvalidationRequest::Subgraph {
                    subgraph: String::from("foo"),
                }])
                .unwrap(),
            ))
            .unwrap();
        let res = router
            .as_service()
            .ready()
            .await
            .unwrap()
            .call(req)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
