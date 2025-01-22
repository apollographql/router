use std::sync::Arc;
use std::time::Instant;

use fred::error::RedisError;
use fred::types::Scanner;
use futures::stream;
use futures::StreamExt;
use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use thiserror::Error;
use tokio::sync::Semaphore;
use tower::BoxError;
use tracing::Instrument;

use super::entity::Storage as EntityStorage;
use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::plugins::cache::entity::hash_entity_key;
use crate::plugins::cache::entity::ENTITY_CACHE_VERSION;

#[derive(Clone)]
pub(crate) struct Invalidation {
    pub(crate) storage: Arc<EntityStorage>,
    pub(crate) scan_count: u32,
    pub(crate) semaphore: Arc<Semaphore>,
}

#[derive(Error, Debug, Clone)]
pub(crate) enum InvalidationError {
    #[error("redis error")]
    RedisError(#[from] RedisError),
    #[error("several errors")]
    Errors(#[from] InvalidationErrors),
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum InvalidationOrigin {
    Endpoint,
    Extensions,
}

impl Invalidation {
    pub(crate) async fn new(
        storage: Arc<EntityStorage>,
        scan_count: u32,
        concurrent_requests: u32,
    ) -> Result<Self, BoxError> {
        Ok(Self {
            storage,
            scan_count,
            semaphore: Arc::new(Semaphore::new(concurrent_requests as usize)),
        })
    }

    pub(crate) async fn invalidate(
        &self,
        origin: InvalidationOrigin,
        requests: Vec<InvalidationRequest>,
    ) -> Result<u64, BoxError> {
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

        Ok(self
            .handle_request_batch(origin, requests)
            .instrument(tracing::info_span!(
                "cache.invalidation.batch",
                "origin" = origin
            ))
            .await?)
    }

    async fn handle_request(
        &self,
        redis_storage: &RedisCacheStorage,
        origin: &'static str,
        request: &InvalidationRequest,
    ) -> Result<u64, InvalidationError> {
        let key_prefix = request.key_prefix();
        let subgraph = request.subgraph_name();
        tracing::debug!(
            "got invalidation request: {request:?}, will scan for: {}",
            key_prefix
        );

        let mut stream = redis_storage.scan(key_prefix.clone(), Some(self.scan_count));
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
                            let deleted = redis_storage.delete(keys).await.unwrap_or(0) as u64;
                            count += deleted;
                        }
                    }
                    scan_res.next()?;
                }
            }
        }

        u64_counter!(
            "apollo.router.operations.entity.invalidation.entry",
            "Entity cache counter for invalidated entries",
            count,
            "origin" = origin,
            "subgraph.name" = subgraph.clone()
        );

        u64_histogram!(
            "apollo.router.cache.invalidation.keys",
            "Number of invalidated keys per invalidation request.",
            count
        );

        match error {
            Some(err) => Err(err.into()),
            None => Ok(count),
        }
    }

    async fn handle_request_batch(
        &self,
        origin: &'static str,
        requests: Vec<InvalidationRequest>,
    ) -> Result<u64, InvalidationError> {
        let mut count = 0;
        let mut errors = Vec::new();
        let mut futures = Vec::new();
        for request in requests {
            let redis_storage = match self.storage.get(request.subgraph_name()) {
                Some(s) => s,
                None => continue,
            };

            let semaphore = self.semaphore.clone();
            let f = async move {
                // limit the number of invalidation requests executing at any point in time
                let _ = semaphore.acquire().await;

                let start = Instant::now();

                let res = self
                    .handle_request(redis_storage, origin, &request)
                    .instrument(tracing::info_span!("cache.invalidation.request"))
                    .await;

                f64_histogram!(
                    "apollo.router.cache.invalidation.duration",
                    "Duration of the invalidation event execution.",
                    start.elapsed().as_secs_f64()
                );
                res
            };
            futures.push(f);
        }
        let mut stream: stream::FuturesUnordered<_> = futures.into_iter().collect();
        while let Some(res) = stream.next().await {
            match res {
                Ok(c) => count += c,
                Err(err) => {
                    errors.push(err);
                }
            }
        }

        if !errors.is_empty() {
            Err(InvalidationErrors(errors).into())
        } else {
            Ok(count)
        }
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

    pub(super) fn kind(&self) -> &'static str {
        match self {
            InvalidationRequest::Subgraph { .. } => "subgraph",
            InvalidationRequest::Type { .. } => "type",
            InvalidationRequest::Entity { .. } => "entity",
        }
    }
}
