//!  Persisted query manifest poller. Once created, will poll for updates continuously, reading persisted queries into memory.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use apollo_compiler::ast;
use futures::prelude::*;
use parking_lot::RwLock;
use reqwest::Client;
use tokio::fs::read_to_string;
use tokio::sync::mpsc;
use tower::BoxError;

use super::freeform_graphql_behavior::FreeformGraphQLAction;
use super::freeform_graphql_behavior::FreeformGraphQLBehavior;
use super::freeform_graphql_behavior::get_freeform_graphql_behavior;
use super::manifest::FullPersistedQueryOperationId;
use super::manifest::PersistedQueryManifest;
use super::manifest::SignedUrlChunk;
use crate::Configuration;
use crate::uplink::UplinkConfig;
use crate::uplink::persisted_queries_manifest_stream::MaybePersistedQueriesManifestChunks;
use crate::uplink::persisted_queries_manifest_stream::PersistedQueriesManifestChunk;
use crate::uplink::persisted_queries_manifest_stream::PersistedQueriesManifestQuery;
use crate::uplink::stream_from_uplink_transforming_new_response;

#[derive(Debug)]
pub(crate) struct PersistedQueryManifestPollerState {
    persisted_query_manifest: PersistedQueryManifest,
    pub(crate) freeform_graphql_behavior: FreeformGraphQLBehavior,
}

/// Manages polling uplink for persisted query chunks and unpacking those chunks into a [`PersistedQueryManifest`].
#[derive(Debug)]
pub(crate) struct PersistedQueryManifestPoller {
    pub(crate) state: Arc<RwLock<PersistedQueryManifestPollerState>>,
    _drop_signal: mpsc::Sender<()>,
}

impl PersistedQueryManifestPoller {
    /// Create a new [`PersistedQueryManifestPoller`] from CLI options and YAML configuration.
    /// Starts polling immediately and this function only returns after all chunks have been fetched
    /// and the [`PersistedQueryManifest`] has been fully populated.
    pub(crate) async fn new(config: Configuration) -> Result<Self, BoxError> {
        let manifest_source = ManifestSource::from_config(&config)?;
        let manifest_stream = create_manifest_stream(manifest_source).await?;

        // Initialize state
        let state = Arc::new(RwLock::new(PersistedQueryManifestPollerState {
            persisted_query_manifest: PersistedQueryManifest::default(),
            freeform_graphql_behavior: FreeformGraphQLBehavior::DenyAll { log_unknown: false },
        }));

        // Start the background polling task
        let (_drop_signal, drop_receiver) = mpsc::channel::<()>(1);
        let (ready_sender, mut ready_receiver) = mpsc::channel::<ManifestPollResultOnStartup>(1);

        let state_clone = state.clone();
        let config_clone = config.clone();

        tokio::task::spawn(async move {
            poll_manifest_stream(
                manifest_stream,
                state_clone,
                config_clone,
                ready_sender,
                drop_receiver,
            )
            .await;
        });

        match ready_receiver.recv().await {
            Some(ManifestPollResultOnStartup::LoadedOperations) => Ok(Self {
                state,
                _drop_signal,
            }),
            Some(ManifestPollResultOnStartup::Err(e)) => Err(e),
            None => Err("could not receive ready event for persisted query layer".into()),
        }
    }

    pub(crate) fn get_operation_body(
        &self,
        persisted_query_id: &str,
        client_name: Option<String>,
    ) -> Option<String> {
        let state = self.state.read();
        if let Some(body) = state
            .persisted_query_manifest
            .get(&FullPersistedQueryOperationId {
                operation_id: persisted_query_id.to_string(),
                client_name: client_name.clone(),
            })
            .cloned()
        {
            Some(body)
        } else if client_name.is_some() {
            state
                .persisted_query_manifest
                .get(&FullPersistedQueryOperationId {
                    operation_id: persisted_query_id.to_string(),
                    client_name: None,
                })
                .cloned()
        } else {
            None
        }
    }

    pub(crate) fn get_all_operations(&self) -> Vec<String> {
        let state = self.state.read();
        state.persisted_query_manifest.values().cloned().collect()
    }

    pub(crate) fn action_for_freeform_graphql(
        &self,
        ast: Result<&ast::Document, &str>,
    ) -> FreeformGraphQLAction {
        let state = self.state.read();
        state
            .freeform_graphql_behavior
            .action_for_freeform_graphql(ast)
    }

    // Some(bool) means "never allows freeform GraphQL, bool is whether or not to log"
    // None means "sometimes allows freeform GraphQL"
    pub(crate) fn never_allows_freeform_graphql(&self) -> Option<bool> {
        let state = self.state.read();
        if let FreeformGraphQLBehavior::DenyAll { log_unknown } = state.freeform_graphql_behavior {
            Some(log_unknown)
        } else {
            None
        }
    }

