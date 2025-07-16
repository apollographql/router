use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use futures::FutureExt;
use futures::StreamExt;
use futures::stream;
use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use thiserror::Error;
use tower::BoxError;
use tracing::Instrument;

use super::plugin::Storage;
use super::postgres::PostgresCacheStorage;
use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::plugin::RESPONSE_CACHE_VERSION;
use crate::plugins::response_cache::plugin::hash_entity_key;

#[derive(Clone)]
pub(crate) struct Invalidation {
    pub(crate) storage: Arc<Storage>,
}

#[derive(Error, Debug)]
pub(crate) enum InvalidationError {
    #[error("error")]
    Misc(#[from] anyhow::Error),
    #[error("caching database error")]
    Postgres(#[from] sqlx::Error),
    #[error("several errors")]
    Errors(#[from] InvalidationErrors),
}

impl ErrorCode for InvalidationError {
    fn code(&self) -> &'static str {
        match &self {
            InvalidationError::Misc(_) => "MISC",
            InvalidationError::Postgres(error) => error.code(),
            InvalidationError::Errors(_) => "INVALIDATION_ERRORS",
        }
    }
}

#[derive(Debug)]
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

impl Invalidation {
    pub(crate) async fn new(storage: Arc<Storage>) -> Result<Self, BoxError> {
        Ok(Self { storage })
    }

    pub(crate) async fn invalidate(
        &self,
        requests: Vec<InvalidationRequest>,
    ) -> Result<u64, BoxError> {
        u64_counter!(
            "apollo.router.operations.response_cache.invalidation.event",
            "Response cache received a batch of invalidation requests",
            1u64
        );

        Ok(self
            .handle_request_batch(requests)
            .instrument(tracing::info_span!("cache.invalidation.batch"))
            .await?)
    }

    async fn handle_request(
        &self,
        pg_storage: &PostgresCacheStorage,
        request: &mut InvalidationRequest,
    ) -> Result<u64, InvalidationError> {
        let invalidation_key = request.invalidation_key();
        tracing::debug!(
            "got invalidation request: {request:?}, will invalidate: {}",
            invalidation_key
        );
        let count = match request {
            InvalidationRequest::Subgraph { subgraph } => {
                let count = pg_storage
                    .invalidate_by_subgraphs(vec![subgraph.clone()])
                    .await?;
                u64_counter!(
                    "apollo.router.operations.response_cache.invalidation.entry",
                    "Response cache counter for invalidated entries",
                    count,
                    "subgraph.name" = subgraph.clone()
                );
                count
            }
            InvalidationRequest::Entity { subgraph, .. }
            | InvalidationRequest::Type { subgraph, .. } => {
                let count = pg_storage
                    .invalidate(vec![invalidation_key], vec![subgraph.clone()])
                    .await?;

                u64_counter!(
                    "apollo.router.operations.response_cache.invalidation.entry",
                    "Response cache counter for invalidated entries",
                    count,
                    "subgraph.name" = subgraph.clone()
                );
                count
            }
            InvalidationRequest::CacheTag {
                subgraphs,
                cache_tag,
            } => {
                pg_storage
                    .invalidate(
                        vec![cache_tag.clone()],
                        subgraphs.clone().into_iter().collect(),
                    )
                    .await?
                // TODO: fixme
                // u64_counter!(
                //     "apollo.router.operations.response_cache.invalidation.entry",
                //     "Response cache counter for invalidated entries",
                //     count,
                //     "origin" = origin,
                //     "subgraph.name" = subgraphs.clone()
                // );
            }
        };

        u64_histogram!(
            "apollo.router.operations.response_cache.invalidation.keys",
            "Number of invalidated keys per invalidation request.",
            count
        );

        Ok(count)
    }

    async fn handle_request_batch(
        &self,
        requests: Vec<InvalidationRequest>,
    ) -> Result<u64, InvalidationError> {
        let mut count = 0;
        let mut errors = Vec::new();
        let mut futures = Vec::new();
        for request in requests {
            let storages = match &request {
                InvalidationRequest::Subgraph { subgraph }
                | InvalidationRequest::Type { subgraph, .. }
                | InvalidationRequest::Entity { subgraph, .. } => {
                    match self.storage.get(subgraph) {
                        Some(s) => vec![s],
                        None => continue,
                    }
                }
                InvalidationRequest::CacheTag { subgraphs, .. } => subgraphs
                    .iter()
                    .filter_map(|subgraph| self.storage.get(subgraph))
                    .collect(),
            };

            for pg_storage in storages {
                let mut request = request.clone();
                let f = async move {
                    let start = Instant::now();

                    let res = self
                        .handle_request(pg_storage, &mut request)
                        .instrument(tracing::info_span!("cache.invalidation.request"))
                        .await;

                    f64_histogram!(
                        "apollo.router.operations.response_cache.invalidation.duration",
                        "Duration of the invalidation event execution, in seconds.",
                        start.elapsed().as_secs_f64()
                    );
                    if let Err(err) = &res {
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code()
                        );
                    }
                    res
                };
                futures.push(f.boxed());
            }
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
#[serde(tag = "kind", rename_all = "snake_case")]
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
        key: serde_json_bytes::Map<ByteString, Value>,
    },
    CacheTag {
        subgraphs: HashSet<String>,
        cache_tag: String,
    },
}

impl InvalidationRequest {
    pub(crate) fn subgraph_names(&self) -> Vec<String> {
        match self {
            InvalidationRequest::Subgraph { subgraph }
            | InvalidationRequest::Type { subgraph, .. }
            | InvalidationRequest::Entity { subgraph, .. } => vec![subgraph.clone()],
            InvalidationRequest::CacheTag { subgraphs, .. } => {
                subgraphs.clone().into_iter().collect()
            }
        }
    }
    fn invalidation_key(&self) -> String {
        match self {
            InvalidationRequest::Subgraph { subgraph } => {
                format!("version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph}",)
            }
            InvalidationRequest::Type { subgraph, r#type } => {
                format!("version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph}:type:{type}",)
            }
            InvalidationRequest::Entity {
                subgraph,
                r#type,
                key,
            } => {
                let entity_key = hash_entity_key(key);
                format!(
                    "version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph}:type:{type}:entity:{entity_key}"
                )
            }
            InvalidationRequest::CacheTag { cache_tag, .. } => cache_tag.clone(),
        }
    }

    pub(super) fn kind(&self) -> &'static str {
        match self {
            InvalidationRequest::Subgraph { .. } => "subgraph",
            InvalidationRequest::Type { .. } => "type",
            InvalidationRequest::Entity { .. } => "entity",
            InvalidationRequest::CacheTag { .. } => "cache_tag",
        }
    }
}
