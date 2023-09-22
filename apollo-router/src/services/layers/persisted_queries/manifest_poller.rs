//!  Persisted query manifest poller. Once created, will poll for updates continuously, reading persisted queries into memory.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::RwLock;

use apollo_parser::ast;
use apollo_parser::ast::AstNode;
use apollo_parser::Parser;
use apollo_parser::SyntaxTree;
use futures::prelude::*;
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tower::BoxError;

use crate::uplink::persisted_queries_manifest_stream::MaybePersistedQueriesManifestChunks;
use crate::uplink::persisted_queries_manifest_stream::PersistedQueriesManifestChunk;
use crate::uplink::persisted_queries_manifest_stream::PersistedQueriesManifestQuery;
use crate::uplink::stream_from_uplink_transforming_new_response;
use crate::uplink::UplinkConfig;
use crate::Configuration;

/// An in memory cache of persisted queries.
pub(crate) type PersistedQueryManifest = HashMap<String, String>;

/// How the router should respond to requests that are not resolved as the IDs
/// of an operation in the manifest. (For the most part this means "requests
/// sent as freeform GraphQL", though it also includes requests sent as an ID
/// that is not found in the PQ manifest but is found in the APQ cache; because
/// you cannot combine APQs with safelisting, this is only relevant in "allow
/// all" and "log unknown" modes.)
#[derive(Debug)]
pub(crate) enum FreeformGraphQLBehavior {
    AllowAll {
        apq_enabled: bool,
    },
    DenyAll {
        log_unknown: bool,
    },
    AllowIfInSafelist {
        safelist: FreeformGraphQLSafelist,
        log_unknown: bool,
    },
    LogUnlessInSafelist {
        safelist: FreeformGraphQLSafelist,
        apq_enabled: bool,
    },
}

/// Describes what the router should do for a given request: allow it, deny it
/// with an error, or allow it but log the operation as unknown.
pub(crate) enum FreeformGraphQLAction {
    Allow,
    Deny,
    AllowAndLog,
    DenyAndLog,
}

impl FreeformGraphQLBehavior {
    fn action_for_freeform_graphql(
        &self,
        body_from_request: &str,
        ast: SyntaxTree,
    ) -> FreeformGraphQLAction {
        match self {
            FreeformGraphQLBehavior::AllowAll { .. } => FreeformGraphQLAction::Allow,
            // Note that this branch doesn't get called in practice, because we catch
            // DenyAll at an earlier phase with never_allows_freeform_graphql.
            FreeformGraphQLBehavior::DenyAll { log_unknown, .. } => {
                if *log_unknown {
                    FreeformGraphQLAction::DenyAndLog
                } else {
                    FreeformGraphQLAction::Deny
                }
            }
            FreeformGraphQLBehavior::AllowIfInSafelist {
                safelist,
                log_unknown,
                ..
            } => {
                if safelist.is_allowed(body_from_request, ast) {
                    FreeformGraphQLAction::Allow
                } else if *log_unknown {
                    FreeformGraphQLAction::DenyAndLog
                } else {
                    FreeformGraphQLAction::Deny
                }
            }
            FreeformGraphQLBehavior::LogUnlessInSafelist { safelist, .. } => {
                if safelist.is_allowed(body_from_request, ast) {
                    FreeformGraphQLAction::Allow
                } else {
                    FreeformGraphQLAction::AllowAndLog
                }
            }
        }
    }
}

/// The normalized bodies of all operations in the PQ manifest.
///
/// Normalization currently consists of:
/// - Sorting the top-level definitions (operation and fragment definitions)
///   deterministically.
/// - Printing the AST using apollo-encoder's default formatting (ie,
///   normalizing all ignored characters such as whitespace and comments).
///
/// Sorting top-level definitions is important because common clients such as
/// Apollo Client Web have modes of use where it is easy to find all the
/// operation and fragment definitions at build time, but challenging to
/// determine what order the client will put them in at run time.
///
/// Normalizing ignored characters is helpful because being strict on whitespace
/// is more likely to get in your way than to aid in security --- but more
/// importantly, once we're doing any normalization at all, it's much easier to
/// normalize to the default formatting instead of trying to preserve
/// formatting.
#[derive(Debug)]
pub(crate) struct FreeformGraphQLSafelist {
    normalized_bodies: HashSet<String>,
}

impl FreeformGraphQLSafelist {
    fn new(manifest: &PersistedQueryManifest) -> Self {
        let mut safelist = Self {
            normalized_bodies: HashSet::new(),
        };

        for body in manifest.values() {
            safelist.insert_from_manifest(body);
        }

        safelist
    }

