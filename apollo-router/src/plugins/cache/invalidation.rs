use std::sync::Arc;
use std::time::Instant;

use fred::error::RedisError;
use fred::types::Scanner;
use futures::SinkExt;
use futures::StreamExt;
use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use thiserror::Error;
use tokio::sync::broadcast;
use tower::BoxError;
use tracing::Instrument;

use super::entity::Storage as EntityStorage;
use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::notification::Handle;
use crate::notification::HandleStream;
use crate::plugins::cache::entity::hash_entity_key;
use crate::plugins::cache::entity::ENTITY_CACHE_VERSION;
use crate::Notify;

#[derive(Clone)]
pub(crate) struct Invalidation {
    #[allow(clippy::type_complexity)]
    pub(super) handle: Handle<
        InvalidationTopic,
        (
            Vec<InvalidationRequest>,
            InvalidationOrigin,
            broadcast::Sender<Result<u64, InvalidationError>>,
        ),
    >,
}

#[derive(Error, Debug, Clone)]
pub(crate) enum InvalidationError {
    #[error("redis error")]
    RedisError(#[from] RedisError),
    #[error("several errors")]
    Errors(#[from] InvalidationErrors),
    #[cfg(test)]
    #[error("custom error: {0}")]
    Custom(String),
}

#[derive(Debug, Clone)]
pub(crate) struct InvalidationErrors(Vec<InvalidationError>);

impl std::fmt::Display for InvalidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalidation errors: [{}]",
            self.0.iter().map(|e| e.to_string()).join("; ")
        )
    }
}

impl std::error::Error for InvalidationErrors {}

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub(crate) struct InvalidationTopic;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum InvalidationOrigin {
    Endpoint,
    Extensions,
}

impl Invalidation {
    pub(crate) async fn new(storage: Arc<EntityStorage>) -> Result<Self, BoxError> {
        let mut notify = Notify::new(None, None, None);
        let (handle, _b) = notify.create_or_subscribe(InvalidationTopic, false).await?;

        let h = handle.clone();

        tokio::task::spawn(async move {
            start(storage, h.into_stream()).await;
        });
        Ok(Self { handle })
    }

    pub(crate) async fn invalidate(
        &mut self,
        origin: InvalidationOrigin,
        requests: Vec<InvalidationRequest>,
    ) -> Result<u64, BoxError> {
        tracing::info!("invalidation endpoint got: {requests:?}");
        let mut sink = self.handle.clone().into_sink();
        let (response_tx, mut response_rx) = broadcast::channel(2);
        sink.send((requests, origin, response_tx.clone()))
            .await
            .map_err(|e| format!("cannot send invalidation request: {}", e.message))?;

        let result = response_rx
            .recv()
            .await
            .map_err(|err| {
                format!(
                    "cannot receive response for invalidation request: {:?}",
                    err
                )
            })?
            .map_err(|err| format!("received an invalidation error: {:?}", err))?;

        Ok(result)
    }
}

// TODO refactor
#[allow(clippy::type_complexity)]
async fn start(
    storage: Arc<EntityStorage>,
    mut handle: HandleStream<
        InvalidationTopic,
        (
            Vec<InvalidationRequest>,
            InvalidationOrigin,
            broadcast::Sender<Result<u64, InvalidationError>>,
        ),
    >,
) {
    while let Some((requests, origin, response_tx)) = handle.next().await {
        let origin = match origin {
            InvalidationOrigin::Endpoint => "endpoint",
            InvalidationOrigin::Extensions => "extensions",
        };
        u64_counter!(
            "apollo.router.operations.entity.invalidation.event",
            "Entity cache received a batch of invalidation requests",
            1u64,
            "origin" = origin
        );

        if let Err(err) = response_tx.send(
            handle_request_batch(&storage, origin, requests)
                .instrument(tracing::info_span!(
                    "cache.invalidation.batch",
                    "origin" = origin
                ))
                .await,
        ) {
            ::tracing::error!("cannot send answer to invalidation request in the channel: {err}");
        }
    }
}

