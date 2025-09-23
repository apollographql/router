use std::collections::HashSet;
use std::sync::Arc;

use futures::FutureExt;
use futures::StreamExt;
use futures::stream;
use itertools::Itertools;
use opentelemetry::StringValue;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use tower::BoxError;
use tracing::Instrument;

use super::plugin::StorageInterface;
use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::plugin::INTERNAL_CACHE_TAG_PREFIX;
use crate::plugins::response_cache::plugin::RESPONSE_CACHE_VERSION;
use crate::plugins::response_cache::storage;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::postgres::Storage;

#[derive(Clone)]
pub(crate) struct Invalidation {
    pub(crate) storage: Arc<StorageInterface>,
}

#[derive(Error, Debug)]
pub(super) enum InvalidationError {
    #[error("error")]
    Misc(#[from] anyhow::Error),
    #[error("caching database error")]
    Storage(#[from] storage::Error),
    #[error("several errors")]
    Errors(#[from] InvalidationErrors),
}

impl ErrorCode for InvalidationError {
    fn code(&self) -> &'static str {
        match &self {
            InvalidationError::Misc(_) => "MISC",
            InvalidationError::Storage(error) => error.code(),
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
    pub(crate) async fn new(storage: Arc<StorageInterface>) -> Result<Self, BoxError> {
        Ok(Self { storage })
    }

    pub(crate) async fn invalidate(
        &self,
        requests: Vec<InvalidationRequest>,
    ) -> Result<u64, BoxError> {
        u64_counter_with_unit!(
            "apollo.router.operations.response_cache.invalidation.event",
            "Response cache received a batch of invalidation requests",
            "{request}",
            1u64
        );

        Ok(self
            .handle_request_batch(requests)
            .instrument(tracing::info_span!("cache.invalidation.batch"))
            .await?)
    }

    async fn handle_request(
        &self,
        storage: &Storage,
        request: &mut InvalidationRequest,
    ) -> Result<u64, InvalidationError> {
        let invalidation_key = request.invalidation_key();
        tracing::debug!(
            "got invalidation request: {request:?}, will invalidate: {}",
            invalidation_key
        );
        let (count, subgraphs) = match request {
            InvalidationRequest::Subgraph { subgraph } => {
                let count = storage
                    .invalidate_by_subgraphs(vec![subgraph.clone()])
                    .await
                    .inspect_err(|err| {
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code(),
                            "kind" = "subgraph",
                            "subgraph.name" = subgraph.clone()
                        );
                    })?;
                u64_counter_with_unit!(
                    "apollo.router.operations.response_cache.invalidation.entry",
                    "Response cache counter for invalidated entries",
                    "{entry}",
                    count,
                    "kind" = "subgraph",
                    "subgraph.name" = subgraph.clone()
                );
                (count, vec![subgraph.clone()])
            }
            InvalidationRequest::Type {
                subgraph,
                r#type: graphql_type,
            } => {
                let subgraph_counts = storage
                    .invalidate(vec![invalidation_key], vec![subgraph.clone()])
                    .await
                    .inspect_err(|err| {
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code(),
                            "kind" = "type",
                            "subgraph.name" = subgraph.clone(),
                            "graphql.type" = graphql_type.clone()
                        );
                    })?;
                let mut total_count = 0;
                for (subgraph_name, count) in subgraph_counts {
                    total_count += count;
                    u64_counter_with_unit!(
                        "apollo.router.operations.response_cache.invalidation.entry",
                        "Response cache counter for invalidated entries",
                        "{entry}",
                        count,
                        "kind" = "type",
                        "subgraph.name" = subgraph_name,
                        "graphql.type" = graphql_type.clone()
                    );
                }

                (total_count, vec![subgraph.clone()])
            }
            InvalidationRequest::CacheTag {
                subgraphs,
                cache_tag,
            } => {
                let subgraph_counts = storage
                    .invalidate(
                        vec![cache_tag.clone()],
                        subgraphs.clone().into_iter().collect(),
                    )
                    .await
                    .inspect_err(|err| {
                        let subgraphs: opentelemetry::Array = subgraphs
                            .clone()
                            .into_iter()
                            .map(StringValue::from)
                            .collect::<Vec<StringValue>>()
                            .into();
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code(),
                            "kind" = "cache_tag",
                            "subgraph.names" = opentelemetry::Value::Array(subgraphs),
                            "cache.tag" = cache_tag.clone()
                        );
                    })?;
                let mut total_count = 0;
                for (subgraph_name, count) in subgraph_counts {
                    total_count += count;
                    u64_counter_with_unit!(
                        "apollo.router.operations.response_cache.invalidation.entry",
                        "Response cache counter for invalidated entries",
                        "{entry}",
                        count,
                        "kind" = "cache_tag",
                        "subgraph.name" = subgraph_name,
                        "cache.tag" = cache_tag.clone()
                    );
                }

                (
                    total_count,
                    subgraphs.clone().into_iter().collect::<Vec<String>>(),
                )
            }
        };

        for subgraph in subgraphs {
            u64_histogram_with_unit!(
                "apollo.router.operations.response_cache.invalidation.request.entry",
                "Number of invalidated entries per invalidation request.",
                "{entry}",
                count,
                "subgraph.name" = subgraph
            );
        }

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
                | InvalidationRequest::Type { subgraph, .. } => match self.storage.get(subgraph) {
                    Some(s) => vec![s],
                    None => continue,
                },
                InvalidationRequest::CacheTag { subgraphs, .. } => subgraphs
                    .iter()
                    .filter_map(|subgraph| self.storage.get(subgraph))
                    .collect(),
            };

            for storage in storages {
                let mut request = request.clone();
                let f = async move {
                    self.handle_request(storage, &mut request)
                        .instrument(tracing::info_span!("cache.invalidation.request"))
                        .await
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
    CacheTag {
        subgraphs: HashSet<String>,
        cache_tag: String,
    },
}

impl InvalidationRequest {
    pub(crate) fn subgraph_names(&self) -> Vec<String> {
        match self {
            InvalidationRequest::Subgraph { subgraph }
            | InvalidationRequest::Type { subgraph, .. } => vec![subgraph.clone()],
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
                format!(
                    "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph}:type:{type}",
                )
            }
            InvalidationRequest::CacheTag { cache_tag, .. } => cache_tag.clone(),
        }
    }

    pub(super) fn kind(&self) -> &'static str {
        match self {
            InvalidationRequest::Subgraph { .. } => "subgraph",
            InvalidationRequest::Type { .. } => "type",
            InvalidationRequest::CacheTag { .. } => "cache_tag",
        }
    }
}
