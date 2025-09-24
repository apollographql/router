use std::collections::HashMap;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use apollo_compiler::Schema;
use apollo_compiler::ast::NamedType;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::parser::Parser;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::StringTemplate;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use lru::LruCache;
use multimap::MultiMap;
use opentelemetry::Array;
use opentelemetry::Key;
use opentelemetry::StringValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tokio::sync::RwLock;
use tokio::sync::broadcast;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::Sender;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::IntervalStream;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;
use tracing::Level;
use tracing::Span;

use super::cache_control::CacheControl;
use super::invalidation::Invalidation;
use super::invalidation_endpoint::InvalidationEndpointConfig;
use super::invalidation_endpoint::InvalidationService;
use super::invalidation_endpoint::SubgraphInvalidationConfig;
use super::metrics::CacheMetricContextKey;
use super::metrics::record_fetch_error;
use crate::Context;
use crate::Endpoint;
use crate::ListenAddr;
use crate::batching::BatchQuery;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::mock_subgraphs::execution::input_coercion::coerce_argument_values;
use crate::plugins::response_cache::cache_key::PrimaryCacheKeyEntity;
use crate::plugins::response_cache::cache_key::PrimaryCacheKeyRoot;
use crate::plugins::response_cache::cache_key::hash_additional_data;
use crate::plugins::response_cache::cache_key::hash_query;
use crate::plugins::response_cache::metrics;
use crate::plugins::response_cache::storage;
use crate::plugins::response_cache::storage::CacheEntry;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::Document;
use crate::plugins::response_cache::storage::postgres::Storage;
use crate::plugins::telemetry::LruSizeInstrument;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::span_ext::SpanMarkError;
use crate::query_planner::OperationKind;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::QueryHash;
use crate::spec::TYPENAME;

/// Change this key if you introduce a breaking change in response caching algorithm to make sure it won't take the previous entries
pub(crate) const RESPONSE_CACHE_VERSION: &str = "1.0";
pub(crate) const CACHE_TAG_DIRECTIVE_NAME: &str = "federation__cacheTag";
pub(crate) const ENTITIES: &str = "_entities";
pub(crate) const REPRESENTATIONS: &str = "representations";
pub(crate) const CONTEXT_CACHE_KEY: &str = "apollo::response_cache::key";
/// Context key to enable support of debugger
pub(crate) const CONTEXT_DEBUG_CACHE_KEYS: &str = "apollo::response_cache::debug_cached_keys";
pub(crate) const CACHE_DEBUG_HEADER_NAME: &str = "apollo-cache-debugging";
pub(crate) const CACHE_DEBUG_EXTENSIONS_KEY: &str = "apolloCacheDebugging";
pub(crate) const CACHE_DEBUGGER_VERSION: &str = "1.0";
pub(crate) const GRAPHQL_RESPONSE_EXTENSION_ROOT_FIELDS_CACHE_TAGS: &str = "apolloCacheTags";
pub(crate) const GRAPHQL_RESPONSE_EXTENSION_ENTITY_CACHE_TAGS: &str = "apolloEntityCacheTags";
/// Used to mark cache tags as internal and should not be exported or displayed to our users
pub(crate) const INTERNAL_CACHE_TAG_PREFIX: &str = "__apollo_internal::";
const DEFAULT_LRU_PRIVATE_QUERIES_SIZE: NonZeroUsize = NonZeroUsize::new(2048).unwrap();
const LRU_PRIVATE_QUERIES_INSTRUMENT_NAME: &str =
    "apollo.router.response_cache.private_queries.lru.size";

register_private_plugin!("apollo", "experimental_response_cache", ResponseCache);

#[derive(Clone)]
pub(crate) struct ResponseCache {
    pub(super) storage: Arc<StorageInterface>,
    endpoint_config: Option<Arc<InvalidationEndpointConfig>>,
    subgraphs: Arc<SubgraphConfiguration<Subgraph>>,
    entity_type: Option<String>,
    enabled: bool,
    debug: bool,
    private_queries: Arc<RwLock<LruCache<PrivateQueryKey, ()>>>,
    pub(crate) invalidation: Invalidation,
    supergraph_schema: Arc<Valid<Schema>>,
    /// map containing the enum GRAPH
    subgraph_enums: Arc<HashMap<String, String>>,
    /// To close all related tasks
    drop_tx: Sender<()>,
    lru_size_instrument: LruSizeInstrument,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct PrivateQueryKey {
    query_hash: String,
    has_private_id: bool,
}

impl Drop for ResponseCache {
    fn drop(&mut self) {
        let _ = self.drop_tx.send(());
    }
}

#[derive(Clone)]
pub(crate) struct StorageInterface {
    all: Option<Arc<OnceLock<Storage>>>,
    subgraphs: HashMap<String, Arc<OnceLock<Storage>>>,
}

impl StorageInterface {
    pub(crate) fn get(&self, subgraph: &str) -> Option<&Storage> {
        let storage = self.subgraphs.get(subgraph).or(self.all.as_ref())?;
        storage.get()
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        if let Some(all) = self.all.as_ref().and_then(|all| all.get()) {
            all.migrate().await?;
        }
        futures::future::try_join_all(
            self.subgraphs
                .values()
                .filter_map(|s| Some(s.get()?.migrate())),
        )
        .await?;

        Ok(())
    }

    /// Spawn tokio task to refresh metrics about expired data count
    fn expired_data_count_tasks(&self, drop_signal: Receiver<()>) {
        if let Some(all) = self.all.as_ref().and_then(|all| all.get()) {
            tokio::task::spawn(metrics::expired_data_task(
                all.clone(),
                drop_signal.resubscribe(),
                None,
            ));
        }
        for (subgraph_name, subgraph_cache_storage) in &self.subgraphs {
            if let Some(subgraph_cache_storage) = subgraph_cache_storage.get() {
                tokio::task::spawn(metrics::expired_data_task(
                    subgraph_cache_storage.clone(),
                    drop_signal.resubscribe(),
                    subgraph_name.clone().into(),
                ));
            }
        }
    }

    async fn update_cron(&self) -> anyhow::Result<()> {
        if let Some(all) = self.all.as_ref().and_then(|all| all.get()) {
            all.update_cron().await?;
        }
        futures::future::try_join_all(
            self.subgraphs
                .values()
                .filter_map(|s| Some(s.get()?.update_cron())),
        )
        .await?;

        Ok(())
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
impl StorageInterface {
    /// Replace the `all` storage layer in this struct.
    ///
    /// This supports tests which initialize the `StorageInterface` without a backing database
    /// and then add one later, simulating a delayed storage connection.
    pub(crate) fn replace_storage(&self, storage: Storage) -> Option<()> {
        self.all.as_ref()?.set(storage).ok()
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
impl From<Storage> for StorageInterface {
    fn from(storage: Storage) -> Self {
        Self {
            all: Some(Arc::new(storage.into())),
            subgraphs: HashMap::new(),
        }
    }
}

/// Configuration for response caching
#[derive(Clone, Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct Config {
    /// Enable or disable the response caching feature
    #[serde(default)]
    pub(crate) enabled: bool,

    #[serde(default)]
    /// Enable debug mode for the debugger
    debug: bool,

    /// Configure invalidation per subgraph
    pub(crate) subgraph: SubgraphConfiguration<Subgraph>,

    /// Global invalidation configuration
    invalidation: Option<InvalidationEndpointConfig>,

    /// Buffer size for known private queries (default: 2048)
    #[serde(default = "default_lru_private_queries_size")]
    private_queries_buffer_size: NonZeroUsize,
}

const fn default_lru_private_queries_size() -> NonZeroUsize {
    DEFAULT_LRU_PRIVATE_QUERIES_SIZE
}

/// Per subgraph configuration for response caching
#[derive(Clone, Debug, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct Subgraph {
    /// PostgreSQL configuration
    pub(crate) postgres: Option<storage::postgres::Config>,

    /// expiration for all keys for this subgraph, unless overridden by the `Cache-Control` header in subgraph responses
    pub(crate) ttl: Option<Ttl>,

    /// activates caching for this subgraph, overrides the global configuration
    pub(crate) enabled: Option<bool>,

    /// Context key used to separate cache sections per user
    pub(crate) private_id: Option<String>,

    /// Invalidation configuration
    pub(crate) invalidation: Option<SubgraphInvalidationConfig>,
}

impl Default for Subgraph {
    fn default() -> Self {
        Self {
            postgres: None,
            enabled: Some(true),
            ttl: Default::default(),
            private_id: Default::default(),
            invalidation: Default::default(),
        }
    }
}

/// Per subgraph configuration for response caching
#[derive(Clone, Debug, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct Ttl(
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(crate) Duration,
);

#[derive(Default, Serialize, Deserialize, Debug)]
#[serde(default)]
pub(crate) struct CacheSubgraph(pub(crate) HashMap<String, CacheHitMiss>);

#[derive(Default, Serialize, Deserialize, Debug)]
#[serde(default)]
pub(crate) struct CacheHitMiss {
    pub(crate) hit: usize,
    pub(crate) miss: usize,
}

#[async_trait::async_trait]
impl PluginPrivate for ResponseCache {
    const HIDDEN_FROM_CONFIG_JSON_SCHEMA: bool = true;
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        let entity_type = init
            .supergraph_schema
            .schema_definition
            .query
            .as_ref()
            .map(|q| q.name.to_string());