async fn handle_request(
    storage: &RedisCacheStorage,
    origin: &'static str,
    request: &InvalidationRequest,
) -> Result<u64, InvalidationError> {
    let key_prefix = request.key_prefix();
    let subgraph = request.subgraph_name();
    tracing::info!(
        "got invalidation request: {request:?}, will scan for: {}",
        key_prefix
    );

    // FIXME: configurable batch size
    let mut stream = storage.scan(key_prefix.clone(), Some(1000));
    let mut count = 0u64;
    let mut error = None;

    while let Some(res) = stream.next().await {
        match res {
            Err(e) => {
                tracing::error!(
                    pattern = key_prefix,
                    error = %e,
                    message = "error scanning for key",
                );
                error = Some(e);
                break;
            }
            Ok(scan_res) => {
                if let Some(keys) = scan_res.results() {
                    let keys = keys
                        .iter()
                        .filter_map(|k| k.as_str())
                        .map(|k| RedisKey(k.to_string()))
                        .collect::<Vec<_>>();
                    if !keys.is_empty() {
                        tracing::info!("deleting keys: {keys:?}");
                        count += keys.len() as u64;
                        storage.delete(keys).await;

                        tracing::info!("deleted keys");
                        u64_counter!(
                            "apollo.router.operations.entity.invalidation.entry",
                            "Entity cache counter for invalidated entries",
                            1u64,
                            "origin" = origin,
                            "subgraph.name" = subgraph.clone()
                        );
                    } else {
                        tracing::info!("scanning did not find keys");
                    }
                }
            }
        }
    }

    u64_histogram!(
        "apollo.router.cache.invalidation.keys",
        "Number of invalidated keys.",
        count
    );

    match error {
        Some(err) => Err(err.into()),
        None => Ok(count),
    }
}

async fn handle_request_batch(
    storage: &EntityStorage,
    origin: &'static str,
    requests: Vec<InvalidationRequest>,
) -> Result<u64, InvalidationError> {
    tracing::info!("handle_request_batch got: {requests:?}");

    let mut count = 0;
    let mut errors = Vec::new();
    for request in requests {
        let start = Instant::now();
        let redis_storage = match storage.get(request.subgraph_name()) {
            Some(s) => s,
            None => continue,
        };
        match handle_request(redis_storage, origin, &request)
            .instrument(tracing::info_span!("cache.invalidation.request"))
            .await
        {
            Ok(c) => count += c,
            Err(err) => {
                errors.push(err);
            }
        }
        f64_histogram!(
            "apollo.router.cache.invalidation.duration",
            "Duration of the invalidation event execution.",
            start.elapsed().as_secs_f64()
        );
    }

    if !errors.is_empty() {
        Err(InvalidationErrors(errors).into())
    } else {
        Ok(count)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum InvalidationRequest {
    Subgraph {
        subgraph: String,
    },
    Type {
        subgraph: String,
        r#type: String,
    },
    Entity {
        subgraph: String,
        r#type: String,
        key: Value,
    },
}

impl InvalidationRequest {
    fn key_prefix(&self) -> String {
        match self {
            InvalidationRequest::Subgraph { subgraph } => {
                format!("version:{ENTITY_CACHE_VERSION}:subgraph:{subgraph}:*",)
            }
            InvalidationRequest::Type { subgraph, r#type } => {
                format!("version:{ENTITY_CACHE_VERSION}:subgraph:{subgraph}:type:{type}:*",)
            }
            InvalidationRequest::Entity {
                subgraph,
                r#type,
                key,
            } => {
                let entity_key = hash_entity_key(key);
                format!("version:{ENTITY_CACHE_VERSION}:subgraph:{subgraph}:type:{type}:entity:{entity_key}:*")
            }
        }
    }

    pub(super) fn subgraph_name(&self) -> &String {
        match self {
            InvalidationRequest::Subgraph { subgraph }
            | InvalidationRequest::Type { subgraph, .. }
            | InvalidationRequest::Entity { subgraph, .. } => subgraph,
        }
    }
}