    // The way we handle requests that contain an unknown ID or which contain
    // both an ID and its body depends on whether or not APQ is also enabled.
    // It's important that we never allow APQ to be enabled when we're running
    // in a safelisting mode; this function ensures that by only ever returning
    // true from non-safelisting modes.
    pub(crate) fn augmenting_apq_with_pre_registration_and_no_safelisting(&self) -> bool {
        let state = self.state.read();
        match state.freeform_graphql_behavior {
            FreeformGraphQLBehavior::AllowAll { apq_enabled, .. }
            | FreeformGraphQLBehavior::LogUnlessInSafelist { apq_enabled, .. } => apq_enabled,
            _ => false,
        }
    }
}

async fn manifest_from_uplink_chunks(
    new_chunks: Vec<PersistedQueriesManifestChunk>,
    http_client: Client,
) -> Result<PersistedQueryManifest, BoxError> {
    let mut new_persisted_query_manifest = PersistedQueryManifest::default();
    tracing::debug!("ingesting new persisted queries: {:?}", &new_chunks);
    // TODO: consider doing these fetches in parallel
    for new_chunk in new_chunks {
        fetch_chunk_into_manifest(
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

async fn fetch_chunk_into_manifest(
    chunk: PersistedQueriesManifestChunk,
    manifest: &mut PersistedQueryManifest,
    http_client: Client,
) -> Result<(), BoxError> {
    let mut it = chunk.urls.iter().peekable();
    while let Some(chunk_url) = it.next() {
        match fetch_chunk(http_client.clone(), chunk_url).await {
            Ok(chunk) => {
                manifest.add_chunk(&chunk);
                return Ok(());
            }
            Err(e) => {
                if it.peek().is_some() {
                    // There's another URL to try, so log as debug and move on.
                    tracing::debug!(
                        "failed to fetch persisted query list chunk from {}: {}. \
                         Other endpoints will be tried",
                        chunk_url,
                        e
                    );
                    continue;
                } else {
                    // No more URLs; fail the function.
                    return Err(e);
                }
            }
        }
    }
    // The loop always returns unless there's another iteration after it, so the
    // only way we can fall off the loop is if we never entered it.
    Err("persisted query chunk did not include any URLs to fetch operations from".into())
}

async fn fetch_chunk(http_client: Client, chunk_url: &String) -> Result<SignedUrlChunk, BoxError> {
    let chunk = http_client
        .get(chunk_url.clone())
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| -> BoxError {
            format!("error fetching persisted queries manifest chunk from {chunk_url}: {e}").into()
        })?
        .json::<SignedUrlChunk>()
        .await
        .map_err(|e| -> BoxError {
            format!("error reading body of persisted queries manifest chunk from {chunk_url}: {e}")
                .into()
        })?;

    chunk.validate()
}

/// The result of the first time build of the persisted query manifest.
#[derive(Debug)]
pub(crate) enum ManifestPollResultOnStartup {
    LoadedOperations,
    Err(BoxError),
}

/// The source of persisted query manifests
#[derive(Debug)]
enum ManifestSource {
    LocalStatic(Vec<String>),
    LocalHotReload(Vec<String>),
    Uplink(UplinkConfig),
}

impl ManifestSource {
    fn from_config(config: &Configuration) -> Result<Self, BoxError> {
        let source = if config.persisted_queries.hot_reload {
            if let Some(paths) = &config.persisted_queries.local_manifests {
                ManifestSource::LocalHotReload(paths.clone())
            } else {
                return Err("`persisted_queries.hot_reload` requires `local_manifests`".into());
            }
        } else if let Some(paths) = &config.persisted_queries.local_manifests {
            ManifestSource::LocalStatic(paths.clone())
        } else if let Some(uplink_config) = config.uplink.as_ref() {
            ManifestSource::Uplink(uplink_config.clone())
        } else {
            return Err(
                "persisted queries requires either local_manifests or Apollo GraphOS configuration"
                    .into(),
            );
        };

        Ok(source)
    }
}

/// A stream of manifest updates
type ManifestStream = dyn Stream<Item = Result<PersistedQueryManifest, BoxError>> + Send + 'static;

async fn create_manifest_stream(
    source: ManifestSource,
) -> Result<Pin<Box<ManifestStream>>, BoxError> {
    match source {
        ManifestSource::LocalStatic(paths) => Ok(stream::once(load_local_manifests(paths)).boxed()),
        ManifestSource::LocalHotReload(paths) => Ok(create_hot_reload_stream(paths).boxed()),
        ManifestSource::Uplink(uplink_config) => {
            let client = Client::builder()
                .timeout(uplink_config.timeout)
                .gzip(true)
                .build()?;
            Ok(create_uplink_stream(uplink_config, client).boxed())
        }
    }
}

async fn poll_manifest_stream(
    mut manifest_stream: Pin<Box<ManifestStream>>,
    state: Arc<RwLock<PersistedQueryManifestPollerState>>,
    config: Configuration,
    ready_sender: mpsc::Sender<ManifestPollResultOnStartup>,
    mut drop_receiver: mpsc::Receiver<()>,
) {
    let mut ready_sender = Some(ready_sender);

    loop {
        tokio::select! {
            manifest_result = manifest_stream.next() => {
                match manifest_result {
                    Some(Ok(new_manifest)) => {
                        let operation_count = new_manifest.len();
                        let freeform_graphql_behavior =
                            get_freeform_graphql_behavior(&config, &new_manifest);

                        *state.write() = PersistedQueryManifestPollerState {
                            persisted_query_manifest: new_manifest,
                            freeform_graphql_behavior,
                        };
                        tracing::info!("persisted query manifest successfully updated ({} operations total)", operation_count);

                        if let Some(sender) = ready_sender.take() {
                            let _ = sender.send(ManifestPollResultOnStartup::LoadedOperations).await;
                        }
                    }
                    Some(Err(e)) => {
                        if let Some(sender) = ready_sender.take() {
                            let _ = sender.send(ManifestPollResultOnStartup::Err(e)).await;
                        } else {
                            tracing::error!("Error polling manifest: {}", e);
                        }
                    }
                    None => break,
                }
            }
            _ = drop_receiver.recv() => break,
        }
    }
}

async fn load_local_manifests(paths: Vec<String>) -> Result<PersistedQueryManifest, BoxError> {
    let mut complete_manifest = PersistedQueryManifest::default();

    for path in paths.iter() {
        let raw_file_contents = read_to_string(path).await.map_err(|e| -> BoxError {
            format!("Failed to read persisted query list file at path: {path}, {e}").into()
        })?;

        let chunk = SignedUrlChunk::parse_and_validate(&raw_file_contents)?;
        complete_manifest.add_chunk(&chunk);
    }

    tracing::info!(
        "Loaded {} persisted queries from local files.",
        complete_manifest.len()
    );

    Ok(complete_manifest)
}

fn create_uplink_stream(
    uplink_config: UplinkConfig,
    http_client: Client,
) -> impl Stream<Item = Result<PersistedQueryManifest, BoxError>> {
    stream_from_uplink_transforming_new_response::<
        PersistedQueriesManifestQuery,
        MaybePersistedQueriesManifestChunks,
        Option<PersistedQueryManifest>,
    >(uplink_config, move |response| {
        let http_client = http_client.clone();
        Box::new(Box::pin(async move {
            match response {
                Some(chunks) => manifest_from_uplink_chunks(chunks, http_client)
                    .await
                    .map(Some)
                    .map_err(|e| -> BoxError { e }),
                None => Ok(None),
            }
        }))
    })
    .filter_map(|result| async move {
        match result {
            Ok(Some(manifest)) => Some(Ok(manifest)),
            Ok(None) => Some(Ok(PersistedQueryManifest::default())),
            Err(e) => Some(Err(e.into())),
        }
    })
}

fn create_hot_reload_stream(
    paths: Vec<String>,
) -> impl Stream<Item = Result<PersistedQueryManifest, BoxError>> {
    // Create file watchers for each path
    let file_watchers = paths.into_iter().map(|raw_path| {
        crate::files::watch(std::path::Path::new(&raw_path.clone())).then(move |_| {
            let raw_path = raw_path.clone();
            async move {
                match read_to_string(&raw_path).await {
                    Ok(raw_file_contents) => {
                        match SignedUrlChunk::parse_and_validate(&raw_file_contents) {
                            Ok(chunk) => Ok((raw_path, chunk)),
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(e.into()),
                }
            }
            .boxed()
        })
    });

    // We need to keep track of the local manifest chunks so we can replace them when
    // they change.
    let mut chunks: HashMap<String, SignedUrlChunk> = HashMap::new();

    // Combine all watchers into a single stream
    stream::select_all(file_watchers).map(move |result| {
        result.map(|(path, chunk)| {
            tracing::info!(
                "hot reloading persisted query manifest file at path: {}",
                path
            );
            chunks.insert(path, chunk);

            let mut manifest = PersistedQueryManifest::default();
            for chunk in chunks.values() {
                manifest.add_chunk(chunk);
            }

            manifest
        })
    })
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;
    use url::Url;

    use super::*;
    use crate::configuration::Apq;
    use crate::configuration::PersistedQueries;
    use crate::test_harness::mocks::persisted_queries::*;
    use crate::uplink::Endpoints;

    #[tokio::test(flavor = "multi_thread")]
    async fn poller_can_get_operation_bodies() {
        let (id, body, manifest) = fake_manifest();
        let (_mock_guard, uplink_config) = mock_pq_uplink(&manifest).await;
        let manifest_manager = PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(manifest_manager.get_operation_body(&id, None), Some(body))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poller_wont_start_without_uplink_connection() {
        let uplink_endpoint = Url::parse("https://definitely.not.uplink").unwrap();
        assert!(
            PersistedQueryManifestPoller::new(
                Configuration::fake_builder()
                    .uplink(UplinkConfig::for_tests(Endpoints::fallback(vec![
                        uplink_endpoint
                    ])))
                    .build()
                    .unwrap(),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poller_fails_over_on_gcs_failure() {
        let (_mock_server1, url1) = mock_pq_uplink_bad_gcs().await;
        let (id, body, manifest) = fake_manifest();
        let (_mock_guard2, url2) = mock_pq_uplink_one_endpoint(&manifest, None).await;
        let manifest_manager = PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .uplink(UplinkConfig::for_tests(Endpoints::fallback(vec![
                    url1, url2,
                ])))
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(manifest_manager.get_operation_body(&id, None), Some(body))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn uses_local_manifest() {
        let (_, body, _) = fake_manifest();
        let id = "5678".to_string();

        let manifest_manager = PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .apq(Apq::fake_new(Some(false)))
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .local_manifests(vec![
                            "tests/fixtures/persisted-queries-manifest.json".to_string(),
                        ])
                        .build(),
                )
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(manifest_manager.get_operation_body(&id, None), Some(body))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hot_reload_config_enforcement() {
        let err = PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .apq(Apq::fake_new(Some(false)))
                .persisted_query(PersistedQueries::builder().hot_reload(true).build())
                .build()
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "`persisted_queries.hot_reload` requires `local_manifests`"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hot_reload_stream_initial_load() {
        let (_, body, _) = fake_manifest();
        let id = "5678".to_string();

        let manifest_manager = PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .apq(Apq::fake_new(Some(false)))
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .local_manifests(vec![
                            "tests/fixtures/persisted-queries-manifest.json".to_string(),
                        ])
                        .hot_reload(true)
                        .build(),
                )
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(manifest_manager.get_operation_body(&id, None), Some(body));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handles_empty_pq_manifest_from_uplink() {
        let (_mock_guard, uplink_config) = mock_empty_pq_uplink().await;
        let manifest_manager = PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();

        // Should successfully start up with empty manifest
        assert_eq!(manifest_manager.get_all_operations().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hot_reload_stream_reloads_on_file_change() {
        const FIXTURE_PATH: &str = "tests/fixtures/persisted-queries-manifest-hot-reload.json";
        // Note: this directly matches the contents of the file in the fixtures
        // directory in order to ensure the fixture is restored after we modify
        // it for this test.
        const ORIGINAL_MANIFEST_CONTENTS: &str = r#"{
    "format": "apollo-persisted-query-manifest",
    "version": 1,
    "operations": [
        {
            "id": "5678",
            "name": "typename",
            "type": "query",
            "body": "query { typename }"
        }
    ]
}
"#;

        const UPDATED_MANIFEST_CONTENTS: &str = r#"{
    "format": "apollo-persisted-query-manifest",
    "version": 1,
    "operations": [
        {
            "id": "1234",
            "name": "typename",
            "type": "query",
            "body": "query { typename }"
        }
    ]
}
"#;

        let original_id = "5678".to_string();
        let updated_id = "1234".to_string();
        let body = "query { typename }".to_string();

        let manifest_manager = PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .apq(Apq::fake_new(Some(false)))
                .persisted_query(
                    PersistedQueries::builder()
                        .enabled(true)
                        .local_manifests(vec![FIXTURE_PATH.to_string()])
                        .hot_reload(true)
                        .build(),
                )
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            manifest_manager.get_operation_body(&original_id, None),
            Some(body.clone())
        );

        // Change the file
        let mut file = tokio::fs::File::create(FIXTURE_PATH).await.unwrap();
        file.write_all(UPDATED_MANIFEST_CONTENTS.as_bytes())
            .await
            .unwrap();
        file.sync_all().await.unwrap();

        // Wait for the file to be reloaded
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        // The original ID should be gone
        assert_eq!(
            manifest_manager.get_operation_body(&original_id, None),
            None
        );
        // The updated ID should be present
        assert_eq!(
            manifest_manager.get_operation_body(&updated_id, None),
            Some(body.clone())
        );

        // Cleanup, restore the original file
        let mut file = tokio::fs::File::create(FIXTURE_PATH).await.unwrap();
        file.write_all(ORIGINAL_MANIFEST_CONTENTS.as_bytes())
            .await
            .unwrap();
        file.sync_all().await.unwrap();

        // Ensure the restoration is successful
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert_eq!(
            manifest_manager.get_operation_body(&original_id, None),
            Some(body)
        );
    }
}