        let mut all = None;
        let (drop_tx, drop_rx) = broadcast::channel(2);
        let mut task_aborts = Vec::new();
        if let Some(postgres) = &init.config.subgraph.all.postgres {
            let postgres_config = postgres.clone();
            let required_to_start = postgres_config.required_to_start;
            all = match Storage::new(&postgres_config).await {
                Ok(storage) => Some(Arc::new(OnceLock::from(storage))),
                Err(e) => {
                    tracing::error!(
                        cache = "response",
                        error = %e,
                        "could not open connection to Postgres for caching",
                    );
                    if required_to_start {
                        return Err(e.into());
                    } else {
                        let storage = Arc::new(OnceLock::new());
                        task_aborts.push(
                            tokio::spawn(check_connection(
                                postgres_config,
                                storage.clone(),
                                drop_rx,
                                None,
                            ))
                            .abort_handle(),
                        );
                        Some(storage)
                    }
                }
            };
        }
        let mut subgraph_storages = HashMap::new();
        for (subgraph, config) in &init.config.subgraph.subgraphs {
            if let Some(postgres) = &config.postgres {
                let required_to_start = postgres.required_to_start;
                let storage = match Storage::new(postgres).await {
                    Ok(storage) => Arc::new(OnceLock::from(storage)),
                    Err(e) => {
                        tracing::error!(
                            cache = "response",
                            error = %e,
                            "could not open connection to Postgres for caching",
                        );
                        if required_to_start {
                            return Err(e.into());
                        } else {
                            let storage = Arc::new(OnceLock::new());
                            task_aborts.push(
                                tokio::spawn(check_connection(
                                    postgres.clone(),
                                    storage.clone(),
                                    drop_tx.subscribe(),
                                    subgraph.clone().into(),
                                ))
                                .abort_handle(),
                            );
                            storage
                        }
                    }
                };
                subgraph_storages.insert(subgraph.clone(), storage);
            }
        }

        if init.config.subgraph.all.ttl.is_none()
            && init
                .config
                .subgraph
                .subgraphs
                .values()
                .any(|s| s.ttl.is_none())
        {
            return Err("a TTL must be configured for all subgraphs or globally"
                .to_string()
                .into());
        }

        if init
            .config
            .subgraph
            .all
            .invalidation
            .as_ref()
            .map(|i| i.shared_key.is_empty())
            .unwrap_or_default()
        {
            return Err(
                "you must set a default shared_key invalidation for all subgraphs"
                    .to_string()
                    .into(),
            );
        }

        let storage = Arc::new(StorageInterface {
            all,
            subgraphs: subgraph_storages,
        });
        storage.migrate().await?;
        storage.update_cron().await?;

        let invalidation = Invalidation::new(storage.clone()).await?;

        Ok(Self {
            storage,
            entity_type,
            enabled: init.config.enabled,
            debug: init.config.debug,
            endpoint_config: init.config.invalidation.clone().map(Arc::new),
            subgraphs: Arc::new(init.config.subgraph),
            private_queries: Arc::new(RwLock::new(LruCache::new(
                init.config.private_queries_buffer_size,
            ))),
            invalidation,
            subgraph_enums: Arc::new(get_subgraph_enums(&init.supergraph_schema)),
            supergraph_schema: init.supergraph_schema,
            drop_tx,
            lru_size_instrument: LruSizeInstrument::new(LRU_PRIVATE_QUERIES_INSTRUMENT_NAME),
        })
    }

    fn activate(&self) {
        self.storage
            .expired_data_count_tasks(self.drop_tx.subscribe());
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let debug = self.debug;
        ServiceBuilder::new()
            .map_response(move |mut response: supergraph::Response| {
                if let Some(cache_control) = response
                    .context
                    .extensions()
                    .with_lock(|lock| lock.get::<CacheControl>().cloned())
                {
                    let _ = cache_control.to_headers(response.response.headers_mut());
                }

                if debug
                    && let Some(debug_data) =
                        response.context.get_json_value(CONTEXT_DEBUG_CACHE_KEYS)
                {
                    return response.map_stream(move |mut body| {
                        body.extensions.insert(
                            CACHE_DEBUG_EXTENSIONS_KEY,
                            serde_json_bytes::json!({
                                "version": CACHE_DEBUGGER_VERSION,
                                "data": debug_data.clone()
                            }),
                        );
                        body
                    });
                }

                response
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let subgraph_ttl = self
            .subgraph_ttl(name)
            .unwrap_or_else(|| Duration::from_secs(60 * 60 * 24)); // The unwrap should not happen because it's checked when creating the plugin
        let subgraph_enabled = self.subgraph_enabled(name);
        let private_id = self.subgraphs.get(name).private_id.clone();

        let name = name.to_string();

        if subgraph_enabled {
            let private_queries = self.private_queries.clone();
            let inner = ServiceBuilder::new()
                .map_response(move |response: subgraph::Response| {
                    update_cache_control(
                        &response.context,
                        &CacheControl::new(response.response.headers(), None)
                            .ok()
                            .unwrap_or_else(CacheControl::no_store),
                    );

                    response
                })
                .service(CacheService {
                    service: ServiceBuilder::new()
                        .buffered()
                        .service(service)
                        .boxed_clone(),
                    entity_type: self.entity_type.clone(),
                    name: name.to_string(),
                    storage: self.storage.clone(),
                    subgraph_ttl,
                    private_queries,
                    private_id,
                    debug: self.debug,
                    supergraph_schema: self.supergraph_schema.clone(),
                    subgraph_enums: self.subgraph_enums.clone(),
                    lru_size_instrument: self.lru_size_instrument.clone(),
                });
            tower::util::BoxService::new(inner)
        } else {
            ServiceBuilder::new()
                .map_response(move |response: subgraph::Response| {
                    update_cache_control(
                        &response.context,
                        &CacheControl::new(response.response.headers(), None)
                            .ok()
                            .unwrap_or_else(CacheControl::no_store),
                    );

                    response
                })
                .service(service)
                .boxed()
        }
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut map = MultiMap::new();
        if self.enabled
            && self
                .subgraphs
                .all
                .invalidation
                .as_ref()
                .map(|i| i.enabled)
                .unwrap_or_default()
        {
            match &self.endpoint_config {
                Some(endpoint_config) => {
                    let endpoint = Endpoint::from_router_service(
                        endpoint_config.path.clone(),
                        InvalidationService::new(self.subgraphs.clone(), self.invalidation.clone())
                            .boxed(),
                    );
                    tracing::info!(
                        "Response cache invalidation endpoint listening on: {}{}",
                        endpoint_config.listen,
                        endpoint_config.path
                    );
                    map.insert(endpoint_config.listen.clone(), endpoint);
                }
                None => {
                    tracing::warn!(
                        "Cannot start response cache invalidation endpoint because the listen address and endpoint is not configured"
                    );
                }
            }
        }

        map
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
pub(super) const INVALIDATION_SHARED_KEY: &str = "supersecret";
impl ResponseCache {
    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    pub(crate) async fn for_test(
        storage: Storage,
        subgraphs: HashMap<String, Subgraph>,
        supergraph_schema: Arc<Valid<Schema>>,
        truncate_namespace: bool,
        update_cron: bool,
    ) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        use std::net::IpAddr;
        use std::net::Ipv4Addr;
        use std::net::SocketAddr;
        storage.migrate().await?;
        if update_cron {
            storage.update_cron().await?;
        }
        if truncate_namespace {
            storage.truncate_namespace().await?;
        }

        let storage = Arc::new(StorageInterface {
            all: Some(Arc::new(storage.into())),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone()).await?;
        let (drop_tx, _drop_rx) = broadcast::channel(2);
        Ok(Self {
            storage,
            entity_type: None,
            enabled: true,
            debug: true,
            subgraphs: Arc::new(SubgraphConfiguration {
                all: Subgraph {
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: INVALIDATION_SHARED_KEY.to_string(),
                    }),
                    ..Default::default()
                },
                subgraphs,
            }),
            private_queries: Arc::new(RwLock::new(LruCache::new(DEFAULT_LRU_PRIVATE_QUERIES_SIZE))),
            endpoint_config: Some(Arc::new(InvalidationEndpointConfig {
                path: String::from("/invalidation"),
                listen: ListenAddr::SocketAddr(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                    4000,
                )),
            })),
            invalidation,
            subgraph_enums: Arc::new(get_subgraph_enums(&supergraph_schema)),
            supergraph_schema,
            drop_tx,
            lru_size_instrument: LruSizeInstrument::new(LRU_PRIVATE_QUERIES_INSTRUMENT_NAME),
        })
    }
    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    /// Use this method when you want to test ResponseCache without database available
    pub(crate) async fn without_storage_for_failure_mode(
        subgraphs: HashMap<String, Subgraph>,
        supergraph_schema: Arc<Valid<Schema>>,
    ) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        use std::net::IpAddr;
        use std::net::Ipv4Addr;
        use std::net::SocketAddr;

        let storage = Arc::new(StorageInterface {
            all: Some(Default::default()),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone()).await?;
        let (drop_tx, _drop_rx) = broadcast::channel(2);

        Ok(Self {
            storage,
            entity_type: None,
            enabled: true,
            debug: true,
            subgraphs: Arc::new(SubgraphConfiguration {
                all: Subgraph {
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: INVALIDATION_SHARED_KEY.to_string(),
                    }),
                    ..Default::default()
                },
                subgraphs,
            }),
            private_queries: Arc::new(RwLock::new(LruCache::new(DEFAULT_LRU_PRIVATE_QUERIES_SIZE))),
            endpoint_config: Some(Arc::new(InvalidationEndpointConfig {
                path: String::from("/invalidation"),
                listen: ListenAddr::SocketAddr(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                    4000,
                )),
            })),
            invalidation,
            subgraph_enums: Arc::new(get_subgraph_enums(&supergraph_schema)),
            supergraph_schema,
            drop_tx,
            lru_size_instrument: LruSizeInstrument::new(LRU_PRIVATE_QUERIES_INSTRUMENT_NAME),
        })
    }

    // Returns boolean to know if cache is enabled for this subgraph
    fn subgraph_enabled(&self, subgraph_name: &str) -> bool {
        if !self.enabled {
            return false;
        }
        match (
            self.subgraphs.all.enabled,
            self.subgraphs.get(subgraph_name).enabled,
        ) {
            (_, Some(x)) => x, // explicit per-subgraph setting overrides the `all` default
            (Some(true) | None, None) => true, // unset defaults to true
            (Some(false), None) => false,
        }
    }

    // Returns the configured ttl for this subgraph
    fn subgraph_ttl(&self, subgraph_name: &str) -> Option<Duration> {
        self.subgraphs
            .get(subgraph_name)
            .ttl
            .clone()
            .map(|t| t.0)
            .or_else(|| self.subgraphs.all.ttl.clone().map(|ttl| ttl.0))
    }
}

