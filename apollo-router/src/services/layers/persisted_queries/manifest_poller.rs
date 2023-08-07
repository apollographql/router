//!  Persisted query manifest poller. Once created, will poll for updates continuously, reading persisted queries into memory.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::RwLock;

use anyhow::anyhow;
use futures::prelude::*;
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tower::BoxError;

use crate::uplink::persisted_queries_manifest_stream::MaybePersistedQueriesManifestChunks;
use crate::uplink::persisted_queries_manifest_stream::PersistedQueriesManifestChunk;
use crate::uplink::persisted_queries_manifest_stream::PersistedQueriesManifestQuery;
use crate::uplink::stream_from_uplink;
use crate::uplink::UplinkConfig;

/// An in memory cache of persisted queries.
pub(crate) type PersistedQueryManifest = HashMap<String, String>;

/// An in memory cache of persisted query bodies.
pub(crate) type PersistedQuerySet = HashSet<String>;

/// Manages polling uplink for persisted query chunks and unpacking those chunks into a [`PersistedQueryManifest`].
#[derive(Debug)]
pub(crate) struct PersistedQueryManifestPoller {
    persisted_query_manifest: Arc<RwLock<PersistedQueryManifest>>,
    persisted_query_bodies: Arc<RwLock<PersistedQuerySet>>,
    shutdown_sender: mpsc::Sender<()>,
}

impl PersistedQueryManifestPoller {
    /// Create a new [`PersistedQueryManifestPoller`] from CLI options and YAML configuration.
    /// Starts polling immediately and this function only returns after all chunks have been fetched
    /// and the [`PersistedQueryManifest`] has been fully populated.
    pub(crate) async fn new(uplink_config: &UplinkConfig) -> Result<Self, BoxError> {
        let persisted_query_manifest = Arc::new(RwLock::new(PersistedQueryManifest::new()));
        let persisted_query_bodies = Arc::new(RwLock::new(PersistedQuerySet::new()));

        let http_client = Client::builder().timeout(uplink_config.timeout).build()
            .map_err(|e| {
                anyhow!(
                    "could not initialize HTTP client for fetching persisted queries manifest chunks: {}",
                    e
                )
            })?;

        let (shutdown_sender, shutdown_receiver) = mpsc::channel::<()>(1);
        let (ready_sender, mut ready_receiver) = mpsc::channel::<ManifestPollResultOnStartup>(1);

        // start polling uplink for persisted query chunks
        tokio::task::spawn(poll_uplink(
            uplink_config.clone(),
            persisted_query_manifest.clone(),
            persisted_query_bodies.clone(),
            ready_sender,
            shutdown_receiver,
            http_client,
        ));

        // wait for the uplink poller to report its first success and continue
        // or report the error
        match ready_receiver.recv().await {
            Some(startup_result) => match startup_result {
                ManifestPollResultOnStartup::LoadedOperations => (),
                ManifestPollResultOnStartup::Err(error) => return Err(error),
            },
            None => {
                return Err(
                    anyhow!("could not receive ready event for persisted query layer").into(),
                );
            }
        }

        Ok(Self {
            shutdown_sender,
            persisted_query_manifest,
            persisted_query_bodies,
        })
    }

    /// Send a shutdown message to the background task that is polling for persisted query chunks.
    pub(crate) async fn shutdown(&self) -> Result<(), BoxError> {
        self.shutdown_sender
            .send(())
            .await
            .map_err(|_| anyhow!("could not send shutdown event in persisted query layer").into())
    }

    pub(crate) fn get_operation_body(&self, persisted_query_id: &str) -> Option<String> {
        let persisted_query_manifest = self
            .persisted_query_manifest
            .read()
            .expect("could not acquire read lock on persisted query manifest");
        persisted_query_manifest.get(persisted_query_id).cloned()
    }

    pub(crate) fn is_operation_persisted(&self, query: &str) -> bool {
        let persisted_query_bodies = self
            .persisted_query_bodies
            .read()
            .expect("could not acquire read lock on persisted query body set");
        persisted_query_bodies.contains(query)
    }
}

