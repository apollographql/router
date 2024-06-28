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
    handle: Handle<InvalidationTopic, Vec<InvalidationRequest>>,
}

#[derive(Copy, Clone, Hash, PartialEq, Eq)]
pub(crate) struct InvalidationTopic;

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
        requests: Vec<InvalidationRequest>,
    ) -> Result<(), BoxError> {
        if self.enabled {
            let mut sink = self.handle.clone().into_sink();
            sink.send(requests).await.unwrap();
        }

        Ok(())
    }
}

async fn start(
    storage: RedisCacheStorage,
    mut handle: HandleStream<InvalidationTopic, Vec<InvalidationRequest>>,
) {
    while let Some(requests) = handle.next().await {
        handle_request_batch(&storage, requests)
            .instrument(tracing::info_span!("cache.invalidation.batch"))
            .await
    }
}

async fn handle_request_batch(storage: &RedisCacheStorage, requests: Vec<InvalidationRequest>) {
    for request in requests {
        handle_request(&storage, &request)
            .instrument(tracing::info_span!("cache.invalidation.request"))
            .await;
    }
}

async fn handle_request(storage: &RedisCacheStorage, request: &InvalidationRequest) {
    println!(
        "got invalidation request: {request:?}, will scan for: {}",
        request.key_prefix()
    );

    // FIXME: configurable batch size
    let mut stream = storage.scan(request.key_prefix(), Some(10));

    while let Some(res) = stream.next().await {
        println!("scan returned a result");
        match res {
            Err(e) => {
                tracing::error!(
                    pattern = request.key_prefix(),
                    error = %e,
                    message = "error scanning for key",
                );
                println!("error: {e}");
                break;
            }
            Ok(scan_res) => {
                println!("got scan result");
                if let Some(keys) = scan_res.results() {
                    let keys = keys
                        .iter()
                        .filter_map(|k| k.as_str())
                        .map(|k| RedisKey(k.to_string()))
                        .collect::<Vec<_>>();
                    println!("scanned keys: {keys:?}");
                    if !keys.is_empty() {
                        storage.delete(keys).await;
                    }
                }

                if !scan_res.has_more() {
                    println!("no more results in scan_res");
                    break;
                } else {
                    if let Err(e) = scan_res.next() {
                        tracing::error!(
                            pattern = request.key_prefix(),
                            error = %e,
                            message = "error scanning for key",
                        );
                        break;
                    }
                }
            }
        }
    }

    println!("handle_request end");
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
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
}