/// Get the map of subgraph enum variant mapped with subgraph name
fn get_subgraph_enums(supergraph_schema: &Valid<Schema>) -> HashMap<String, String> {
    let mut subgraph_enums = HashMap::new();
    if let Some(graph_enum) = supergraph_schema.get_enum("join__Graph") {
        subgraph_enums.extend(graph_enum.values.iter().filter_map(
            |(enum_name, enum_value_def)| {
                let subgraph_name = enum_value_def
                    .directives
                    .get("join__graph")?
                    .specified_argument_by_name("name")?
                    .as_str()?
                    .to_string();

                Some((enum_name.to_string(), subgraph_name))
            },
        ));
    }

    subgraph_enums
}

#[derive(Clone)]
struct CacheService {
    service: subgraph::BoxCloneService,
    name: String,
    entity_type: Option<String>,
    storage: Arc<StorageInterface>,
    subgraph_ttl: Duration,
    private_queries: Arc<RwLock<LruCache<PrivateQueryKey, ()>>>,
    private_id: Option<String>,
    debug: bool,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: Arc<HashMap<String, String>>,
    lru_size_instrument: LruSizeInstrument,
}

impl Service<subgraph::Request> for CacheService {
    type Response = subgraph::Response;
    type Error = BoxError;
    type Future = <subgraph::BoxService as Service<subgraph::Request>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: subgraph::Request) -> Self::Future {
        let clone = self.clone();
        let inner = std::mem::replace(self, clone);

        Box::pin(inner.call_inner(request))
    }
}

impl CacheService {
    async fn call_inner(
        mut self,
        request: subgraph::Request,
    ) -> Result<subgraph::Response, BoxError> {
        let storage = match self
            .storage
            .get(&self.name)
            .ok_or(storage::Error::NoStorage)
        {
            Ok(storage) => storage.clone(),
            Err(err) => {
                record_fetch_error(&err, &self.name);
                return self
                    .service
                    .map_response(move |response: subgraph::Response| {
                        update_cache_control(
                            &response.context,
                            &CacheControl::new(response.response.headers(), None)
                                .ok()
                                .unwrap_or_else(CacheControl::no_store),
                        );

                        response
                    })
                    .call(request)
                    .await;
            }
        };

        self.debug = self.debug
            && (request
                .supergraph_request
                .headers()
                .get(CACHE_DEBUG_HEADER_NAME)
                == Some(&HeaderValue::from_static("true")));
        // Check if the request is part of a batch. If it is, completely bypass response caching since it
        // will break any request batches which this request is part of.
        // This check is what enables Batching and response caching to work together, so be very careful
        // before making any changes to it.
        if request
            .context
            .extensions()
            .with_lock(|lock| lock.contains_key::<BatchQuery>())
        {
            return self.service.call(request).await;
        }
        // Don't use cache at all if no-store is set in cache-control header
        if request
            .subgraph_request
            .headers()
            .contains_key(&CACHE_CONTROL)
        {
            let cache_control = match CacheControl::new(request.subgraph_request.headers(), None) {
                Ok(cache_control) => cache_control,
                Err(err) => {
                    return Ok(subgraph::Response::builder()
                        .subgraph_name(request.subgraph_name)
                        .context(request.context)
                        .error(
                            graphql::Error::builder()
                                .message(format!("cannot get cache-control header: {err}"))
                                .extension_code("INVALID_CACHE_CONTROL_HEADER")
                                .build(),
                        )
                        .extensions(Object::default())
                        .build());
                }
            };
            if cache_control.no_store {
                let mut resp = self.service.call(request).await?;
                cache_control.to_headers(resp.response.headers_mut())?;
                return Ok(resp);
            }
        }
        let private_id = self.get_private_id(&request.context);
        // Knowing if there's a private_id or not will differentiate the hash because for a same query it can be both public and private depending if we have private_id set or not
        let private_query_key = PrivateQueryKey {
            query_hash: hash_query(&request.query_hash),
            has_private_id: private_id.is_some(),
        };

        let is_known_private = {
            self.private_queries
                .read()
                .await
                .contains(&private_query_key)
        };

        // the response will have a private scope but we don't have a way to differentiate users, so we know we will not get or store anything in the cache
        if is_known_private && private_id.is_none() {
            let mut debug_subgraph_request = None;
            let mut root_operation_fields = Vec::new();
            if self.debug {
                root_operation_fields = request
                    .executable_document
                    .as_ref()
                    .and_then(|executable_document| {
                        let operation_name =
                            request.subgraph_request.body().operation_name.as_deref();
                        Some(
                            executable_document
                                .operations
                                .get(operation_name)
                                .ok()?
                                .root_fields(executable_document)
                                .map(|f| f.name.to_string())
                                .collect(),
                        )
                    })
                    .unwrap_or_default();
                debug_subgraph_request = Some(request.subgraph_request.body().clone());
            }
            let is_entity = request
                .subgraph_request
                .body()
                .variables
                .contains_key(REPRESENTATIONS);
            let resp = self.service.call(request).await?;
            if self.debug {
                let cache_control = CacheControl::new(resp.response.headers(), None)?;
                let kind = if is_entity {
                    CacheEntryKind::Entity {
                        typename: "".to_string(),
                        entity_key: Default::default(),
                    }
                } else {
                    CacheEntryKind::RootFields {
                        root_fields: root_operation_fields,
                    }
                };
                resp.context.upsert::<_, CacheKeysContext>(
                    CONTEXT_DEBUG_CACHE_KEYS,
                    |mut val| {
                        val.push(CacheKeyContext {
                            key: "-".to_string(),
                            invalidation_keys: vec![],
                            kind,
                            hashed_private_id: private_id.clone(),
                            subgraph_name: self.name.clone(),
                            subgraph_request: debug_subgraph_request.unwrap_or_default(),
                            source: CacheKeySource::Subgraph,
                            cache_control,
                            data: serde_json_bytes::to_value(resp.response.body().clone())
                                .unwrap_or_default(),
                        });

                        val
                    },
                )?;
            }

            return Ok(resp);
        }

        if !request
            .subgraph_request
            .body()
            .variables
            .contains_key(REPRESENTATIONS)
        {
            if request.operation_kind == OperationKind::Query {
                let mut cache_hit: HashMap<String, CacheHitMiss> = HashMap::new();
                match cache_lookup_root(
                    self.name.clone(),
                    self.entity_type.as_deref(),
                    storage.clone(),
                    is_known_private,
                    private_id.as_deref(),
                    self.debug,
                    request,
                    self.supergraph_schema.clone(),
                    &self.subgraph_enums,
                )
                .instrument(tracing::info_span!(
                    "response_cache.lookup",
                    kind = "root",
                    "graphql.type" = self.entity_type.as_deref().unwrap_or_default(),
                    debug = self.debug,
                    private = is_known_private,
                    contains_private_id = private_id.is_some(),
                    "cache.key" = ::tracing::field::Empty,
                ))
                .await?
                {
                    ControlFlow::Break(response) => {
                        cache_hit.insert("Query".to_string(), CacheHitMiss { hit: 1, miss: 0 });
                        let _ = response.context.insert(
                            CacheMetricContextKey::new(response.subgraph_name.clone()),
                            CacheSubgraph(cache_hit),
                        );

                        Ok(response)
                    }
                    ControlFlow::Continue((request, mut root_cache_key, invalidation_keys)) => {
                        cache_hit.insert("Query".to_string(), CacheHitMiss { hit: 0, miss: 1 });
                        let _ = request.context.insert(
                            CacheMetricContextKey::new(request.subgraph_name.clone()),
                            CacheSubgraph(cache_hit),
                        );
                        let mut root_operation_fields: Vec<String> = Vec::new();
                        let mut debug_subgraph_request = None;
                        if self.debug {
                            root_operation_fields = request
                                .executable_document
                                .as_ref()
                                .and_then(|executable_document| {
                                    let operation_name =
                                        request.subgraph_request.body().operation_name.as_deref();
                                    Some(
                                        executable_document
                                            .operations
                                            .get(operation_name)
                                            .ok()?
                                            .root_fields(executable_document)
                                            .map(|f| f.name.to_string())
                                            .collect(),
                                    )
                                })
                                .unwrap_or_default();
                            debug_subgraph_request = Some(request.subgraph_request.body().clone());
                        }
                        let response = self.service.call(request).await?;

                        let cache_control =
                            if response.response.headers().contains_key(CACHE_CONTROL) {
                                CacheControl::new(
                                    response.response.headers(),
                                    self.subgraph_ttl.into(),
                                )?
                            } else {
                                CacheControl {
                                    no_store: true,
                                    ..Default::default()
                                }
                            };

                        if cache_control.private() {
                            // we did not know in advance that this was a query with a private scope, so we update the cache key
                            if !is_known_private {
                                let size = {
                                    let mut private_queries = self.private_queries.write().await;
                                    private_queries.put(private_query_key.clone(), ());
                                    private_queries.len()
                                };
                                self.lru_size_instrument.update(size as u64);

                                if let Some(s) = private_id.as_ref() {
                                    root_cache_key = format!("{root_cache_key}:{s}");
                                }
                            }

                            if self.debug {
                                response.context.upsert::<_, CacheKeysContext>(
                                    CONTEXT_DEBUG_CACHE_KEYS,
                                    |mut val| {
                                        val.push(CacheKeyContext {
                                            key: root_cache_key.clone(),
                                            hashed_private_id: private_id.clone(),
                                            invalidation_keys: invalidation_keys
                                                .clone()
                                                .into_iter()
                                                .filter(|k| {
                                                    !k.starts_with(INTERNAL_CACHE_TAG_PREFIX)
                                                })
                                                .collect(),
                                            kind: CacheEntryKind::RootFields {
                                                root_fields: root_operation_fields,
                                            },
                                            subgraph_name: self.name.clone(),
                                            subgraph_request: debug_subgraph_request
                                                .unwrap_or_default(),
                                            source: CacheKeySource::Subgraph,
                                            cache_control: cache_control.clone(),
                                            data: serde_json_bytes::to_value(
                                                response.response.body().clone(),
                                            )
                                            .unwrap_or_default(),
                                        });

                                        val
                                    },
                                )?;
                            }

                            if private_id.is_none() {
                                // the response has a private scope but we don't have a way to differentiate users, so we do not store the response in cache
                                // We don't need to fill the context with this cache key as it will never be cached
                                return Ok(response);
                            }
                        } else if self.debug {
                            response.context.upsert::<_, CacheKeysContext>(
                                CONTEXT_DEBUG_CACHE_KEYS,
                                |mut val| {
                                    val.push(CacheKeyContext {
                                        key: root_cache_key.clone(),
                                        hashed_private_id: private_id.clone(),
                                        invalidation_keys: invalidation_keys
                                            .clone()
                                            .into_iter()
                                            .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                                            .collect(),
                                        kind: CacheEntryKind::RootFields {
                                            root_fields: root_operation_fields,
                                        },
                                        subgraph_name: self.name.clone(),
                                        subgraph_request: debug_subgraph_request
                                            .unwrap_or_default(),
                                        source: CacheKeySource::Subgraph,
                                        cache_control: cache_control.clone(),
                                        data: serde_json_bytes::to_value(
                                            response.response.body().clone(),
                                        )
                                        .unwrap_or_default(),
                                    });

                                    val
                                },
                            )?;
                        }

                        if cache_control.should_store() {
                            cache_store_root_from_response(
                                storage,
                                self.subgraph_ttl,
                                &response,
                                cache_control,
                                root_cache_key,
                                invalidation_keys,
                                self.debug,
                            )
                            .await?;
                        }

                        Ok(response)
                    }
                }
            } else {
                let response = self.service.call(request).await?;

                Ok(response)
            }
        } else {
            match cache_lookup_entities(
                self.name.clone(),
                self.supergraph_schema.clone(),
                &self.subgraph_enums,
                storage.clone(),
                is_known_private,
                private_id.as_deref(),
                request,
                self.debug,
            )
            .instrument(tracing::info_span!(
                "response_cache.lookup",
                kind = "entity",
                debug = self.debug,
                private = is_known_private,
                contains_private_id = private_id.is_some()
            ))
            .await?
            {
                ControlFlow::Break(response) => Ok(response),
                ControlFlow::Continue((request, mut cache_result)) => {
                    let context = request.context.clone();
                    let mut debug_subgraph_request = None;
                    if self.debug {
                        debug_subgraph_request = Some(request.subgraph_request.body().clone());
                        let debug_cache_keys_ctx = cache_result.0.iter().filter_map(|ir| {
                            ir.cache_entry.as_ref().map(|cache_entry| CacheKeyContext {
                                hashed_private_id: private_id.clone(),
                                key: cache_entry.key.clone(),
                                invalidation_keys: ir.invalidation_keys.clone().into_iter()
                                .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                                .collect(),
                                kind: CacheEntryKind::Entity {
                                    typename: ir.typename.clone(),
                                    entity_key: ir.entity_key.clone(),
                                },
                                subgraph_name: self.name.clone(),
                                subgraph_request: request.subgraph_request.body().clone(),
                                source: CacheKeySource::Cache,
                                cache_control: cache_entry.control.clone(),
                                data: serde_json_bytes::json!({
                                    "data": serde_json_bytes::to_value(cache_entry.data.clone()).unwrap_or_default()
                                }),
                            })
                        });
                        request.context.upsert::<_, CacheKeysContext>(
                            CONTEXT_DEBUG_CACHE_KEYS,
                            |mut val| {
                                val.extend(debug_cache_keys_ctx);

                                val
                            },
                        )?;
                    }

                    let mut response = match self.service.call(request).await {
                        Ok(response) => response,
                        Err(e) => {
                            let e = match e.downcast::<FetchError>() {
                                Ok(inner) => match *inner {
                                    FetchError::SubrequestHttpError { .. } => *inner,
                                    _ => FetchError::SubrequestHttpError {
                                        status_code: None,
                                        service: self.name.to_string(),
                                        reason: inner.to_string(),
                                    },
                                },
                                Err(e) => FetchError::SubrequestHttpError {
                                    status_code: None,
                                    service: self.name.to_string(),
                                    reason: e.to_string(),
                                },
                            };

                            let graphql_error = e.to_graphql_error(None);

                            let (new_entities, new_errors) = assemble_response_from_errors(
                                &[graphql_error],
                                &mut cache_result.0,
                            );

                            let mut data = Object::default();
                            data.insert(ENTITIES, new_entities.into());

                            let mut response = subgraph::Response::builder()
                                .context(context)
                                .data(Value::Object(data))
                                .errors(new_errors)
                                .subgraph_name(self.name)
                                .extensions(Object::new())
                                .build();
                            CacheControl::no_store().to_headers(response.response.headers_mut())?;

                            return Ok(response);
                        }
                    };

                    let mut cache_control = if response
                        .response
                        .headers()
                        .contains_key(CACHE_CONTROL)
                    {
                        CacheControl::new(response.response.headers(), self.subgraph_ttl.into())?
                    } else {
                        CacheControl::no_store()
                    };

                    if let Some(control_from_cached) = cache_result.1 {
                        cache_control = cache_control.merge(&control_from_cached);
                    }

                    if !is_known_private && cache_control.private() {
                        self.private_queries
                            .write()
                            .await
                            .put(private_query_key, ());
                    }

                    cache_store_entities_from_response(
                        storage,
                        self.subgraph_ttl,
                        &mut response,
                        cache_control.clone(),
                        cache_result.0,
                        is_known_private,
                        private_id,
                        debug_subgraph_request,
                    )
                    .await?;

                    cache_control.to_headers(response.response.headers_mut())?;

                    Ok(response)
                }
            }
        }
    }