async fn poll_uplink(
    uplink_config: UplinkConfig,
    existing_persisted_query_manifest: Arc<RwLock<PersistedQueryManifest>>,
    existing_persisted_query_bodies: Arc<RwLock<PersistedQuerySet>>,
    ready_sender: mpsc::Sender<ManifestPollResultOnStartup>,
    mut shutdown_event_receiver: mpsc::Receiver<()>,
    http_client: Client,
) {
    let mut uplink_executor = stream::select_all(vec![
        stream_from_uplink::<PersistedQueriesManifestQuery, MaybePersistedQueriesManifestChunks>(
            uplink_config.clone(),
        )
        .filter_map(|res| {
            let http_client = http_client.clone();
            let graph_ref = uplink_config.apollo_graph_ref.clone();
            async move {
                match res {
                    Ok(Some(chunks)) => match manifest_from_chunks(chunks, http_client).await {
                        Ok(new_manifest) => Some(ManifestPollEvent::NewManifest(new_manifest)),
                        Err(e) => Some(ManifestPollEvent::FetchError(e)),
                    },
                    Ok(None) => Some(ManifestPollEvent::NoPersistedQueryList { graph_ref }),
                    Err(e) => Some(ManifestPollEvent::Err(e.into())),
                }
            }
        })
        .boxed(),
        shutdown_event_receiver
            .recv()
            .into_stream()
            .filter_map(|res| {
                future::ready(match res {
                    Some(()) => Some(ManifestPollEvent::Shutdown),
                    None => Some(ManifestPollEvent::Err(
                        anyhow!("could not receive shutdown event for persisted query layer")
                            .into(),
                    )),
                })
            })
            .boxed(),
    ])
    .take_while(|msg| future::ready(!matches!(msg, ManifestPollEvent::Shutdown)))
    .boxed();

    let mut resolved_first_pq_manifest = false;

    while let Some(event) = uplink_executor.next().await {
        match event {
            ManifestPollEvent::NewManifest(new_manifest) => {
                // copy all of the bodies in the new manifest into a hash set before acquiring any locks
                let new_bodies: HashSet<String> = new_manifest.values().cloned().collect();
                existing_persisted_query_manifest
                    .write()
                    .map(|mut existing_manifest| {
                        existing_persisted_query_bodies
                            .write()
                            .map(|mut existing_bodies| {
                                // update the existing map of pq id to pq body to be the new collection
                                *existing_manifest = new_manifest;

                                // update the set of pq bodies from the values in the new collection
                                *existing_bodies = new_bodies;
                            })
                            .expect("could not acquire write lock on persisted query body set");
                    })
                    .expect("could not acquire write lock on persisted query manifest");

                if !resolved_first_pq_manifest {
                    send_startup_event(
                        &ready_sender,
                        ManifestPollResultOnStartup::LoadedOperations,
                    )
                    .await;
                    resolved_first_pq_manifest = true;
                }
            }
            ManifestPollEvent::FetchError(e) => {
                send_startup_event(
                    &ready_sender,
                    ManifestPollResultOnStartup::Err(
                        anyhow!("could not fetch persisted queries: {e}").into(),
                    ),
                )
                .await
            }
            ManifestPollEvent::Err(e) => {
                send_startup_event(&ready_sender, ManifestPollResultOnStartup::Err(e)).await
            }
            ManifestPollEvent::NoPersistedQueryList { graph_ref } => {
                send_startup_event(
                    &ready_sender,
                    ManifestPollResultOnStartup::Err(
                        anyhow!("no persisted query list found for graph ref {}", &graph_ref)
                            .into(),
                    ),
                )
                .await
            }
            // this event is a no-op because we `take_while` on messages that are not this one
            ManifestPollEvent::Shutdown => (),
        }
    }

    async fn send_startup_event(
        ready_sender: &mpsc::Sender<ManifestPollResultOnStartup>,
        message: ManifestPollResultOnStartup,
    ) {
        if let Err(e) = ready_sender.send(message).await {
            tracing::debug!("could not send startup event for the persisted query layer: {e}");
        }
    }
}

