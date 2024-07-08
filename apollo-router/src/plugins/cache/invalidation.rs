use std::time::Instant;

use fred::types::Scanner;
use futures::SinkExt;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tower::BoxError;
use tracing::Instrument;

use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::notification::Handle;
use crate::notification::HandleStream;
use crate::Notify;

#[derive(Clone)]
pub(crate) struct Invalidation {
    enabled: bool,
    handle: Handle<InvalidationTopic, (InvalidationOrigin, Vec<InvalidationRequest>)>,
}

#[derive(Copy, Clone, Hash, PartialEq, Eq)]
pub(crate) struct InvalidationTopic;

#[derive(Clone, Debug)]
pub(crate) enum InvalidationOrigin {
    Endpoint,
    Extensions,
}

impl Invalidation {
    pub(crate) async fn new(storage: Option<RedisCacheStorage>) -> Result<Self, BoxError> {
        let mut notify = Notify::new(None, None, None);
        let (handle, _b) = notify.create_or_subscribe(InvalidationTopic, false).await?;
        let enabled = storage.is_some();
        if let Some(storage) = storage {
            let h = handle.clone();

            tokio::task::spawn(async move { start(storage, h.into_stream()).await });
        }
        Ok(Self { enabled, handle })
    }

    pub(crate) async fn invalidate(
        &mut self,
        origin: InvalidationOrigin,
        requests: Vec<InvalidationRequest>,
    ) -> Result<(), BoxError> {
        if self.enabled {
            let mut sink = self.handle.clone().into_sink();
            sink.send((origin, requests)).await.map_err(|e| e.message)?;
        }

        Ok(())
    }
}

async fn start(
    storage: RedisCacheStorage,
    mut handle: HandleStream<InvalidationTopic, (InvalidationOrigin, Vec<InvalidationRequest>)>,
) {
    while let Some((origin, requests)) = handle.next().await {
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
        handle_request_batch(&storage, origin, requests)
            .instrument(tracing::info_span!(
                "cache.invalidation.batch",
                "origin" = origin
            ))
            .await
    }
}

async fn handle_request_batch(
    storage: &RedisCacheStorage,
    origin: &'static str,
    requests: Vec<InvalidationRequest>,
) {
    for request in requests {
        let start = Instant::now();
        handle_request(storage, origin, &request)
            .instrument(tracing::info_span!("cache.invalidation.request"))
            .await;
        f64_histogram!(
            "apollo.router.cache.invalidation.duration",
            "Duration of the invalidation event execution.",
            start.elapsed().as_secs_f64()
        );
    }
}

async fn handle_request(
    storage: &RedisCacheStorage,
    origin: &'static str,
    request: &InvalidationRequest,
) {
    let key_prefix = request.key_prefix();
    let subgraph = request.subgraph();
    tracing::debug!(
        "got invalidation request: {request:?}, will scan for: {}",
        key_prefix
    );

    // FIXME: configurable batch size
    let mut stream = storage.scan(key_prefix.clone(), Some(10));
    let mut count = 0u64;

    while let Some(res) = stream.next().await {
        match res {
            Err(e) => {
                tracing::error!(
                    pattern = key_prefix,
                    error = %e,
                    message = "error scanning for key",
                );
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
                        tracing::debug!("deleting keys: {keys:?}");
                        count += keys.len() as u64;
                        storage.delete(keys).await;

                        u64_counter!(
                            "apollo.router.operations.entity.invalidation.entry",
                            "Entity cache counter for invalidated entries",
                            1u64,
                            "origin" = origin,
                            "subgraph.name" = subgraph.clone()
                        );
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
                format!("subgraph:{subgraph}*",)
            }
            _ => {
                todo!()
            }
        }
    }

    fn subgraph(&self) -> String {
        match self {
            InvalidationRequest::Subgraph { subgraph } => subgraph.clone(),
            _ => {
                todo!()
            }
        }
    }
}