    fn get_private_id(&self, context: &Context) -> Option<String> {
        self.private_id.as_ref().and_then(|key| {
            context.get_json_value(key).and_then(|value| {
                value.as_str().map(|s| {
                    let mut digest = blake3::Hasher::new();
                    digest.update(s.as_bytes());
                    digest.finalize().to_hex().to_string()
                })
            })
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn cache_lookup_root(
    name: String,
    entity_type_opt: Option<&str>,
    cache: Storage,
    is_known_private: bool,
    private_id: Option<&str>,
    debug: bool,
    mut request: subgraph::Request,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
) -> Result<ControlFlow<subgraph::Response, (subgraph::Request, String, Vec<String>)>, BoxError> {
    let invalidation_cache_keys =
        get_invalidation_root_keys_from_schema(&request, subgraph_enums, supergraph_schema)?;
    let body = request.subgraph_request.body_mut();
    body.variables.sort_keys();

    let (key, mut invalidation_keys) = extract_cache_key_root(
        &name,
        entity_type_opt,
        &request.query_hash,
        body,
        &request.context,
        &request.authorization,
        is_known_private,
        private_id,
    );
    invalidation_keys.extend(invalidation_cache_keys);

    Span::current().record("cache.key", key.clone());

    match cache.fetch(&key, &request.subgraph_name).await {
        Ok(value) => {
            if value.control.can_use() {
                let control = value.control.clone();
                update_cache_control(&request.context, &control);
                if debug {
                    let root_operation_fields: Vec<String> = request
                        .executable_document
                        .as_ref()
                        .and_then(|executable_document| {
                            Some(
                                executable_document
                                    .operations
                                    .iter()
                                    .next()?
                                    .root_fields(executable_document)
                                    .map(|f| f.name.to_string())
                                    .collect(),
                            )
                        })
                        .unwrap_or_default();

                    request.context.upsert::<_, CacheKeysContext>(
                        CONTEXT_DEBUG_CACHE_KEYS,
                        |mut val| {
                            val.push(CacheKeyContext {
                                key: value.key.clone(),
                                hashed_private_id: private_id.map(ToString::to_string),
                                invalidation_keys: invalidation_keys
                                    .clone()
                                    .into_iter()
                                    .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                                    .collect(),
                                kind: CacheEntryKind::RootFields {
                                    root_fields: root_operation_fields,
                                },
                                subgraph_name: request.subgraph_name.clone(),
                                subgraph_request: request.subgraph_request.body().clone(),
                                source: CacheKeySource::Cache,
                                cache_control: value.control.clone(),
                                data: serde_json_bytes::json!({"data": value.data.clone()}),
                            });

                            val
                        },
                    )?;
                }

                Span::current().set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("hit".into()),
                );
                let mut response = subgraph::Response::builder()
                    .data(value.data)
                    .extensions(Object::new())
                    .context(request.context)
                    .subgraph_name(request.subgraph_name.clone())
                    .build();

                value.control.to_headers(response.response.headers_mut())?;
                Ok(ControlFlow::Break(response))
            } else {
                Span::current().set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("miss".into()),
                );
                Ok(ControlFlow::Continue((request, key, invalidation_keys)))
            }
        }
        Err(err) => {
            let span = Span::current();
            if !err.is_row_not_found() {
                span.mark_as_error(format!("cannot get cache entry: {err}"));
            }

            span.set_span_dyn_attribute(
                opentelemetry::Key::new("cache.status"),
                opentelemetry::Value::String("miss".into()),
            );
            Ok(ControlFlow::Continue((request, key, invalidation_keys)))
        }
    }
}

fn get_invalidation_root_keys_from_schema(
    request: &subgraph::Request,
    subgraph_enums: &HashMap<String, String>,
    supergraph_schema: Arc<Valid<Schema>>,
) -> Result<HashSet<String>, anyhow::Error> {
    let subgraph_name = &request.subgraph_name;
    let executable_document =
        request
            .executable_document
            .as_ref()
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "cannot get the executable document for subgraph request".to_string(),
            })?;
    let root_operation_fields = executable_document
        .operations
        .get(request.subgraph_request.body().operation_name.as_deref())
        .map_err(|_err| FetchError::MalformedRequest {
            reason: "cannot get the operation from executable document for subgraph request"
                .to_string(),
        })?
        .root_fields(executable_document);
    let root_query_type = supergraph_schema
        .root_operation(apollo_compiler::ast::OperationType::Query)
        .ok_or_else(|| FetchError::MalformedRequest {
            reason: "cannot get the root operation from supergraph schema".to_string(),
        })?;
    let query_object_type = supergraph_schema
        .get_object(root_query_type.as_str())
        .ok_or_else(|| FetchError::MalformedRequest {
            reason: "cannot get the root query type from supergraph schema".to_string(),
        })?;