async fn manifest_from_chunks(
    new_chunks: Vec<PersistedQueriesManifestChunk>,
    http_client: Client,
) -> Result<PersistedQueryManifest, BoxError> {
    let mut new_persisted_query_manifest = PersistedQueryManifest::new();
    tracing::debug!("ingesting new persisted queries: {:?}", &new_chunks);
    // TODO: consider doing these fetches in parallel
    for new_chunk in new_chunks {
        add_chunk_to_operations(
            new_chunk,
            &mut new_persisted_query_manifest,
            http_client.clone(),
        )
        .await?
    }

    tracing::info!(
        "Loaded {} persisted queries.",
        new_persisted_query_manifest.len()
    );

    Ok(new_persisted_query_manifest)
}

async fn add_chunk_to_operations(
    chunk: PersistedQueriesManifestChunk,
    operations: &mut PersistedQueryManifest,
    http_client: Client,
) -> anyhow::Result<()> {
    // TODO: chunk URLs will eventually respond with fallback URLs, when it does, implement falling back here
    if let Some(chunk_url) = chunk.urls.get(0) {
        let chunk = http_client
            .get(chunk_url.clone())
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| {
                anyhow!(
                    "error fetching persisted queries manifest chunk from {}: {}",
                    chunk_url,
                    e
                )
            })?
            .json::<SignedUrlChunk>()
            .await
            .map_err(|e| {
                anyhow!(
                    "error reading body of persisted queries manifest chunk from {}: {}",
                    chunk_url,
                    e
                )
            })?;

        if chunk.format != "apollo-persisted-query-manifest" {
            return Err(anyhow!(
                "chunk format is not 'apollo-persisted-query-manifest'"
            ));
        }

        if chunk.version != 1 {
            return Err(anyhow!("persisted query manifest chunk version is not 1"));
        }

        for operation in chunk.operations {
            operations.insert(operation.id, operation.body);
        }

        Ok(())
    } else {
        Err(anyhow!(
            "persisted query chunk did not include any URLs to fetch operations from"
        ))
    }
}

/// Types of events produced by the manifest poller.
#[derive(Debug)]
pub(crate) enum ManifestPollEvent {
    NewManifest(PersistedQueryManifest),
    NoPersistedQueryList { graph_ref: String },
    Err(BoxError),
    FetchError(BoxError),
    Shutdown,
}

/// The result of the first time build of the persisted query manifest.
#[derive(Debug)]
pub(crate) enum ManifestPollResultOnStartup {
    LoadedOperations,
    Err(BoxError),
}

/// The format of each persisted query chunk returned from uplink.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SignedUrlChunk {
    pub(crate) format: String,
    pub(crate) version: u64,
    pub(crate) operations: Vec<Operation>,
}

/// A single operation containing an ID and a body,
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Operation {
    pub(crate) id: String,
    pub(crate) body: String,
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;
    use crate::test_harness::mocks::persisted_queries::*;
    use crate::uplink::Endpoints;

    #[tokio::test(flavor = "multi_thread")]
    async fn poller_can_get_operation_bodies() {
        let (id, body, manifest) = fake_manifest();
        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;
        let manifest_manager = PersistedQueryManifestPoller::new(&uplink_config)
            .await
            .unwrap();
        assert_eq!(manifest_manager.get_operation_body(&id), Some(body))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poller_wont_start_without_uplink_connection() {
        let uplink_endpoint = Url::parse("https://definitely.not.uplink").unwrap();
        assert!(
            PersistedQueryManifestPoller::new(&UplinkConfig::for_tests(Endpoints::fallback(vec![
                uplink_endpoint
            ])))
            .await
            .is_err()
        );
    }
}
