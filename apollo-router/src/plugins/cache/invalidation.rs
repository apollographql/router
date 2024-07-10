use std::sync::Arc;
use std::time::Instant;

use fred::types::Scanner;
use futures::SinkExt;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tower::BoxError;
use tracing::Instrument;

use super::entity::Storage as EntityStorage;
use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::notification::Handle;
use crate::notification::HandleStream;
use crate::Notify;

#[derive(Clone)]
pub(crate) struct Invalidation {
    handle: Handle<InvalidationTopic, Vec<InvalidationRequest>>,
}

#[derive(Copy, Clone, Hash, PartialEq, Eq)]
pub(crate) struct InvalidationTopic;

impl Invalidation {
    pub(crate) async fn new(storage: Arc<EntityStorage>) -> Result<Self, BoxError> {
        let mut notify = Notify::new(None, None, None);
        let (handle, _b) = notify.create_or_subscribe(InvalidationTopic, false).await?;
        let h = handle.clone();

        tokio::task::spawn(async move { start(storage, h.into_stream()).await });

        Ok(Self { handle })
    }

    pub(crate) async fn invalidate(
        &mut self,
        requests: Vec<InvalidationRequest>,
    ) -> Result<(), BoxError> {
        let mut sink = self.handle.clone().into_sink();
        sink.send(requests).await.map_err(|e| e.message)?;

        Ok(())
    }
}

async fn start(
    storage: Arc<EntityStorage>,
    mut handle: HandleStream<InvalidationTopic, Vec<InvalidationRequest>>,
) {
    while let Some(requests) = handle.next().await {
        handle_request_batch(&storage, requests)
            .instrument(tracing::info_span!("cache.invalidation.batch"))
            .await
    }
}

async fn handle_request_batch(storage: &EntityStorage, requests: Vec<InvalidationRequest>) {
    for request in requests {
        let start = Instant::now();
        let redis_storage = match storage.get(request.subgraph()) {
            Some(s) => s,
            None => continue,
        };
        handle_request(redis_storage, &request)
            .instrument(tracing::info_span!("cache.invalidation.request"))
            .await;
        f64_histogram!(
            "apollo.router.cache.invalidation.duration",
            "Duration of the invalidation event execution.",
            start.elapsed().as_secs_f64()
        );
    }
}

async fn handle_request(storage: &RedisCacheStorage, request: &InvalidationRequest) {
    let key_prefix = request.key_prefix();
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
            InvalidationRequest::Type { subgraph, r#type } => {
                format!("subgraph:{subgraph}:type:{type}*",)
            }
            _ => {
                todo!()
            }
        }
    }

    fn subgraph(&self) -> &str {
        match self {
            InvalidationRequest::Subgraph { subgraph } => subgraph,
            InvalidationRequest::Type { subgraph, .. } => subgraph,
            InvalidationRequest::Entity { subgraph, .. } => subgraph,
        }
    }
}