    let cache_keys = root_operation_fields
        .map(|field| {
            // We don't use field.definition because we need the directive set in supergraph schema not in the executable document
            let field_def = query_object_type.fields.get(&field.name).ok_or_else(|| {
                FetchError::MalformedRequest {
                    reason: "cannot get the field definition from supergraph schema".to_string(),
                }
            })?;
            let cache_keys = field_def
                .directives
                .get_all("join__directive")
                .filter_map(|dir| {
                    let name = dir.argument_by_name("name", &supergraph_schema).ok()?;
                    if name.as_str()? != CACHE_TAG_DIRECTIVE_NAME {
                        return None;
                    }
                    let is_current_subgraph =
                        dir.argument_by_name("graphs", &supergraph_schema)
                            .ok()
                            .and_then(|f| {
                                Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
                                    |g| {
                                        subgraph_enums.get(g.as_str()).map(|s| s.as_str())
                                            == Some(subgraph_name)
                                    },
                                ))
                            })
                            .unwrap_or_default();
                    if !is_current_subgraph {
                        return None;
                    }
                    let mut format = None;
                    for (field_name, value) in dir
                        .argument_by_name("args", &supergraph_schema)
                        .ok()?
                        .as_object()?
                    {
                        if field_name.as_str() == "format" {
                            format = value
                                .as_str()
                                .and_then(|v| v.parse::<StringTemplate>().ok())
                        }
                    }
                    format
                });
            let mut errors = Vec::new();
            // Query::validate_variables runs before this
            let variable_values =
                Valid::assume_valid_ref(&request.subgraph_request.body().variables);
            let args = coerce_argument_values(
                &supergraph_schema,
                executable_document,
                variable_values,
                &mut errors,
                Default::default(),
                field_def,
                field,
            )
            .map_err(|_| FetchError::MalformedRequest {
                reason: format!("cannot argument values for root fields {:?}", field.name),
            })?;

            if !errors.is_empty() {
                return Err(FetchError::MalformedRequest {
                    reason: format!(
                        "cannot coerce argument values for root fields {:?}, errors: {errors:?}",
                        field.name,
                    ),
                }
                .into());
            }

            let mut vars = IndexMap::default();
            vars.insert("$args".to_string(), Value::Object(args));
            cache_keys
                .map(|ck| Ok(ck.interpolate(&vars).map(|(res, _)| res)?))
                .collect::<Result<Vec<String>, anyhow::Error>>()
        })
        .collect::<Result<Vec<Vec<String>>, anyhow::Error>>()?;

    let invalidation_cache_keys: HashSet<String> = cache_keys.into_iter().flatten().collect();

    Ok(invalidation_cache_keys)
}

struct ResponseCacheResults(Vec<IntermediateResult>, Option<CacheControl>);

#[allow(clippy::too_many_arguments)]
async fn cache_lookup_entities(
    name: String,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
    cache: Storage,
    is_known_private: bool,
    private_id: Option<&str>,
    mut request: subgraph::Request,
    debug: bool,
) -> Result<ControlFlow<subgraph::Response, (subgraph::Request, ResponseCacheResults)>, BoxError> {
    let cache_metadata = extract_cache_keys(
        &name,
        supergraph_schema,
        subgraph_enums,
        &mut request,
        is_known_private,
        private_id,
    )?;
    let keys_len = cache_metadata.len();

    let cache_keys = cache_metadata
        .iter()
        .map(|k| k.cache_key.as_str())
        .collect::<Vec<&str>>();
    let cache_result = cache.fetch_multiple(&cache_keys, &name).await;
    Span::current().set_span_dyn_attribute(
        "cache.keys".into(),
        opentelemetry::Value::Array(Array::String(
            cache_keys
                .into_iter()
                .map(|ck| StringValue::from(ck.to_string()))
                .collect(),
        )),
    );

    let cache_result: Vec<Option<CacheEntry>> = match cache_result {
        Ok(res) => res
            .into_iter()
            .map(|v| match v {
                Some(v) if v.control.can_use() => Some(v),
                _ => None,
            })
            .collect(),
        Err(err) => {
            if !err.is_row_not_found() {
                let span = Span::current();
                span.mark_as_error(format!("cannot get cache entry: {err}"));
            }

            std::iter::repeat_n(None, keys_len).collect()
        }
    };
    let body = request.subgraph_request.body_mut();

    let representations = body
        .variables
        .get_mut(REPRESENTATIONS)
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");
    // remove from representations the entities we already obtained from the cache
    let (new_representations, cache_result, cache_control) = filter_representations(
        &name,
        representations,
        cache_metadata,
        cache_result,
        &request.context,
    )?;

    if !new_representations.is_empty() {
        body.variables
            .insert(REPRESENTATIONS, new_representations.into());
        let cache_status = if cache_result.is_empty() {
            opentelemetry::Value::String("miss".into())
        } else {
            opentelemetry::Value::String("partial_hit".into())
        };
        Span::current()
            .set_span_dyn_attribute(opentelemetry::Key::new("cache.status"), cache_status);

        Ok(ControlFlow::Continue((
            request,
            ResponseCacheResults(cache_result, cache_control),
        )))
    } else {
        if debug {
            let debug_cache_keys_ctx = cache_result.iter().filter_map(|ir| {
                ir.cache_entry.as_ref().map(|cache_entry| CacheKeyContext {
                    key: ir.key.clone(),
                    hashed_private_id: private_id.map(ToString::to_string),
                    invalidation_keys: ir
                        .invalidation_keys
                        .clone()
                        .into_iter()
                        .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                        .collect(),
                    kind: CacheEntryKind::Entity {
                        typename: ir.typename.clone(),
                        entity_key: ir.entity_key.clone(),
                    },
                    subgraph_name: name.clone(),
                    subgraph_request: request.subgraph_request.body().clone(),
                    source: CacheKeySource::Cache,
                    cache_control: cache_entry.control.clone(),
                    data: serde_json_bytes::json!({"data": cache_entry.data.clone()}),
                })
            });
            request.context.upsert::<_, CacheKeysContext>(
                CONTEXT_DEBUG_CACHE_KEYS,
                |mut val| {
                    val.extend(debug_cache_keys_ctx);

                    val
                },
            )?;
        }
        Span::current().set_span_dyn_attribute(
            opentelemetry::Key::new("cache.status"),
            opentelemetry::Value::String("hit".into()),
        );

        let entities = cache_result
            .into_iter()
            .filter_map(|res| res.cache_entry)
            .map(|entry| entry.data)
            .collect::<Vec<_>>();
        let mut data = Object::default();
        data.insert(ENTITIES, entities.into());

        let mut response = subgraph::Response::builder()
            .data(data)
            .extensions(Object::new())
            .subgraph_name(request.subgraph_name)
            .context(request.context)
            .build();

        cache_control
            .unwrap_or_default()
            .to_headers(response.response.headers_mut())?;

        Ok(ControlFlow::Break(response))
    }
}

fn update_cache_control(context: &Context, cache_control: &CacheControl) {
    context.extensions().with_lock(|lock| {
        if let Some(c) = lock.get_mut::<CacheControl>() {
            *c = c.merge(cache_control);
        } else {
            // Go through the "merge" algorithm even with a single value
            // in order to keep single-fetch queries consistent between cache hit and miss,
            // and with multi-fetch queries.
            let new_cache_control = cache_control.merge(cache_control);
            lock.insert(new_cache_control);
        }
    })
}