    fn insert_from_manifest(&mut self, body_from_manifest: &str) {
        self.normalized_bodies.insert(
            self.normalize_body(body_from_manifest, Parser::new(body_from_manifest).parse()),
        );
    }

    fn is_allowed(&self, body_from_request: &str, ast: SyntaxTree) -> bool {
        // Note: consider adding an LRU cache that caches this function's return
        // value based solely on body_from_request without needing to normalize
        // the body.
        self.normalized_bodies
            .contains(&self.normalize_body(body_from_request, ast))
    }

    fn normalize_body(&self, body_from_request: &str, ast: SyntaxTree) -> String {
        if ast.errors().peekable().peek().is_some() {
            // If we can't parse the operation (whether from the PQ list or the
            // incoming request), then we can't normalize it. We keep it around
            // unnormalized, so that it at least works as a byte-for-byte
            // safelist entry. (We also do this if we get any other errors
            // below.)
            body_from_request.to_string()
        } else {
            let mut encoder_document = apollo_encoder::Document::new();

            let mut operations = vec![];
            let mut fragments = vec![];

            for definition in ast.document().syntax().children() {
                if ast::OperationDefinition::can_cast(definition.kind()) {
                    operations.push(ast::OperationDefinition::cast(definition).unwrap())
                } else if ast::FragmentDefinition::can_cast(definition.kind()) {
                    fragments.push(ast::FragmentDefinition::cast(definition).unwrap())
                }
            }

            // First include operation definitions, sorted by name.
            operations.sort_by_key(|x| x.name().map(|n| n.text().to_string()));
            for parser_operation in operations {
                let encoder_operation = match parser_operation.try_into() {
                    Ok(op) => op,
                    Err(_) => return body_from_request.to_string(),
                };

                encoder_document.operation(encoder_operation)
            }

            // Next include fragment definitions, sorted by name.
            fragments.sort_by_key(|x| {
                x.fragment_name()
                    .and_then(|n| n.name())
                    .map(|n| n.text().to_string())
            });
            for parser_fragment in fragments {
                let encoder_fragment = match parser_fragment.try_into() {
                    Ok(op) => op,
                    Err(_) => return body_from_request.to_string(),
                };

                encoder_document.fragment(encoder_fragment)
            }

            encoder_document.to_string()
        }
    }
}

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
        if let Some(uplink_config) = config.uplink.as_ref() {
            // Note that the contents of this Arc<RwLock> will be overwritten by poll_uplink before
            // we return from this `new` method, so the particular choice of freeform_graphql_behavior
            // here does not matter. (Can we improve this? We could use an Option but then we'd just
            // end up `unwrap`ping a lot later. Perhaps MaybeUninit, but that's even worse?)
            let state = Arc::new(RwLock::new(PersistedQueryManifestPollerState {
                persisted_query_manifest: PersistedQueryManifest::new(),
                freeform_graphql_behavior: FreeformGraphQLBehavior::DenyAll { log_unknown: false },
            }));

            let http_client = Client::builder().timeout(uplink_config.timeout).build()
            .map_err(|e| -> BoxError {
                format!(
                    "could not initialize HTTP client for fetching persisted queries manifest chunks: {}",
                    e
                ).into()
            })?;

            let (_drop_signal, drop_receiver) = mpsc::channel::<()>(1);
            let (ready_sender, mut ready_receiver) =
                mpsc::channel::<ManifestPollResultOnStartup>(1);

            // start polling uplink for persisted query chunks
            tokio::task::spawn(poll_uplink(
                uplink_config.clone(),
                state.clone(),
                config,
                ready_sender,
                drop_receiver,
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
                    return Err("could not receive ready event for persisted query layer".into());
                }
            }

            Ok(Self {
                state,
                _drop_signal,
            })
        } else {
            Err("persisted queries requires Apollo GraphOS. ensure that you have set APOLLO_KEY and APOLLO_GRAPH_REF environment variables".into())
        }
    }

    pub(crate) fn get_operation_body(&self, persisted_query_id: &str) -> Option<String> {
        let state = self
            .state
            .read()
            .expect("could not acquire read lock on persisted query manifest state");
        state
            .persisted_query_manifest
            .get(persisted_query_id)
            .cloned()
    }

    pub(crate) fn get_all_operations(&self) -> Vec<String> {
        let state = self
            .state
            .read()
            .expect("could not acquire read lock on persisted query manifest state");
        state.persisted_query_manifest.values().cloned().collect()
    }

    pub(crate) fn action_for_freeform_graphql(
        &self,
        query: &str,
        ast: SyntaxTree,
    ) -> FreeformGraphQLAction {
        let state = self
            .state
            .read()
            .expect("could not acquire read lock on persisted query state");
        state
            .freeform_graphql_behavior
            .action_for_freeform_graphql(query, ast)
    }

    // Some(bool) means "never allows freeform GraphQL, bool is whether or not to log"
    // None means "sometimes allows freeform GraphQL"
    pub(crate) fn never_allows_freeform_graphql(&self) -> Option<bool> {
        let state = self
            .state
            .read()
            .expect("could not acquire read lock on persisted query state");
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
        let state = self
            .state
            .read()
            .expect("could not acquire read lock on persisted query state");
        match state.freeform_graphql_behavior {
            FreeformGraphQLBehavior::AllowAll { apq_enabled, .. }
            | FreeformGraphQLBehavior::LogUnlessInSafelist { apq_enabled, .. } => apq_enabled,
            _ => false,
        }
    }
}