async fn cache_store_root_from_response(
    cache: Storage,
    default_subgraph_ttl: Duration,
    response: &subgraph::Response,
    cache_control: CacheControl,
    cache_key: String,
    mut invalidation_keys: Vec<String>,
    _debug: bool,
) -> Result<(), BoxError> {
    if let Some(data) = response.response.body().data.as_ref() {
        let ttl = cache_control
            .ttl()
            .map(|secs| Duration::from_secs(secs as u64))
            .unwrap_or(default_subgraph_ttl);

        if response.response.body().errors.is_empty() && cache_control.should_store() {
            // Support surrogate keys coming from subgraph response extensions
            if let Some(Value::Array(cache_tags)) = response
                .response
                .body()
                .extensions
                .get(GRAPHQL_RESPONSE_EXTENSION_ROOT_FIELDS_CACHE_TAGS)
            {
                invalidation_keys.extend(
                    cache_tags
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_owned()),
                );
            }

            let document = Document {
                key: cache_key,
                data: data.clone(),
                control: cache_control,
                invalidation_keys,
                expire: ttl,
            };

            let subgraph_name = response.subgraph_name.clone();
            let span = tracing::info_span!("response_cache.store", "kind" = "root", "subgraph.name" = subgraph_name.clone(), "ttl" = ?ttl);

            // Write to cache in a non-awaited task so that it's not on the request’s critical path
            tokio::spawn(async move {
                let _ = cache
                    .insert(document, &subgraph_name)
                    .instrument(span)
                    .await;
            });
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cache_store_entities_from_response(
    cache: Storage,
    default_subgraph_ttl: Duration,
    response: &mut subgraph::Response,
    cache_control: CacheControl,
    mut result_from_cache: Vec<IntermediateResult>,
    is_known_private: bool,
    private_id: Option<String>,
    // Only Some if debug is enabled
    subgraph_request: Option<graphql::Request>,
) -> Result<(), BoxError> {
    let mut data = response.response.body_mut().data.take();

    if let Some(mut entities) = data
        .as_mut()
        .and_then(|v| v.as_object_mut())
        .and_then(|o| o.remove(ENTITIES))
    {
        // if the scope is private but we do not have a way to differentiate users, do not store anything in the cache
        let should_cache_private = !cache_control.private() || private_id.is_some();

        let update_key_private = if !is_known_private && cache_control.private() {
            private_id
        } else {
            None
        };

        // Support surrogate keys coming from subgraph extensions
        let per_entity_surrogate_keys = response
            .response
            .body()
            .extensions
            .get(GRAPHQL_RESPONSE_EXTENSION_ENTITY_CACHE_TAGS)
            .and_then(|value| value.as_array())
            .map(|vec| vec.as_slice())
            .unwrap_or_default();

        let (new_entities, new_errors) = insert_entities_in_result(
            entities
                .as_array_mut()
                .ok_or_else(|| FetchError::MalformedResponse {
                    reason: "expected an array of entities".to_string(),
                })?,
            &response.response.body().errors,
            cache,
            default_subgraph_ttl,
            cache_control,
            &mut result_from_cache,
            update_key_private,
            should_cache_private,
            &response.subgraph_name,
            per_entity_surrogate_keys,
            response.context.clone(),
            subgraph_request,
        )
        .await?;

        data.as_mut()
            .and_then(|v| v.as_object_mut())
            .map(|o| o.insert(ENTITIES, new_entities.into()));
        response.response.body_mut().data = data;
        response.response.body_mut().errors = new_errors;
    } else {
        let (new_entities, new_errors) =
            assemble_response_from_errors(&response.response.body().errors, &mut result_from_cache);

        let mut data = Object::default();
        data.insert(ENTITIES, new_entities.into());

        response.response.body_mut().data = Some(Value::Object(data));
        response.response.body_mut().errors = new_errors;
    }

    Ok(())
}

// build a cache key for the root operation
#[allow(clippy::too_many_arguments)]
fn extract_cache_key_root(
    subgraph_name: &str,
    entity_type_opt: Option<&str>,
    query_hash: &QueryHash,
    body: &graphql::Request,
    context: &Context,
    cache_key: &CacheKeyMetadata,
    is_known_private: bool,
    private_id: Option<&str>,
) -> (String, Vec<String>) {
    let entity_type = entity_type_opt.unwrap_or("Query");

    let key = PrimaryCacheKeyRoot {
        subgraph_name,
        graphql_type: entity_type,
        subgraph_query_hash: query_hash,
        body,
        context,
        auth_cache_key_metadata: cache_key,
        private_id: if is_known_private { private_id } else { None },
    }
    .hash();
    let invalidation_keys = vec![format!(
        "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}"
    )];

    (key, invalidation_keys)
}

struct CacheMetadata {
    cache_key: String,
    invalidation_keys: Vec<String>,
    entity_key: serde_json_bytes::Map<ByteString, Value>,
}

// build a list of keys to get from the cache in one query
#[allow(clippy::too_many_arguments)]
fn extract_cache_keys(
    subgraph_name: &str,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
    request: &mut subgraph::Request,
    is_known_private: bool,
    private_id: Option<&str>,
) -> Result<Vec<CacheMetadata>, BoxError> {
    let context = &request.context;
    let authorization = &request.authorization;
    // hash the query and operation name
    let query_hash = hash_query(&request.query_hash);
    // hash more data like variables and authorization status
    let additional_data_hash =
        hash_additional_data(request.subgraph_request.body_mut(), context, authorization);

    let representations = request
        .subgraph_request
        .body_mut()
        .variables
        .get_mut(REPRESENTATIONS)
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");

    // Get entity key to only get the right fields in representations
    let mut res = Vec::with_capacity(representations.len());
    let mut entities = HashMap::new();
    let mut typenames = HashSet::new();
    for representation in representations {
        let representation =
            representation
                .as_object_mut()
                .ok_or_else(|| FetchError::MalformedRequest {
                    reason: "representation variable should be an array of object".to_string(),
                })?;
        let typename_value =
            representation
                .remove(TYPENAME)
                .ok_or_else(|| FetchError::MalformedRequest {
                    reason: "missing __typename in representation".to_string(),
                })?;

        let typename = typename_value
            .as_str()
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "__typename in representation is not a string".to_string(),
            })?;
        typenames.insert(typename.to_string());
        match entities.get_mut(typename) {
            Some(entity_nb) => *entity_nb += 1,
            None => {
                entities.insert(typename.to_string(), 1u64);
            }
        }

        // Split `representation` into two parts: the entity key part and the rest.
        let representation_entity_key = take_matching_key_field_set(
            representation,
            typename,
            subgraph_name,
            &supergraph_schema,
            subgraph_enums,
        )?;

        // Create primary cache key for an entity
        let key = PrimaryCacheKeyEntity {
            subgraph_name,
            entity_type: typename,
            representation,
            entity_key: &representation_entity_key,
            subgraph_query_hash: &query_hash,
            additional_data_hash: &additional_data_hash,
            private_id: if is_known_private { private_id } else { None },
        }
        .hash();

        // Used as a surrogate cache key
        let mut invalidation_keys = vec![format!(
            "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{typename}"
        )];

        // get cache keys from directive
        let invalidation_cache_keys = get_invalidation_entity_keys_from_schema(
            &supergraph_schema,
            subgraph_name,
            subgraph_enums,
            typename,
            &representation_entity_key,
        )?;

        // Restore the `representation` back whole again
        representation.insert(TYPENAME, typename_value);
        merge_representation(representation, representation_entity_key.clone()); //FIXME: not always clone, only on debug
        invalidation_keys.extend(invalidation_cache_keys);
        let cache_key_metadata = CacheMetadata {
            cache_key: key,
            invalidation_keys,
            entity_key: representation_entity_key,
        };
        res.push(cache_key_metadata);
    }

    Span::current().set_span_dyn_attribute(
        Key::from_static_str("graphql.types"),
        opentelemetry::Value::Array(
            typenames
                .into_iter()
                .map(StringValue::from)
                .collect::<Vec<StringValue>>()
                .into(),
        ),
    );

    for (typename, entity_nb) in entities {
        u64_histogram_with_unit!(
            "apollo.router.operations.response_cache.fetch.entity",
            "Number of entities per subgraph fetch node",
            "{entity}",
            entity_nb,
            "subgraph.name" = subgraph_name.to_string(),
            "graphql.type" = typename
        );
    }

    Ok(res)
}

/// Get invalidation keys from @cacheTag directives in supergraph schema for entities
fn get_invalidation_entity_keys_from_schema(
    supergraph_schema: &Arc<Valid<Schema>>,
    subgraph_name: &str,
    subgraph_enums: &HashMap<String, String>,
    typename: &str,
    entity_keys: &serde_json_bytes::Map<ByteString, Value>,
) -> Result<HashSet<String>, anyhow::Error> {
    let field_def =
        supergraph_schema
            .get_object(typename)
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "can't find corresponding type for __typename {typename:?}".to_string(),
            })?;
    let cache_keys = field_def
        .directives
        .get_all("join__directive")
        .filter_map(|dir| {
            let name = dir.argument_by_name("name", supergraph_schema).ok()?;
            if name.as_str()? != CACHE_TAG_DIRECTIVE_NAME {
                return None;
            }
            let is_current_subgraph = dir
                .argument_by_name("graphs", supergraph_schema)
                .ok()
                .and_then(|f| {
                    Some(
                        f.as_list()?
                            .iter()
                            .filter_map(|graph| graph.as_enum())
                            .any(|g| {
                                subgraph_enums.get(g.as_str()).map(|s| s.as_str())
                                    == Some(subgraph_name)
                            }),
                    )
                })
                .unwrap_or_default();
            if !is_current_subgraph {
                return None;
            }
            dir.argument_by_name("args", supergraph_schema)
                .ok()?
                .as_object()?
                .iter()
                .find_map(|(field_name, value)| {
                    if field_name.as_str() == "format" {
                        value.as_str()?.parse::<StringTemplate>().ok()
                    } else {
                        None
                    }
                })
        });
    let mut vars = IndexMap::default();
    vars.insert("$key".to_string(), Value::Object(entity_keys.clone()));
    let invalidation_cache_keys = cache_keys
        .map(|ck| ck.interpolate(&vars).map(|(res, _)| res))
        .collect::<Result<HashSet<String>, apollo_federation::connectors::StringTemplateError>>()?;
    Ok(invalidation_cache_keys)
}

fn take_matching_key_field_set(
    representation: &mut serde_json_bytes::Map<ByteString, Value>,
    typename: &str,
    subgraph_name: &str,
    supergraph_schema: &Valid<Schema>,
    subgraph_enums: &HashMap<String, String>,
) -> Result<serde_json_bytes::Map<ByteString, Value>, FetchError> {
    // find an entry in the `key_field_sets` that matches the `representation`.
    let matched_key_field_set =
        collect_key_field_sets(typename, subgraph_name, supergraph_schema, subgraph_enums)?
        .find(|field_set| {
            matches_selection_set(representation, &field_set.selection_set)
        })
        .ok_or_else(|| {
            tracing::trace!("representation does not match any key field set for typename {typename} in subgraph {subgraph_name}");
            FetchError::MalformedRequest {
                reason: format!("unexpected critical internal error for typename {typename} in subgraph {subgraph_name}"),
            }
        })?;
    take_selection_set(representation, &matched_key_field_set.selection_set).ok_or_else(|| {
        FetchError::MalformedRequest {
            reason: format!("representation does not match the field set {matched_key_field_set}"),
        }
    })
}

// Collect `@key` field sets on a `typename` in a `subgraph_name`.
// - Returns a Vec of FieldSet, since there may be more than one @key directives in the subgraph.
fn collect_key_field_sets(
    typename: &str,
    subgraph_name: &str,
    supergraph_schema: &Valid<Schema>,
    subgraph_enums: &HashMap<String, String>,
) -> Result<impl Iterator<Item = apollo_compiler::executable::FieldSet>, FetchError> {
    Ok(supergraph_schema
        .types
        .get(typename)
        .ok_or_else(|| FetchError::MalformedRequest {
            reason: format!("unknown typename {typename:?} in representations"),
        })?
        .directives()
        .get_all("join__type")
        .filter_map(move |directive| {
            let schema_subgraph_name = directive
                .specified_argument_by_name("graph")
                .and_then(|arg| arg.as_enum())
                .and_then(|arg| subgraph_enums.get(arg.as_str()))?;

            if schema_subgraph_name == subgraph_name {
                let mut parser = Parser::new();
                directive
                    .specified_argument_by_name("key")
                    .and_then(|arg| arg.as_str())
                    .and_then(|arg| {
                        parser
                            .parse_field_set(
                                supergraph_schema,
                                NamedType::new(typename).ok()?,
                                arg,
                                "response_caching.graphql",
                            )
                            .ok()
                    })
            } else {
                None
            }
        }))
}

// Does the shape of `representation`  match the `selection_set`?
fn matches_selection_set(
    representation: &serde_json_bytes::Map<ByteString, Value>,
    selection_set: &apollo_compiler::executable::SelectionSet,
) -> bool {
    for field in selection_set.root_fields(&Default::default()) {
        // Note: field sets can't have aliases.
        let Some(value) = representation.get(field.name.as_str()) else {
            return false;
        };

        if field.selection_set.is_empty() {
            // `value` must be a scalar.
            if matches!(value, Value::Object(_)) {
                return false;
            }
            continue;
        }

        // Check the sub-selection set.
        let Value::Object(sub_value) = value else {
            return false;
        };
        if !matches_selection_set(sub_value, &field.selection_set) {
            return false;
        }
    }
    true
}

// Removes the selection set from `representation` and returns the value corresponding to it.
// - Returns None if the representation doesn't match the selection set.
fn take_selection_set(
    representation: &mut serde_json_bytes::Map<ByteString, Value>,
    selection_set: &apollo_compiler::executable::SelectionSet,
) -> Option<serde_json_bytes::Map<ByteString, Value>> {
    let mut result = serde_json_bytes::Map::new();
    for field in selection_set.root_fields(&Default::default()) {
        // Note: field sets can't have aliases.
        if field.selection_set.is_empty() {
            let value = representation.remove(field.name.as_str())?;
            // `value` must be a scalar.
            if matches!(value, Value::Object(_)) {
                return None;
            }
            // Move the scalar field to the `result`.
            result.insert(ByteString::from(field.name.as_str()), value);
            continue;
        } else {
            let value = representation.get_mut(field.name.as_str())?;
            // Update the sub-selection set.
            let Value::Object(sub_value) = value else {
                return None;
            };
            let removed = take_selection_set(sub_value, &field.selection_set)?;
            result.insert(
                ByteString::from(field.name.as_str()),
                Value::Object(removed),
            );
        }
    }
    Some(result)
}

// The inverse of `take_selection_set`.
fn merge_representation(
    dest: &mut serde_json_bytes::Map<ByteString, Value>,
    source: serde_json_bytes::Map<ByteString, Value>,
) {
    source.into_iter().for_each(|(key, src_value)| {
        // Note: field sets can't have aliases.
        let Some(dest_value) = dest.get_mut(&key) else {
            dest.insert(key, src_value);
            return;
        };

        // Overlapping fields must be objects.
        if let (Value::Object(dest_sub_value), Value::Object(src_sub_value)) =
            (dest_value, src_value)
        {
            // Merge sub-values
            merge_representation(dest_sub_value, src_sub_value);
        }
    });
}

/// represents the result of a cache lookup for an entity type and key
struct IntermediateResult {
    key: String,
    invalidation_keys: Vec<String>,
    typename: String,
    entity_key: serde_json_bytes::Map<ByteString, Value>,
    cache_entry: Option<CacheEntry>,
}

// build a new list of representations without the ones we got from the cache
#[allow(clippy::type_complexity)]
fn filter_representations(
    subgraph_name: &str,
    representations: &mut Vec<Value>,
    // keys: Vec<(String, Vec<String>)>,
    keys: Vec<CacheMetadata>,
    mut cache_result: Vec<Option<CacheEntry>>,
    context: &Context,
) -> Result<(Vec<Value>, Vec<IntermediateResult>, Option<CacheControl>), BoxError> {
    let mut new_representations: Vec<Value> = Vec::new();
    let mut result = Vec::new();
    let mut cache_hit: HashMap<String, CacheHitMiss> = HashMap::new();
    let mut cache_control = None;

    for (
        (
            mut representation,
            CacheMetadata {
                cache_key: key,
                invalidation_keys,
                entity_key,
                ..
            },
        ),
        mut cache_entry,
    ) in representations
        .drain(..)
        .zip(keys)
        .zip(cache_result.drain(..))
    {
        let opt_type = representation
            .as_object_mut()
            .and_then(|o| o.remove(TYPENAME))
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "missing __typename in representation".to_string(),
            })?;

        let typename = opt_type.as_str().unwrap_or("-").to_string();

        // do not use that cache entry if it is stale
        if let Some(false) = cache_entry.as_ref().map(|c| c.control.can_use()) {
            cache_entry = None;
        }
        match cache_entry.as_ref() {
            None => {
                cache_hit.entry(typename.clone()).or_default().miss += 1;

                representation
                    .as_object_mut()
                    .map(|o| o.insert(TYPENAME, opt_type));
                new_representations.push(representation);
            }
            Some(entry) => {
                cache_hit.entry(typename.clone()).or_default().hit += 1;
                match cache_control.as_mut() {
                    None => cache_control = Some(entry.control.clone()),
                    Some(c) => *c = c.merge(&entry.control),
                }
            }
        }

        result.push(IntermediateResult {
            key,
            invalidation_keys,
            typename,
            cache_entry,
            entity_key,
        });
    }

    let _ = context.insert(
        CacheMetricContextKey::new(subgraph_name.to_string()),
        CacheSubgraph(cache_hit),
    );

    Ok((new_representations, result, cache_control))
}