async fn poll_uplink(
    uplink_config: UplinkConfig,
    state: Arc<RwLock<PersistedQueryManifestPollerState>>,
    config: Configuration,
    ready_sender: mpsc::Sender<ManifestPollResultOnStartup>,
    mut drop_receiver: mpsc::Receiver<()>,
    http_client: Client,
) {
    let http_client = http_client.clone();
    let mut uplink_executor = stream::select_all(vec![
        stream_from_uplink_transforming_new_response::<
            PersistedQueriesManifestQuery,
            MaybePersistedQueriesManifestChunks,
            Option<PersistedQueryManifest>,
        >(uplink_config.clone(), move |response| {
            let http_client = http_client.clone();
            Box::new(Box::pin(async move {
                match response {
                    Some(chunks) => manifest_from_chunks(chunks, http_client)
                        .await
                        .map(Some)
                        .map_err(|err| {
                            format!("could not download persisted query lists: {}", err).into()
                        }),
                    None => Ok(None),
                }
            }))
        })
        .map(|res| match res {
            Ok(Some(new_manifest)) => ManifestPollEvent::NewManifest(new_manifest),
            Ok(None) => ManifestPollEvent::NoPersistedQueryList {
                graph_ref: uplink_config.apollo_graph_ref.clone(),
            },
            Err(e) => ManifestPollEvent::Err(e.into()),
        })
        .boxed(),
        drop_receiver
            .recv()
            .into_stream()
            .filter_map(|res| {
                future::ready(match res {
                    None => Some(ManifestPollEvent::Shutdown),
                    Some(()) => Some(ManifestPollEvent::Err(
                        "received message on drop channel in persisted query layer, which never \
                         gets sent"
                            .into(),
                    )),
                })
            })
            .boxed(),
    ])
    .take_while(|msg| future::ready(!matches!(msg, ManifestPollEvent::Shutdown)))
    .boxed();

    let mut ready_sender_once = Some(ready_sender);

    while let Some(event) = uplink_executor.next().await {
        match event {
            ManifestPollEvent::NewManifest(new_manifest) => {
                let freeform_graphql_behavior = if config.preview_persisted_queries.safelist.enabled
                {
                    if config.preview_persisted_queries.safelist.require_id {
                        FreeformGraphQLBehavior::DenyAll {
                            log_unknown: config.preview_persisted_queries.log_unknown,
                        }
                    } else {
                        FreeformGraphQLBehavior::AllowIfInSafelist {
                            safelist: FreeformGraphQLSafelist::new(&new_manifest),
                            log_unknown: config.preview_persisted_queries.log_unknown,
                        }
                    }
                } else if config.preview_persisted_queries.log_unknown {
                    FreeformGraphQLBehavior::LogUnlessInSafelist {
                        safelist: FreeformGraphQLSafelist::new(&new_manifest),
                        apq_enabled: config.apq.enabled,
                    }
                } else {
                    FreeformGraphQLBehavior::AllowAll {
                        apq_enabled: config.apq.enabled,
                    }
                };

                let new_state = PersistedQueryManifestPollerState {
                    persisted_query_manifest: new_manifest,
                    freeform_graphql_behavior,
                };

                state
                    .write()
                    .map(|mut locked_state| {
                        *locked_state = new_state;
                    })
                    .expect("could not acquire write lock on persisted query manifest state");

                send_startup_event_or_log_error(
                    &mut ready_sender_once,
                    ManifestPollResultOnStartup::LoadedOperations,
                )
                .await;
            }
            ManifestPollEvent::Err(e) => {
                send_startup_event_or_log_error(
                    &mut ready_sender_once,
                    ManifestPollResultOnStartup::Err(e),
                )
                .await
            }
            ManifestPollEvent::NoPersistedQueryList { graph_ref } => {
                send_startup_event_or_log_error(
                    &mut ready_sender_once,
                    ManifestPollResultOnStartup::Err(
                        format!("no persisted query list found for graph ref {}", &graph_ref)
                            .into(),
                    ),
                )
                .await
            }
            // this event is a no-op because we `take_while` on messages that are not this one
            ManifestPollEvent::Shutdown => (),
        }
    }

    async fn send_startup_event_or_log_error(
        ready_sender: &mut Option<mpsc::Sender<ManifestPollResultOnStartup>>,
        message: ManifestPollResultOnStartup,
    ) {
        match (ready_sender.take(), message) {
            (Some(ready_sender), message) => {
                if let Err(e) = ready_sender.send(message).await {
                    tracing::debug!(
                        "could not send startup event for the persisted query layer: {e}"
                    );
                }
            }
            (None, ManifestPollResultOnStartup::Err(err)) => {
                // We've already successfully started up, but we received some sort of error. This doesn't
                // need to break our functional router, but we can log in case folks are interested.
                tracing::error!(
                    "error while polling uplink for persisted query manifests: {}",
                    err
                )
            }
            // Do nothing in the normal background "new manifest" case.
            (None, ManifestPollResultOnStartup::LoadedOperations) => {}
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
) -> Result<(), BoxError> {
    let mut it = chunk.urls.iter().peekable();
    while let Some(chunk_url) = it.next() {
        match fetch_chunk(http_client.clone(), chunk_url).await {
            Ok(chunk) => {
                for operation in chunk.operations {
                    operations.insert(operation.id, operation.body);
                }
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
            format!(
                "error fetching persisted queries manifest chunk from {}: {}",
                chunk_url, e
            )
            .into()
        })?
        .json::<SignedUrlChunk>()
        .await
        .map_err(|e| -> BoxError {
            format!(
                "error reading body of persisted queries manifest chunk from {}: {}",
                chunk_url, e
            )
            .into()
        })?;

    if chunk.format != "apollo-persisted-query-manifest" {
        return Err("chunk format is not 'apollo-persisted-query-manifest'".into());
    }

    if chunk.version != 1 {
        return Err("persisted query manifest chunk version is not 1".into());
    }

    Ok(chunk)
}

/// Types of events produced by the manifest poller.
#[derive(Debug)]
pub(crate) enum ManifestPollEvent {
    NewManifest(PersistedQueryManifest),
    NoPersistedQueryList { graph_ref: String },
    Err(BoxError),
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
        let manifest_manager = PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .uplink(uplink_config)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(manifest_manager.get_operation_body(&id), Some(body))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poller_wont_start_without_uplink_connection() {
        let uplink_endpoint = Url::parse("https://definitely.not.uplink").unwrap();
        assert!(PersistedQueryManifestPoller::new(
            Configuration::fake_builder()
                .uplink(UplinkConfig::for_tests(Endpoints::fallback(vec![
                    uplink_endpoint
                ])))
                .build()
                .unwrap(),
        )
        .await
        .is_err());
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
        assert_eq!(manifest_manager.get_operation_body(&id), Some(body))
    }

    #[test]
    fn safelist_body_normalization() {
        let safelist = FreeformGraphQLSafelist::new(&PersistedQueryManifest::from([(
            "valid-syntax".to_string(),
            "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{b c  } # yeah"
                .to_string(),
        ), (
            "invalid-syntax".to_string(),
            "}}}".to_string()),
        ]));

        let is_allowed =
            |body: &str| -> bool { safelist.is_allowed(body, Parser::new(body).parse()) };

        // Precise string matches.
        assert!(is_allowed(
            "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{b c  } # yeah"
        ));

        // Reordering definitions and reformatting a bit matches.
        assert!(is_allowed(
            "#comment\n  fragment, B on U  , { b    c }    query SomeOp {  ...A ...B }  fragment    \nA on T { a }"
        ));

        // Reordering fields does not match!
        assert!(!is_allowed(
            "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{c b  } # yeah"
        ));

        // Documents with invalid syntax don't match...
        assert!(!is_allowed("}}}}"));

        // ... unless they precisely match a safelisted document that also has invalid syntax.
        assert!(is_allowed("}}}"));
    }
}