// fill in the entities for the response
#[allow(clippy::too_many_arguments)]
async fn insert_entities_in_result(
    entities: &mut Vec<Value>,
    errors: &[Error],
    cache: Storage,
    default_subgraph_ttl: Duration,
    cache_control: CacheControl,
    result: &mut Vec<IntermediateResult>,
    update_key_private: Option<String>,
    should_cache_private: bool,
    subgraph_name: &str,
    per_entity_surrogate_keys: &[Value],
    context: Context,
    // Only Some if debug is enabled
    subgraph_request: Option<graphql::Request>,
) -> Result<(Vec<Value>, Vec<Error>), BoxError> {
    let ttl = cache_control
        .ttl()
        .map(|secs| Duration::from_secs(secs as u64))
        .unwrap_or(default_subgraph_ttl);

    let mut new_entities = Vec::new();
    let mut new_errors = Vec::new();

    let mut inserted_types: HashMap<String, usize> = HashMap::new();
    let mut to_insert: Vec<_> = Vec::new();
    let mut debug_ctx_entries = Vec::new();
    let mut entities_it = entities.drain(..).enumerate();
    let mut per_entity_surrogate_keys_it = per_entity_surrogate_keys.iter();

    // insert requested entities and cached entities in the same order as
    // they were requested
    for (
        new_entity_idx,
        IntermediateResult {
            mut key,
            mut invalidation_keys,
            typename,
            cache_entry,
            entity_key,
        },
    ) in result.drain(..).enumerate()
    {
        match cache_entry {
            Some(v) => {
                new_entities.push(v.data);
            }
            None => {
                let (entity_idx, value) =
                    entities_it
                        .next()
                        .ok_or_else(|| FetchError::MalformedResponse {
                            reason: "invalid number of entities".to_string(),
                        })?;
                let specific_surrogate_keys = per_entity_surrogate_keys_it.next();

                *inserted_types.entry(typename.clone()).or_default() += 1;

                if let Some(ref id) = update_key_private {
                    key = format!("{key}:{id}");
                }

                let mut has_errors = false;
                for error in errors.iter().filter(|e| {
                    e.path
                        .as_ref()
                        .map(|path| {
                            path.starts_with(&Path(vec![
                                PathElement::Key(ENTITIES.to_string(), None),
                                PathElement::Index(entity_idx),
                            ]))
                        })
                        .unwrap_or(false)
                }) {
                    // update the entity index, because it does not match with the original one
                    let mut e = error.clone();
                    if let Some(path) = e.path.as_mut() {
                        path.0[1] = PathElement::Index(new_entity_idx);
                    }

                    new_errors.push(e);
                    has_errors = true;
                }

                // Only in debug mode
                if let Some(subgraph_request) = &subgraph_request {
                    debug_ctx_entries.push(CacheKeyContext {
                        key: key.clone(),
                        hashed_private_id: update_key_private.clone(),
                        invalidation_keys: invalidation_keys
                            .clone()
                            .into_iter()
                            .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                            .collect(),
                        kind: CacheEntryKind::Entity {
                            typename: typename.clone(),
                            entity_key: entity_key.clone(),
                        },
                        subgraph_name: subgraph_name.to_string(),
                        subgraph_request: subgraph_request.clone(),
                        source: CacheKeySource::Subgraph,
                        cache_control: cache_control.clone(),
                        data: serde_json_bytes::json!({"data": value.clone()}),
                    });
                }
                if !has_errors && cache_control.should_store() && should_cache_private {
                    if let Some(Value::Array(keys)) = specific_surrogate_keys {
                        invalidation_keys
                            .extend(keys.iter().filter_map(|v| v.as_str()).map(|s| s.to_owned()));
                    }
                    to_insert.push(Document {
                        control: cache_control.clone(),
                        data: value.clone(),
                        key,
                        invalidation_keys,
                        expire: ttl,
                    });
                }

                new_entities.push(value);
            }
        }
    }

    // For debug mode
    if !debug_ctx_entries.is_empty() {
        context.upsert::<_, CacheKeysContext>(CONTEXT_DEBUG_CACHE_KEYS, |mut val| {
            val.extend(debug_ctx_entries);
            val
        })?;
    }

    if !to_insert.is_empty() {
        let batch_size = to_insert.len();
        let span = tracing::info_span!("response_cache.store", "kind" = "entity", "subgraph.name" = subgraph_name, "ttl" = ?ttl, "batch.size" = %batch_size);
        let subgraph_name = subgraph_name.to_string();

        // Write to cache in a non-awaited task so that it's not on the request’s critical path
        tokio::spawn(async move {
            let _ = cache
                .insert_in_batch(to_insert, &subgraph_name)
                .instrument(span)
                .await;
        });
    }

    for (ty, nb) in inserted_types {
        tracing::event!(Level::TRACE, entity_type = ty.as_str(), cache_insert = nb,);
    }

    Ok((new_entities, new_errors))
}

fn assemble_response_from_errors(
    graphql_errors: &[Error],
    result: &mut Vec<IntermediateResult>,
) -> (Vec<Value>, Vec<Error>) {
    let mut new_entities = Vec::new();
    let mut new_errors = Vec::new();

    for (new_entity_idx, IntermediateResult { cache_entry, .. }) in result.drain(..).enumerate() {
        match cache_entry {
            Some(v) => {
                new_entities.push(v.data);
            }
            None => {
                new_entities.push(Value::Null);

                for mut error in graphql_errors.iter().cloned() {
                    error.path = Some(Path(vec![
                        PathElement::Key(ENTITIES.to_string(), None),
                        PathElement::Index(new_entity_idx),
                    ]));
                    new_errors.push(error);
                }
            }
        }
    }
    (new_entities, new_errors)
}

async fn check_connection(
    postgres_config: storage::postgres::Config,
    cache_storage: Arc<OnceLock<Storage>>,
    mut abort_signal: Receiver<()>,
    subgraph_name: Option<String>,
) {
    let mut interval =
        IntervalStream::new(tokio::time::interval(std::time::Duration::from_secs(30)));
    let abort_signal_cloned = abort_signal.resubscribe();
    loop {
        tokio::select! {
            biased;
            _ = abort_signal.recv() => {
                break;
            }
            _ = interval.next() => {
                u64_counter_with_unit!(
                    "apollo.router.response_cache.reconnection",
                    "Number of reconnections to the cache storage",
                    "{retry}",
                    1,
                    "subgraph.name" = subgraph_name.clone().unwrap_or_default()
                );
                if let Ok(storage) = Storage::new(&postgres_config).await {
                    if let Err(err) = storage.migrate().await {
                        tracing::error!(error = %err, "cannot migrate storage");
                    }
                    if let Err(err) = storage.update_cron().await {
                        tracing::error!(error = %err, "cannot update cron storage");
                    }
                    let _ = cache_storage.set(storage.clone());
                    tokio::task::spawn(metrics::expired_data_task(storage, abort_signal_cloned, None));
                    break;
                }
            }
        }
    }
}

pub(crate) type CacheKeysContext = Vec<CacheKeyContext>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheKeyContext {
    pub(super) key: String,
    pub(super) invalidation_keys: Vec<String>,
    pub(super) kind: CacheEntryKind,
    pub(super) subgraph_name: String,
    pub(super) subgraph_request: graphql::Request,
    pub(super) source: CacheKeySource,
    pub(super) cache_control: CacheControl,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) hashed_private_id: Option<String>,
    pub(super) data: serde_json_bytes::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq, Hash))]
#[serde(rename_all = "camelCase", untagged)]
pub(crate) enum CacheEntryKind {
    Entity {
        typename: String,
        #[serde(rename = "entityKey")]
        entity_key: Object,
    },
    RootFields {
        #[serde(rename = "rootFields")]
        root_fields: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq, Hash))]
#[serde(rename_all = "camelCase")]
pub(crate) enum CacheKeySource {
    /// Data fetched from subgraph
    Subgraph,
    /// Data fetched from cache
    Cache,
}

#[cfg(test)]
impl PartialOrd for CacheKeySource {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
impl Ord for CacheKeySource {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (CacheKeySource::Subgraph, CacheKeySource::Subgraph) => std::cmp::Ordering::Equal,
            (CacheKeySource::Subgraph, CacheKeySource::Cache) => std::cmp::Ordering::Greater,
            (CacheKeySource::Cache, CacheKeySource::Subgraph) => std::cmp::Ordering::Less,
            (CacheKeySource::Cache, CacheKeySource::Cache) => std::cmp::Ordering::Equal,
        }
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use apollo_compiler::Schema;

    use super::Subgraph;
    use super::Ttl;
    use crate::configuration::subgraph::SubgraphConfiguration;
    use crate::plugins::response_cache::plugin::ResponseCache;
    use crate::plugins::response_cache::storage::postgres::Config;
    use crate::plugins::response_cache::storage::postgres::Storage;

    const SCHEMA: &str = include_str!("../../testdata/orga_supergraph_cache_key.graphql");

    #[tokio::test]
    async fn test_subgraph_enabled() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let storage = Storage::new(&Config::test("test_subgraph_enabled"))
            .await
            .unwrap();
        let map = serde_json::json!({
            "user": {
                "private_id": "sub"
            },
            "orga": {
                "private_id": "sub",
                "enabled": true
            },
            "archive": {
                "private_id": "sub",
                "enabled": false
            }
        });

        let mut response_cache = ResponseCache::for_test(
            storage.clone(),
            serde_json::from_value(map).unwrap(),
            valid_schema.clone(),
            true,
            false,
        )
        .await
        .unwrap();

        assert!(response_cache.subgraph_enabled("user"));
        assert!(!response_cache.subgraph_enabled("archive"));
        let subgraph_config = serde_json::json!({
            "all": {
                "enabled": false
            },
            "subgraphs": response_cache.subgraphs.subgraphs.clone()
        });
        response_cache.subgraphs = Arc::new(serde_json::from_value(subgraph_config).unwrap());
        assert!(!response_cache.subgraph_enabled("archive"));
        assert!(response_cache.subgraph_enabled("user"));
        assert!(response_cache.subgraph_enabled("orga"));
    }

    #[tokio::test]
    async fn test_subgraph_ttl() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let storage = Storage::new(&Config::test("test_subgraph_ttl"))
            .await
            .unwrap();
        let map = serde_json::json!({
            "user": {
                "private_id": "sub",
                "ttl": "2s"
            },
            "orga": {
                "private_id": "sub",
                "enabled": true
            },
            "archive": {
                "private_id": "sub",
                "enabled": false,
                "ttl": "5000ms"
            }
        });

        let mut response_cache = ResponseCache::for_test(
            storage.clone(),
            serde_json::from_value(map).unwrap(),
            valid_schema.clone(),
            true,
            false,
        )
        .await
        .unwrap();

        assert_eq!(
            response_cache.subgraph_ttl("user"),
            Some(Duration::from_secs(2))
        );
        assert!(response_cache.subgraph_ttl("orga").is_none());
        assert_eq!(
            response_cache.subgraph_ttl("archive"),
            Some(Duration::from_millis(5000))
        );
        // Update ttl for all
        response_cache.subgraphs = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: Some(Ttl(Duration::from_secs(25))),
                ..Default::default()
            },
            subgraphs: response_cache.subgraphs.subgraphs.clone(),
        });
        assert_eq!(
            response_cache.subgraph_ttl("user"),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            response_cache.subgraph_ttl("orga"),
            Some(Duration::from_secs(25))
        );
        assert_eq!(
            response_cache.subgraph_ttl("archive"),
            Some(Duration::from_millis(5000))
        );
        response_cache.subgraphs = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: Some(Ttl(Duration::from_secs(42))),
                ..Default::default()
            },
            subgraphs: response_cache.subgraphs.subgraphs.clone(),
        });
        assert_eq!(
            response_cache.subgraph_ttl("user"),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            response_cache.subgraph_ttl("orga"),
            Some(Duration::from_secs(42))
        );
        assert_eq!(
            response_cache.subgraph_ttl("archive"),
            Some(Duration::from_millis(5000))
        );
    }
}
