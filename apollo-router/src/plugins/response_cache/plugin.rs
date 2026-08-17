use std::cell::RefCell;
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
use apollo_compiler::resolvers;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::StringTemplate;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use itertools::Itertools;
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
use super::cache_tag::CacheTag;
use super::invalidation::Invalidation;
use super::invalidation_endpoint::IndexMode;
use super::invalidation_endpoint::InvalidationEndpointConfig;
use super::invalidation_endpoint::InvalidationIndexes;
use super::invalidation_endpoint::InvalidationService;
use super::invalidation_endpoint::SubgraphInvalidationConfig;
use super::invalidation_endpoint::effective_invalidation_indexes;
use super::metrics::CacheMetricContextKey;
use super::metrics::record_fetch_error;
use crate::Context;
use crate::Endpoint;
use crate::ListenAddr;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::response_cache::cache_key::PrimaryCacheKeyEntity;
use crate::plugins::response_cache::cache_key::PrimaryCacheKeyRoot;
use crate::plugins::response_cache::cache_key::hash_additional_data;
use crate::plugins::response_cache::cache_key::hash_query;
use crate::plugins::response_cache::debugger::CacheEntryKind;
use crate::plugins::response_cache::debugger::CacheKeyContext;
use crate::plugins::response_cache::debugger::CacheKeySource;
use crate::plugins::response_cache::debugger::CdnInvalidationDebug;
use crate::plugins::response_cache::debugger::add_cache_key_to_context;
use crate::plugins::response_cache::debugger::add_cache_keys_to_context;
use crate::plugins::response_cache::debugger::add_cdn_invalidation_debug_to_context;
use crate::plugins::response_cache::invalidation_labels::InvalidationLabels;
use crate::plugins::response_cache::storage;
use crate::plugins::response_cache::storage::CacheEntry;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::Document;
use crate::plugins::response_cache::storage::redis::Storage;
use crate::plugins::telemetry::LruSizeInstrument;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::span_ext::SpanMarkError;
use crate::query_planner::OperationKind;
use crate::services::subgraph;
use crate::services::subgraph::SubgraphRequestId;
use crate::services::supergraph;
use crate::spec::QueryHash;
use crate::spec::TYPENAME;

/// Change this key if you introduce a breaking change in response caching algorithm to make sure it won't take the previous entries
pub(crate) const RESPONSE_CACHE_VERSION: &str = "1.2";
pub(crate) const CACHE_TAG_DIRECTIVE_NAME: &str = "federation__cacheTag";
pub(crate) const ENTITIES: &str = "_entities";
pub(crate) const REPRESENTATIONS: &str = "representations";
pub(crate) const CONTEXT_CACHE_KEY: &str = "apollo::response_cache::key";
/// Context key to enable support of debugger
pub(crate) const CONTEXT_DEBUG_CACHE_KEYS: &str = "apollo::response_cache::debug_cached_keys";
/// Context key for CDN invalidation header debug info. Only set (once per response) when
/// `cdn_invalidation.enabled` is true and `debug` is true; see `CdnInvalidationDebug`.
pub(crate) const CONTEXT_DEBUG_CDN_INVALIDATION: &str =
    "apollo::response_cache::debug_cdn_invalidation";
/// Context key stashing whether *this specific request* asked for cache debug info (config-level
/// `debug` AND the `CACHE_DEBUG_HEADER_NAME` header), set by a `map_request` step in
/// `supergraph_service` so its `map_response` step can read it. Unlike the per-entry
/// `CONTEXT_DEBUG_CACHE_KEYS` (which only ever contains data when `CacheService`, deeper in the
/// stack, already decided per-request whether to populate it), CDN header debug info is built
/// entirely within `supergraph_service`'s own `map_response`, which has no other way to see the
/// original request's headers — hence stashing this explicitly.
pub(crate) const CONTEXT_DEBUG_REQUESTED: &str = "apollo::response_cache::debug_requested";
pub(crate) const CACHE_DEBUG_HEADER_NAME: &str = "apollo-cache-debugging";
pub(crate) const CACHE_DEBUG_EXTENSIONS_KEY: &str = "apolloCacheDebugging";
pub(crate) const CACHE_DEBUGGER_VERSION: &str = "1.0";
pub(crate) const GRAPHQL_RESPONSE_EXTENSION_ROOT_FIELDS_CACHE_TAGS: &str = "apolloCacheTags";
pub(crate) const GRAPHQL_RESPONSE_EXTENSION_ENTITY_CACHE_TAGS: &str = "apolloEntityCacheTags";
const DEFAULT_LRU_PRIVATE_QUERIES_SIZE: NonZeroUsize = NonZeroUsize::new(2048).unwrap();
const LRU_PRIVATE_QUERIES_INSTRUMENT_NAME: &str =
    "apollo.router.response_cache.private_queries.lru.size";
/// Placeholder type name recorded for cache entries backing a whole root-field operation
/// (rather than an entity fetch), so type-tier invalidation labels always have a type to
/// interpolate. e.g. a cached `currentUser` root-field response renders as `type-user-Query`,
/// distinguishing it from an entity cache entry like `type-orga-Organization`.
const DEFAULT_ROOT_FIELD_TYPE_NAME: &str = "Query";

register_private_plugin!("apollo", "response_cache", ResponseCache);

#[derive(Clone)]
pub(crate) struct ResponseCache {
    pub(super) storage: Arc<StorageInterface>,
    endpoint_config: Option<Arc<InvalidationEndpointConfig>>,
    subgraphs: Arc<SubgraphConfiguration<Subgraph>>,
    entity_type: Option<String>,
    enabled: bool,
    debug: bool,
    include_cache_control_header_on_router_response: bool,
    cdn_invalidation: CdnInvalidationConfig,
    private_queries: Arc<RwLock<LruCache<PrivateQueryKey, ()>>>,
    pub(crate) invalidation: Invalidation,
    supergraph_schema: Arc<Valid<Schema>>,
    /// map containing the enum GRAPH
    subgraph_enums: Arc<HashMap<String, String>>,
    lru_size_instrument: LruSizeInstrument,
    /// Sender to tell spawned tasks to abort when this struct is dropped
    drop_tx: broadcast::Sender<()>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct PrivateQueryKey {
    query_hash: String,
    has_private_id: bool,
}

#[derive(Clone, Default)]
pub(crate) struct StorageInterface {
    all: Option<Arc<OnceLock<Storage>>>,
    subgraphs: HashMap<String, Arc<OnceLock<Storage>>>,
}

impl StorageInterface {
    pub(crate) fn get(&self, subgraph: &str) -> Option<&Storage> {
        let storage = self.subgraphs.get(subgraph).or(self.all.as_ref())?;
        storage.get()
    }

    /// Activate all storages so they can start emitting metrics.
    pub(crate) fn activate(&self) {
        if let Some(all) = &self.all
            && let Some(storage) = all.get()
        {
            storage.activate();
        }
        for storage in self.subgraphs.values() {
            if let Some(storage) = storage.get() {
                storage.activate();
            }
        }
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

    /// Enable debug mode for the debugger
    #[serde(default)]
    debug: bool,

    /// Whether to include a Cache-Control header in the supergraph response sent to clients.
    /// When set to false, the router will not set a Cache-Control header on the client response,
    /// while all internal caching behavior (TTL calculations, Redis storage, cache debugger) remains unchanged.
    /// Defaults to true for backward compatibility.
    #[serde(default = "default_include_cache_control_header_on_router_response")]
    include_cache_control_header_on_router_response: bool,

    /// Configure invalidation per subgraph
    pub(crate) subgraph: SubgraphConfiguration<Subgraph>,

    /// Global invalidation configuration
    invalidation: Option<InvalidationEndpointConfig>,

    /// Buffer size for known private queries (default: 2048)
    #[serde(default = "default_lru_private_queries_size")]
    private_queries_buffer_size: NonZeroUsize,

    /// Propagation of aggregated response_cache cache tags to the supergraph response.
    /// Off by default; opt in to surface cache tags to a CDN for tag-based purging. See
    /// `CdnInvalidationConfig` for the individual fields.
    #[serde(default)]
    pub(crate) cdn_invalidation: CdnInvalidationConfig,
}

/// Configuration for surfacing invalidation labels to a CDN as a header (for example, Cloudflare)
/// separated by a delimiting character (by default, ",")
///
/// When enabled, the router collects the union of cache tags contributed by each subgraph
/// (from `apolloCacheTags`, `apolloEntityCacheTags`, and resolved `@cacheTag` directive values)
/// and emits them as a single configurable header on the supergraph response. The coarser
/// `subgraph`/`type` labels are always recorded onto the request context (available to
/// coprocessors or Rhai regardless of this config), but the fine-grained tag values are only
/// aggregated when `enabled` is `true` — there's no way to read the full label set from context
/// without also enabling header emission.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct CdnInvalidationConfig {
    /// Whether to emit the configured header on the supergraph response. Defaults to false.
    pub(crate) enabled: bool,

    /// Name of the invalidation labels header used by CDNs
    pub(crate) header_name: String,

    /// The delimiter used by the invalidation labels header (eg, `" "`, `","`). Defaults to `","`.
    pub(crate) header_delimiter: String,

    /// Maximum number of bytes for the joined header value. When the joined value would exceed
    /// this size, `experimental_on_overflow` decides what to do. Defaults to 16kb.
    pub(crate) max_bytes: usize,

    /// What to do when the joined header value would exceed `max_bytes`.
    ///
    /// This is an experimental configuration option that might be removed in future releases,
    /// please use caution when deciding to use it. Defaults to `truncate`.
    pub(crate) experimental_on_overflow: OverflowBehavior,
}

/// What the router does when the joined `Cache-Tag`/invalidation-labels header value would
/// exceed `max_bytes`. See `CdnInvalidationConfig::experimental_on_overflow`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OverflowBehavior {
    /// Pack the header coarsest-first (subgraph, then type, then tag) and drop whatever doesn't
    /// fit, finest-grained first, so an oversized response still gets a usable, if partial,
    /// header.
    #[default]
    Truncate,
    /// Omit the header entirely rather than send a partial one. Note: this does not drop the
    /// `Cache-Control` header, so the response is still cached — this option is only useful
    /// paired with a CDN-side rule that treats a missing invalidation-labels header as a signal
    /// to bypass caching for that response (e.g. Cloudflare response rules setting `no-store`
    /// when the header is absent).
    Drop,
}

impl Default for CdnInvalidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            header_name: "Cache-Tag".to_string(),
            header_delimiter: ",".to_string(),
            max_bytes: 16384,
            experimental_on_overflow: OverflowBehavior::default(),
        }
    }
}

const fn default_lru_private_queries_size() -> NonZeroUsize {
    DEFAULT_LRU_PRIVATE_QUERIES_SIZE
}

const fn default_include_cache_control_header_on_router_response() -> bool {
    true
}

/// Per subgraph configuration for response caching
#[derive(Clone, Debug, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct Subgraph {
    /// Redis configuration
    pub(crate) redis: Option<storage::redis::Config>,

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
            redis: None,
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

        let mut storage_interface = StorageInterface::default();

        let (drop_tx, drop_rx) = tokio::sync::broadcast::channel(2);
        if init.config.enabled
            && init.config.subgraph.all.enabled.unwrap_or_default()
            && let Some(config) = init.config.subgraph.all.redis.clone()
        {
            let storage = Arc::new(OnceLock::new());
            storage_interface.all = Some(storage.clone());
            connect_or_spawn_reconnection_task(config, storage, drop_rx).await?;
        }

        for (subgraph, subgraph_config) in &init.config.subgraph.subgraphs {
            if Self::static_subgraph_enabled(init.config.enabled, &init.config.subgraph, subgraph) {
                match subgraph_config.redis.clone() {
                    Some(config) => {
                        // We need to do this because the subgraph config automatically clones from the `all` config during deserialization.
                        // We don't want to create a new connection pool if the subgraph just inherits from the `all` config (only if all is enabled).
                        if Some(&config) != init.config.subgraph.all.redis.as_ref()
                            || storage_interface.all.is_none()
                        {
                            let storage = Arc::new(OnceLock::new());
                            storage_interface
                                .subgraphs
                                .insert(subgraph.clone(), storage.clone());
                            connect_or_spawn_reconnection_task(
                                config,
                                storage,
                                drop_tx.subscribe(),
                            )
                            .await?;
                        }
                    }
                    None => {
                        if storage_interface.all.is_none() {
                            return Err(
                                format!("you must have a redis configured either for all subgraphs or for subgraph {subgraph:?}")
                                    .into(),
                            );
                        }
                    }
                }
            }
        }

        let storage_interface = Arc::new(storage_interface);
        let invalidation = Invalidation::new(storage_interface.clone()).await?;

        Ok(Self {
            storage: storage_interface,
            entity_type,
            enabled: init.config.enabled,
            debug: init.config.debug,
            include_cache_control_header_on_router_response: init
                .config
                .include_cache_control_header_on_router_response,
            cdn_invalidation: init.config.cdn_invalidation.clone(),
            endpoint_config: init.config.invalidation.clone().map(Arc::new),
            subgraphs: Arc::new(init.config.subgraph),
            private_queries: Arc::new(RwLock::new(LruCache::new(
                init.config.private_queries_buffer_size,
            ))),
            invalidation,
            subgraph_enums: Arc::new(get_subgraph_enums(&init.supergraph_schema)),
            supergraph_schema: init.supergraph_schema,
            lru_size_instrument: LruSizeInstrument::new(LRU_PRIVATE_QUERIES_INSTRUMENT_NAME),
            drop_tx,
        })
    }

    fn activate(&self) {
        self.storage.activate();
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxCloneService,
    ) -> supergraph::BoxCloneService {
        let debug = self.debug;
        let include_cache_control_header_on_router_response =
            self.include_cache_control_header_on_router_response;
        let cdn_invalidation_config = self.cdn_invalidation.clone();

        ServiceBuilder::new()
            .map_request(move |request: supergraph::Request| {
                // Stashed so `map_response` below can tell whether *this* request asked for
                // debug info — unlike the per-entry cache-key debug data (populated deeper in
                // the stack by `CacheService`, which does see per-request headers directly),
                // the CDN header debug info is built entirely in this supergraph-level
                // `map_response`, which otherwise has no access to the original request.
                let debug_requested = debug
                    && request
                        .supergraph_request
                        .headers()
                        .get(CACHE_DEBUG_HEADER_NAME)
                        == Some(&HeaderValue::from_static("true"));
                let _ = request
                    .context
                    .insert(CONTEXT_DEBUG_REQUESTED, debug_requested);
                request
            })
            .map_response(move |mut response: supergraph::Response| {
                if include_cache_control_header_on_router_response
                    && let Some(mut cache_control) = response
                        .context
                        .extensions()
                        .with_lock(|lock| lock.get::<CacheControl>().cloned())
                {
                    // If the response contains GraphQL errors, force Cache-Control: no-store to prevent
                    // intermediate caches (CDNs, reverse proxies) from caching partial or error responses.
                    let has_errors = response
                        .context
                        .get_json_value(CONTAINS_GRAPHQL_ERROR)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if has_errors {
                        cache_control = CacheControl::default_no_store();
                    }

                    let _ = cache_control.update_response_headers(response.response.headers_mut());
                }

                if cdn_invalidation_config.enabled {
                    let invalidation_labels = InvalidationLabels::get_or_create(&response.context);
                    let result = invalidation_labels.maybe_emit_header(
                        response.response.headers_mut(),
                        &cdn_invalidation_config,
                    );
                    let debug_requested = response
                        .context
                        .get_json_value(CONTEXT_DEBUG_REQUESTED)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if debug_requested {
                        let _ = add_cdn_invalidation_debug_to_context(
                            &response.context,
                            CdnInvalidationDebug::new(
                                cdn_invalidation_config.header_name.clone(),
                                cdn_invalidation_config.max_bytes,
                                &result,
                            ),
                        );
                    }
                }

                if debug {
                    let debug_data = response.context.get_json_value(CONTEXT_DEBUG_CACHE_KEYS);
                    let cdn_invalidation_debug: Option<CdnInvalidationDebug> = response
                        .context
                        .get(CONTEXT_DEBUG_CDN_INVALIDATION)
                        .ok()
                        .flatten();

                    if debug_data.is_some() || cdn_invalidation_debug.is_some() {
                        return response.map_stream(move |mut body| {
                            let mut payload = Object::new();
                            payload.insert(
                                ByteString::from("version".to_string()),
                                Value::from(CACHE_DEBUGGER_VERSION),
                            );
                            // Always present, even when empty: `data` not being an empty array
                            // would be a shape change for existing consumers of this extension,
                            // depending only on whether `CacheService` happened to populate any
                            // per-entry debug info for this particular response.
                            payload.insert(
                                ByteString::from("data".to_string()),
                                debug_data
                                    .clone()
                                    .unwrap_or_else(|| Value::Array(Vec::new())),
                            );
                            if let Some(cdn_debug) = cdn_invalidation_debug.clone() {
                                payload.insert(
                                    ByteString::from("cdnInvalidation".to_string()),
                                    serde_json_bytes::to_value(&cdn_debug).unwrap_or_default(),
                                );
                            }
                            body.extensions
                                .insert(CACHE_DEBUG_EXTENSIONS_KEY, Value::Object(payload));
                            body
                        });
                    }
                }

                response
            })
            .service(service)
            .boxed_clone()
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        let subgraph_ttl = self
            .subgraph_ttl(name)
            .unwrap_or_else(|| Duration::from_secs(60 * 60 * 24)); // The unwrap should not happen because it's checked when creating the plugin (except for tests)
        let subgraph_enabled = self.subgraph_enabled(name);
        let private_id = self.subgraphs.get(name).private_id.clone();
        // Use `effective_invalidation_indexes` so the write-path resolution matches the
        // `/invalidation` endpoint's resolution exactly: prefer per-subgraph config, fall back to
        // the `all` block, then to `InvalidationIndexes::default()`. A subgraph with partial
        // per-subgraph config (e.g., a custom TTL) but no `invalidation` block correctly inherits
        // from `all.invalidation.indexes` instead of skipping to the documented default.
        let indexes = Arc::new(effective_invalidation_indexes(&self.subgraphs, name));

        let name = name.to_string();

        if subgraph_enabled {
            let private_queries = self.private_queries.clone();
            let inner = ServiceBuilder::new()
                .map_response(move |response: subgraph::Response| {
                    let subgraph_cache_control =
                        CacheControl::try_from(response.response.headers())
                            .unwrap_or_else(|_| CacheControl::default_no_store())
                            .with_default_ttl(Some(subgraph_ttl));

                    update_cache_control(&response.context, &subgraph_cache_control);

                    response
                })
                .service(CacheService {
                    service,
                    entity_type: self.entity_type.clone(),
                    name: name.to_string(),
                    storage: self.storage.clone(),
                    subgraph_ttl,
                    private_queries,
                    private_id_key_name: private_id,
                    debug: self.debug,
                    supergraph_schema: self.supergraph_schema.clone(),
                    subgraph_enums: self.subgraph_enums.clone(),
                    lru_size_instrument: self.lru_size_instrument.clone(),
                    indexes,
                    cdn_invalidation_enabled: self.cdn_invalidation.enabled,
                });
            tower::util::BoxCloneService::new(inner)
        } else {
            ServiceBuilder::new()
                .map_response(move |response: subgraph::Response| {
                    let subgraph_cache_control =
                        CacheControl::try_from(response.response.headers())
                            .unwrap_or_else(|_| CacheControl::default_no_store())
                            .with_default_ttl(Some(subgraph_ttl));

                    update_cache_control(&response.context, &subgraph_cache_control);

                    response
                })
                .service(service)
                .boxed_clone()
        }
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut map = MultiMap::new();
        // At least 1 subgraph enabled caching
        let any_caching_enabled = self
            .subgraphs
            .subgraphs
            .iter()
            .any(|(subgraph_name, _cfg)| self.subgraph_enabled(subgraph_name))
            || self.subgraphs.all.enabled.unwrap_or_default();

        let global_invalidation_enabled = self
            .subgraphs
            .all
            .invalidation
            .as_ref()
            .map(|i| i.enabled)
            .unwrap_or_default();

        // If at least one subgraph is enabled and has invalidation enabled
        let any_subgraph_invalidation_enabled =
            self.subgraphs.subgraphs.iter().any(|(subgraph_name, cfg)| {
                self.subgraph_enabled(subgraph_name)
                    && cfg
                        .invalidation
                        .as_ref()
                        .map(|i| i.enabled)
                        .unwrap_or_default()
            });

        if self.enabled
            && any_caching_enabled
            && (global_invalidation_enabled || any_subgraph_invalidation_enabled)
        {
            match &self.endpoint_config {
                Some(endpoint_config) => {
                    let endpoint = Endpoint::from_router_service(
                        endpoint_config.path.clone(),
                        InvalidationService::new(self.subgraphs.clone(), self.invalidation.clone())
                            .boxed_clone(),
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
        subgraphs: SubgraphConfiguration<Subgraph>,
        supergraph_schema: Arc<Valid<Schema>>,
        truncate_namespace: bool,
        drop_tx: broadcast::Sender<()>,
        include_cache_control_header_on_router_response: bool,
    ) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Self::build_for_test(
            storage,
            subgraphs,
            supergraph_schema,
            truncate_namespace,
            drop_tx,
            include_cache_control_header_on_router_response,
            CdnInvalidationConfig::default(),
        )
        .await
    }

    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    /// Like `for_test`, but lets the caller configure `cdn_invalidation` instead of always
    /// disabling it — needed by any test that exercises the CDN `Cache-Tag` header path.
    pub(crate) async fn for_test_with_cdn_invalidation(
        storage: Storage,
        subgraphs: SubgraphConfiguration<Subgraph>,
        supergraph_schema: Arc<Valid<Schema>>,
        truncate_namespace: bool,
        drop_tx: broadcast::Sender<()>,
        include_cache_control_header_on_router_response: bool,
        cdn_invalidation: CdnInvalidationConfig,
    ) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Self::build_for_test(
            storage,
            subgraphs,
            supergraph_schema,
            truncate_namespace,
            drop_tx,
            include_cache_control_header_on_router_response,
            cdn_invalidation,
        )
        .await
    }

    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    async fn build_for_test(
        storage: Storage,
        subgraphs: SubgraphConfiguration<Subgraph>,
        supergraph_schema: Arc<Valid<Schema>>,
        truncate_namespace: bool,
        drop_tx: broadcast::Sender<()>,
        include_cache_control_header_on_router_response: bool,
        cdn_invalidation: CdnInvalidationConfig,
    ) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        use std::net::IpAddr;
        use std::net::Ipv4Addr;
        use std::net::SocketAddr;
        if truncate_namespace {
            storage.truncate_namespace().await?;
        }

        let storage = Arc::new(StorageInterface {
            all: Some(Arc::new(storage.into())),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone()).await?;
        Ok(Self {
            storage,
            entity_type: None,
            enabled: true,
            debug: true,
            include_cache_control_header_on_router_response,
            cdn_invalidation,
            subgraphs: Arc::new(subgraphs),
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
            lru_size_instrument: LruSizeInstrument::new(LRU_PRIVATE_QUERIES_INSTRUMENT_NAME),
            drop_tx,
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
            include_cache_control_header_on_router_response: true,
            cdn_invalidation: CdnInvalidationConfig::default(),
            subgraphs: Arc::new(SubgraphConfiguration {
                all: Subgraph {
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: INVALIDATION_SHARED_KEY.to_string(),
                        ..Default::default()
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
            lru_size_instrument: LruSizeInstrument::new(LRU_PRIVATE_QUERIES_INSTRUMENT_NAME),
            drop_tx,
        })
    }

    /// Returns boolean to know if cache is enabled for this subgraph
    fn subgraph_enabled(&self, subgraph_name: &str) -> bool {
        Self::static_subgraph_enabled(self.enabled, &self.subgraphs, subgraph_name)
    }

    /// Static method which returns boolean to know if cache is enabled for this subgraph
    fn static_subgraph_enabled(
        plugin_enabled: bool,
        subgraph_config: &SubgraphConfiguration<Subgraph>,
        subgraph_name: &str,
    ) -> bool {
        if !plugin_enabled {
            return false;
        }
        match (
            subgraph_config.all.enabled,
            subgraph_config.get(subgraph_name).enabled,
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

impl Drop for ResponseCache {
    fn drop(&mut self) {
        let _ = self.drop_tx.send(());
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

/// A `tower::Service` wrapping a single subgraph's service to add response caching: on a
/// request, checks the cache before calling through to `service`; on a cache miss, stores the
/// subgraph's response (subject to its `Cache-Control` and this subgraph's TTL/indexing
/// configuration) for the next request to hit.
#[derive(Clone)]
struct CacheService {
    service: subgraph::BoxCloneService,
    name: String,
    entity_type: Option<String>,
    storage: Arc<StorageInterface>,
    subgraph_ttl: Duration,
    private_queries: Arc<RwLock<LruCache<PrivateQueryKey, ()>>>,
    private_id_key_name: Option<String>,
    debug: bool,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: Arc<HashMap<String, String>>,
    lru_size_instrument: LruSizeInstrument,
    /// Active invalidation index modes for this subgraph, resolved from configuration.
    /// Gates which Redis ZSET indexes are maintained on cache inserts.
    indexes: Arc<InvalidationIndexes>,
    /// Whether `cdn_invalidation` is enabled plugin-wide. Gates whether cache-tag-driven
    /// `InvalidationLabels` aggregation (the CDN `Cache-Tag` header's content) happens at all —
    /// independent of `indexes`, which gates the Redis-side ZSET writes instead.
    cdn_invalidation_enabled: bool,
}

impl Service<subgraph::Request> for CacheService {
    type Response = subgraph::Response;
    type Error = BoxError;
    type Future = <subgraph::BoxCloneService as Service<subgraph::Request>>::Future;

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
                        let subgraph_cache_control =
                            CacheControl::try_from(response.response.headers())
                                .unwrap_or_else(|_| CacheControl::default_no_store());

                        update_cache_control(&response.context, &subgraph_cache_control);

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
        if request.is_part_of_batch() {
            return self.service.call(request).await;
        }

        // [RFC 9111](https://datatracker.ietf.org/doc/html/rfc9111):
        //  * no-store: allows serving response from cache, but prohibits storing response in cache
        //  * no-cache: prohibits serving response from cache, but allows storing response in cache
        //
        // NB: no-cache actually prohibits serving response from cache _without revalidation_, but
        //  in the router this is the same thing

        let cache_control = if request
            .subgraph_request
            .headers()
            .contains_key(&CACHE_CONTROL)
        {
            let cache_control = match CacheControl::try_from(request.subgraph_request.headers()) {
                Ok(cache_control) => cache_control,
                Err(err) => {
                    return Ok(subgraph::Response::builder()
                        .subgraph_name(request.subgraph_name)
                        .id(request.id)
                        .context(request.context)
                        .error(
                            graphql::Error::request_error_builder()
                                .message(format!("cannot get cache-control header: {err}"))
                                .extension_code("INVALID_CACHE_CONTROL_HEADER")
                                .build(),
                        )
                        .extensions(Object::default())
                        .build());
                }
            };

            // Don't use cache at all if both no-store and no-cache are set in cache-control header
            if cache_control.no_cache() && cache_control.no_store() {
                let mut resp = self.service.call(request).await?;
                cache_control.update_response_headers(resp.response.headers_mut())?;
                return Ok(resp);
            }

            Some(cache_control)
        } else {
            None
        };

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
        let is_entity = request
            .subgraph_request
            .body()
            .variables
            .contains_key(REPRESENTATIONS);

        // the response will have a private scope but we don't have a way to differentiate users, so
        // we know we will not get or store anything in the cache
        if is_known_private && private_id.is_none() {
            self.call_service_for_private_query_without_id(request, is_entity)
                .await
        } else if is_entity {
            self.call_service_for_entities_query(
                request,
                storage,
                is_known_private,
                private_id,
                private_query_key,
                cache_control,
            )
            .await
        } else {
            self.call_service_for_root_fields_operation(
                request,
                storage,
                is_known_private,
                private_id,
                private_query_key,
                cache_control,
            )
            .await
        }
    }

    async fn call_service_for_private_query_without_id(
        mut self,
        request: subgraph::Request,
        is_entity: bool,
    ) -> Result<subgraph::Response, BoxError> {
        let mut debug_subgraph_request = None;
        let mut root_operation_fields = Vec::new();
        if self.debug {
            root_operation_fields = request.root_operation_fields();
            debug_subgraph_request = Some(request.subgraph_request.body().clone());
        }
        let resp = self.service.call(request).await?;
        if self.debug {
            let cache_control = CacheControl::try_from(resp.response.headers())?
                .with_default_ttl(Some(self.subgraph_ttl));
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

            let cache_key_context = CacheKeyContext {
                key: "-".to_string(),
                invalidation_keys: vec![],
                has_tags: false,
                cdn_invalidation_enabled: self.cdn_invalidation_enabled,
                kind,
                hashed_private_id: None,
                subgraph_name: self.name.clone(),
                subgraph_request: debug_subgraph_request.unwrap_or_default(),
                source: CacheKeySource::Subgraph,
                cache_control,
                data: serde_json_bytes::to_value(resp.response.body().clone()).unwrap_or_default(),
                warnings: Vec::new(),
                should_store: false,
                indexes: *self.indexes,
            }
            .update_metadata();
            add_cache_key_to_context(&resp.context, cache_key_context)?;
        }

        Ok(resp)
    }

    /// Handles a whole root-field-operation subgraph request (as opposed to an entity fetch):
    /// looks up the cache, calls through to the subgraph on a miss, and stores the response
    /// (including its invalidation labels) for next time.
    async fn call_service_for_root_fields_operation(
        mut self,
        request: subgraph::Request,
        storage: Storage,
        is_known_private: bool,
        private_id: Option<String>,
        private_query_key: PrivateQueryKey,
        request_cache_control: Option<CacheControl>,
    ) -> Result<subgraph::Response, BoxError> {
        // Skip cache entirely if this is a root fields operation that isn't a query
        if request.operation_kind != OperationKind::Query {
            return self.service.call(request).await;
        }

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
            request_cache_control.as_ref(),
            &self.indexes,
            self.cdn_invalidation_enabled,
        )
        .instrument(tracing::info_span!(
            "response_cache.lookup",
            kind = "root",
            subgraph.name = self.name.clone(),
            "graphql.type" = self.entity_type.as_deref().unwrap_or_default(),
            debug = self.debug,
            private = is_known_private,
            contains_private_id = private_id.is_some(),
            "cache.key" = ::tracing::field::Empty,
        ))
        .await?
        {
            ControlFlow::Break(response) => {
                cache_hit.insert(
                    DEFAULT_ROOT_FIELD_TYPE_NAME.to_string(),
                    CacheHitMiss { hit: 1, miss: 0 },
                );
                let _ = response.context.insert(
                    CacheMetricContextKey::new(response.subgraph_name.clone()),
                    CacheSubgraph(cache_hit),
                );

                Ok(response)
            }
            ControlFlow::Continue((
                request,
                mut root_cache_key,
                mut cache_tags,
                mut cdn_invalidation_tags,
            )) => {
                cache_hit.insert(
                    DEFAULT_ROOT_FIELD_TYPE_NAME.to_string(),
                    CacheHitMiss { hit: 0, miss: 1 },
                );
                let _ = request.context.insert(
                    CacheMetricContextKey::new(request.subgraph_name.clone()),
                    CacheSubgraph(cache_hit),
                );

                // stash a few pieces of the request to use for debugging later
                let mut root_operation_fields: Vec<String> = Vec::new();
                let mut debug_subgraph_request = None;
                if self.debug {
                    root_operation_fields = request.root_operation_fields();
                    debug_subgraph_request = Some(request.subgraph_request.body().clone());
                }

                let response = self.service.call(request).await?;

                let mut cache_control =
                    response.subgraph_cache_control(self.subgraph_ttl.into())?;

                // Support cache tags coming from subgraph response extensions.
                if self
                    .indexes
                    .tracks_invalidation_labels(self.cdn_invalidation_enabled)
                    && let Some(Value::Array(extension_tags)) = response
                        .get_from_extensions(GRAPHQL_RESPONSE_EXTENSION_ROOT_FIELDS_CACHE_TAGS)
                {
                    let extension_tag_strings: Vec<String> = extension_tags
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_owned())
                        .collect();

                    // Fine-grained tags only ever aggregate onto the live `InvalidationLabels`
                    // context when `cdn_invalidation_enabled` — matching `CdnInvalidationConfig`'s
                    // documented contract — even though the outer `if` above also lets the
                    // Redis `cache_tag` index alone reach this block.
                    if self.cdn_invalidation_enabled {
                        InvalidationLabels::add_tags(
                            &response.context,
                            extension_tag_strings.clone(),
                        );
                    }
                    InvalidationLabels::add_subgraph(&response.context, &self.name);
                    InvalidationLabels::add_type(
                        &response.context,
                        &self.name,
                        self.entity_type
                            .clone()
                            .unwrap_or(DEFAULT_ROOT_FIELD_TYPE_NAME.to_string())
                            .as_str(),
                    );

                    // Recorded whenever either consumer got us into this block (the outer `if`
                    // above), not just when `cdn_invalidation_enabled` — this also feeds the
                    // cache debugger's per-entry tag display, which should show these tags
                    // whenever the Redis `cache_tag` index is on, independent of CDN
                    // invalidation. Persisted independently of `cache_tags` below (and thus of
                    // `IndexMode::CacheTag`) so a later cache hit can rebuild the CDN header
                    // even when Redis per-tag indexing is off. See
                    // `CacheMetadata::cdn_invalidation_tags`.
                    cdn_invalidation_tags.extend(extension_tag_strings.iter().cloned());

                    // `cache_tags` feeds the intermediate result on cache misses which gets ZADDed for redis;
                    // it's not part of the CDN-invalidation path; each sink below applies its own gate
                    // independently rather than sharing one.
                    if self.indexes.is_enabled(IndexMode::CacheTag) {
                        cache_tags.extend(extension_tag_strings.into_iter().map(CacheTag::Tag));
                    }
                }
                save_original_cache_control(
                    response.id.clone(),
                    &response.context,
                    cache_control.clone(),
                );

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
                }

                // if the request had no_store on it, propagate that to this cache control
                if let Some(request_cache_control) = request_cache_control {
                    cache_control.merge_no_store(&request_cache_control);
                }

                if self.debug {
                    let cache_key_context = CacheKeyContext {
                        key: root_cache_key.clone(),
                        hashed_private_id: private_id.clone(),
                        invalidation_keys: InvalidationLabels {
                            tags: cdn_invalidation_tags.iter().cloned().collect(),
                            types: HashSet::from([(
                                self.name.clone(),
                                self.entity_type
                                    .clone()
                                    .unwrap_or(DEFAULT_ROOT_FIELD_TYPE_NAME.to_string()),
                            )]),
                            subgraphs: HashSet::from([self.name.clone()]),
                        }
                        .user_facing_only(),
                        has_tags: !cdn_invalidation_tags.is_empty(),
                        cdn_invalidation_enabled: self.cdn_invalidation_enabled,
                        kind: CacheEntryKind::RootFields {
                            root_fields: root_operation_fields,
                        },
                        subgraph_name: self.name.clone(),
                        subgraph_request: debug_subgraph_request.unwrap_or_default(),
                        source: CacheKeySource::Subgraph,
                        cache_control: cache_control.clone(),
                        data: serde_json_bytes::to_value(response.response.body().clone())
                            .unwrap_or_default(),
                        warnings: Vec::new(),
                        should_store: true,
                        indexes: *self.indexes,
                    }
                    .update_metadata();
                    add_cache_key_to_context(&response.context, cache_key_context)?;
                }

                // the response has a private scope but we don't have a way to differentiate
                // users, so we do not store the response in cache
                let unstorable_private_response = cache_control.private() && private_id.is_none();

                if !unstorable_private_response && cache_control.should_store() {
                    // Prepend the whole-subgraph index entry when that index is active. The
                    // by-type and per-tag entries were already appended in scope by the
                    // cache-lookup and extension-read paths according to the same indexes.
                    if self.indexes.is_enabled(IndexMode::Subgraph) {
                        cache_tags.insert(0, CacheTag::Subgraph);
                    }
                    cache_store_root_from_response(
                        storage,
                        self.subgraph_ttl,
                        &response,
                        cache_control,
                        root_cache_key,
                        cache_tags,
                        cdn_invalidation_tags,
                    )
                    .await?;
                }

                Ok(response)
            }
        }
    }

    async fn call_service_for_entities_query(
        mut self,
        request: subgraph::Request,
        storage: Storage,
        is_known_private: bool,
        private_id: Option<String>,
        private_query_key: PrivateQueryKey,
        request_cache_control: Option<CacheControl>,
    ) -> Result<subgraph::Response, BoxError> {
        match cache_lookup_entities(
            self.name.clone(),
            self.supergraph_schema.clone(),
            &self.subgraph_enums,
            storage.clone(),
            is_known_private,
            private_id.as_deref(),
            request,
            self.debug,
            request_cache_control.as_ref(),
            &self.indexes,
            self.cdn_invalidation_enabled,
        )
        .instrument(tracing::info_span!(
            "response_cache.lookup",
            kind = "entity",
            subgraph.name = self.name.clone(),
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
                            invalidation_keys: InvalidationLabels {
                                tags: ir.cdn_invalidation_tags.iter().cloned().collect(),
                                types: HashSet::from([(self.name.clone(), ir.typename.clone())]),
                                subgraphs: HashSet::from([self.name.clone()]),
                            }
                            .user_facing_only(),
                            has_tags: !ir.cdn_invalidation_tags.is_empty(),
                            cdn_invalidation_enabled: self.cdn_invalidation_enabled,
                            kind: CacheEntryKind::Entity {
                                typename: ir.typename.clone(),
                                entity_key: ir.entity_key.clone().unwrap_or_default(),
                            },
                            subgraph_name: self.name.clone(),
                            subgraph_request: request.subgraph_request.body().clone(),
                            source: CacheKeySource::Cache,
                            cache_control: cache_entry.control.clone(),
                            data: serde_json_bytes::json!({
                                    "data": serde_json_bytes::to_value(cache_entry.data.clone()).unwrap_or_default()
                                }),
                            warnings: Vec::new(),
                            should_store: false,
                            indexes: *self.indexes,
                        }.update_metadata())
                    });
                    add_cache_keys_to_context(&request.context, debug_cache_keys_ctx)?;
                }
                let req_id = request.id.clone();
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

                        let (new_entities, new_errors) =
                            assemble_response_from_errors(&[graphql_error], &mut cache_result.0);

                        let mut data = Object::default();
                        data.insert(ENTITIES, new_entities.into());

                        let mut response = subgraph::Response::builder()
                            .context(context)
                            .data(Value::Object(data))
                            .id(req_id)
                            .errors(new_errors)
                            .subgraph_name(self.name)
                            .extensions(Object::new())
                            .build();
                        CacheControl::default_no_store()
                            .update_response_headers(response.response.headers_mut())?;

                        return Ok(response);
                    }
                };

                let mut cache_control =
                    response.subgraph_cache_control(self.subgraph_ttl.into())?;

                save_original_cache_control(
                    response.id.clone(),
                    &response.context,
                    cache_control.clone(),
                );

                if let Some(control_from_cached) = cache_result.1 {
                    cache_control = cache_control.merge(&control_from_cached);
                }

                // if the request had no_store on it, propagate that to this cache control
                if let Some(request_cache_control) = request_cache_control {
                    cache_control.merge_no_store(&request_cache_control);
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
                    self.indexes.clone(),
                    self.cdn_invalidation_enabled,
                )
                .await?;

                cache_control.update_response_headers(response.response.headers_mut())?;

                Ok(response)
            }
        }
    }

    fn get_private_id(&self, context: &Context) -> Option<String> {
        let private_id_value = context.get_json_value(self.private_id_key_name.as_ref()?)?;
        let private_id = private_id_value.as_str()?;

        let mut digest = blake3::Hasher::new();
        digest.update(private_id.as_bytes());
        Some(digest.finalize().to_hex().to_string())
    }
}

/// Looks up the cache for a whole root-field-operation subgraph request. Returns
/// `ControlFlow::Break` with the cached response on a hit (after merging its stored
/// invalidation labels into this request's `InvalidationLabels` aggregator), or
/// `ControlFlow::Continue` with the computed cache key and this-response's cache/CDN tags on a
/// miss, for the caller to fetch from the subgraph and then store.
///
/// `name` is the subgraph name, not an operation or field name.
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
    cache_control: Option<&CacheControl>,
    indexes: &InvalidationIndexes,
    cdn_invalidation_enabled: bool,
) -> Result<
    ControlFlow<subgraph::Response, (subgraph::Request, String, Vec<CacheTag>, Vec<String>)>,
    BoxError,
> {
    // Skip the schema traversal entirely when nothing would consume its result. The traversal
    // walks `@cacheTag` directives to produce per-tag invalidation keys; two independent
    // consumers can want them (the Redis per-tag index; the CDN `Cache-Tag` header aggregator)
    // — only skip the work when neither does.
    let invalidation_cache_keys = if indexes.tracks_invalidation_labels(cdn_invalidation_enabled) {
        get_invalidation_root_keys_from_schema(&request, subgraph_enums, supergraph_schema)?
    } else {
        HashSet::new()
    };
    let body = request.subgraph_request.body_mut();
    body.variables.sort_keys();

    let (key, mut cache_tags) = extract_cache_key_root(
        &name,
        entity_type_opt,
        &request.query_hash,
        body,
        &request.context,
        &request.authorization,
        is_known_private,
        private_id,
        indexes,
    );
    // Redis indexing and the CDN header both draw on `invalidation_cache_keys`, but each applies
    // its own gate independently rather than sharing one — see the comment above where it's
    // computed.
    if indexes.is_enabled(IndexMode::CacheTag) {
        cache_tags.extend(invalidation_cache_keys.iter().cloned().map(CacheTag::Tag));
    }

    // Persisted independently of `cache_tags` above (and thus of `IndexMode::CacheTag`) so a
    // later cache hit can rebuild the CDN header even when Redis per-tag indexing is off.
    // `invalidation_cache_keys` is already empty when neither consumer wants it.
    let cdn_invalidation_tags: Vec<String> = invalidation_cache_keys.iter().cloned().collect();

    if cdn_invalidation_enabled {
        InvalidationLabels::add_tags(
            &request.context,
            invalidation_cache_keys.into_iter().collect(),
        );
    }
    InvalidationLabels::add_subgraph(&request.context, &name);
    InvalidationLabels::add_type(
        &request.context,
        &name,
        entity_type_opt.unwrap_or(DEFAULT_ROOT_FIELD_TYPE_NAME),
    );

    Span::current().record("cache.key", key.clone());

    if cache_control.is_some_and(|c| c.no_cache()) {
        // skip cache lookup if no-cache is set - we have no means of revalidating entries without
        // just performing the query, so there's no benefit to hitting the cache
        return Ok(ControlFlow::Continue((
            request,
            key,
            cache_tags,
            cdn_invalidation_tags,
        )));
    }

    match cache.fetch(&key, &request.subgraph_name).await {
        Ok(value) => {
            if value.control.can_use() {
                let control = value.control.clone();
                // Keep original cache control for every subgraph request (useful for telemetry)
                save_original_cache_control(request.id.clone(), &request.context, control.clone());
                update_cache_control(&request.context, &control);
                if debug {
                    let debug_value = value.clone();
                    // TODO: this uses iter() rather than get(request.subgraph_operation_name()) - why?
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

                    // `debug_value.invalidation_labels` (reconstructed from storage) only ever
                    // carries the `tags` tier — `types`/`subgraphs` are never persisted, only
                    // computed live per-request — so union in this entry's own subgraph/type
                    // label here to show the full set of strings a customer could purge by.
                    let stored_tags = debug_value
                        .invalidation_labels
                        .map(|labels| labels.tags)
                        .unwrap_or_default();
                    let has_tags = !stored_tags.is_empty();
                    let invalidation_keys = InvalidationLabels {
                        tags: stored_tags,
                        types: HashSet::from([(
                            name.clone(),
                            entity_type_opt
                                .unwrap_or(DEFAULT_ROOT_FIELD_TYPE_NAME)
                                .to_string(),
                        )]),
                        subgraphs: HashSet::from([name.clone()]),
                    }
                    .user_facing_only();

                    let cache_key_context = CacheKeyContext {
                        key: debug_value.key,
                        has_tags,
                        cdn_invalidation_enabled,
                        hashed_private_id: private_id.map(ToString::to_string),
                        invalidation_keys,
                        kind: CacheEntryKind::RootFields {
                            root_fields: root_operation_fields,
                        },
                        subgraph_name: request.subgraph_name.clone(),
                        subgraph_request: request.subgraph_request.body().clone(),
                        source: CacheKeySource::Cache,
                        cache_control: debug_value.control.clone(),
                        data: serde_json_bytes::json!({"data": debug_value.data.clone()}),
                        warnings: Vec::new(),
                        should_store: false,
                        indexes: *indexes,
                    }
                    .update_metadata();

                    add_cache_key_to_context(&request.context, cache_key_context)?;
                }

                Span::current().set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("hit".into()),
                );

                // Surface the cache tags persisted with the entry so the supergraph response
                // aggregator can reflect this hit. Done before the response builder consumes
                // the request context.
                if let Some(hit_labels) = value.invalidation_labels {
                    // backfill the subgraph and type--the CacheEntry doesn't store those, just the
                    // previous invalidation labels
                    InvalidationLabels::merge(&request.context, hit_labels);
                    InvalidationLabels::add_subgraph(&request.context, &name);
                    InvalidationLabels::add_type(
                        &request.context,
                        &name,
                        entity_type_opt.unwrap_or(DEFAULT_ROOT_FIELD_TYPE_NAME),
                    );

                    //record_external_cache_tags_to_context(
                    //    &request.context,
                    //    invalidation_labels,
                    //    &name,
                    //    entity_type_opt.unwrap_or(DEFAULT_ROOT_FIELD_TYPE_NAME),
                    //);
                }

                let mut response = subgraph::Response::builder()
                    .data(value.data)
                    .extensions(Object::new())
                    .id(request.id)
                    .context(request.context)
                    .subgraph_name(request.subgraph_name.clone())
                    .build();

                value
                    .control
                    .update_response_headers(response.response.headers_mut())?;
                Ok(ControlFlow::Break(response))
            } else {
                Span::current().set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("miss".into()),
                );
                Ok(ControlFlow::Continue((
                    request,
                    key,
                    cache_tags,
                    cdn_invalidation_tags,
                )))
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
            Ok(ControlFlow::Continue((
                request,
                key,
                cache_tags,
                cdn_invalidation_tags,
            )))
        }
    }
}

fn get_invalidation_root_keys_from_schema(
    request: &subgraph::Request,
    subgraph_enums: &HashMap<String, String>,
    supergraph_schema: Arc<Valid<Schema>>,
) -> Result<HashSet<String>, anyhow::Error> {
    struct Root<'a> {
        subgraph_name: &'a str,
        subgraph_enums: &'a HashMap<String, String>,
        query_object_type: &'a ObjectType,
        result: RefCell<Result<HashSet<String>, anyhow::Error>>,
    }

    impl resolvers::ObjectValue for Root<'_> {
        fn type_name(&self) -> &str {
            DEFAULT_ROOT_FIELD_TYPE_NAME
        }

        fn resolve_field<'a>(
            &'a self,
            info: &'a resolvers::ResolveInfo<'a>,
        ) -> Result<resolvers::ResolvedValue<'a>, resolvers::FieldError> {
            let mut result = self.result.borrow_mut();
            let Ok(keys) = &mut *result else {
                return Ok(resolvers::ResolvedValue::SkipForPartialExecution);
            };
            // We don't use info.field_definition() because we need the directive
            // set in supergraph schema not in the executable document
            let Some(field_def) = self.query_object_type.fields.get(info.field_name()) else {
                *result = Err(FetchError::MalformedRequest {
                    reason: "cannot get the field definition from supergraph schema".to_string(),
                }
                .into());
                return Ok(resolvers::ResolvedValue::SkipForPartialExecution);
            };
            let templates = field_def
                .directives
                .get_all("join__directive")
                .filter_map(|dir| {
                    let name = dir.argument_by_name("name", info.schema()).ok()?;
                    if name.as_str()? != CACHE_TAG_DIRECTIVE_NAME {
                        return None;
                    }
                    let is_current_subgraph =
                        dir.argument_by_name("graphs", info.schema())
                            .ok()
                            .and_then(|f| {
                                Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
                                    |g| {
                                        self.subgraph_enums.get(g.as_str()).map(|s| s.as_str())
                                            == Some(self.subgraph_name)
                                    },
                                ))
                            })
                            .unwrap_or_default();
                    if !is_current_subgraph {
                        return None;
                    }
                    let mut format = None;
                    for (field_name, value) in dir
                        .argument_by_name("args", info.schema())
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

            let mut vars = IndexMap::default();
            vars.insert("$args".to_string(), Value::Object(info.arguments().clone()));

            for template in templates {
                match template.interpolate(&vars) {
                    Ok((key, _)) => {
                        keys.insert(key);
                    }
                    Err(e) => {
                        *result = Err(e.into());
                        break;
                    }
                }
            }
            Ok(resolvers::ResolvedValue::SkipForPartialExecution)
        }
    }

    let executable_document =
        request
            .executable_document
            .as_ref()
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "cannot get the executable document for subgraph request".to_string(),
            })?;
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
    let root = Root {
        subgraph_name: &request.subgraph_name,
        subgraph_enums,
        query_object_type,
        result: RefCell::new(Ok(HashSet::new())),
    };
    let subgraph_request = request.subgraph_request.body();
    // FIXME: in principle we should use the subgraph schema here.
    // Maybe this is good enough as far as finding root fields is concerned?
    resolvers::Execution::new(&supergraph_schema, executable_document)
        .operation_name(subgraph_request.operation_name.as_deref())
        .unwrap()
        .raw_variable_values(&subgraph_request.variables)
        .execute_sync(&root)
        .map_err(|e| anyhow::Error::msg(e.message().to_string()))?;

    root.result.into_inner()
}

#[derive(Default)]
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
    cache_control: Option<&CacheControl>,
    indexes: &InvalidationIndexes,
    cdn_invalidation_enabled: bool,
) -> Result<ControlFlow<subgraph::Response, (subgraph::Request, ResponseCacheResults)>, BoxError> {
    let is_no_cache = cache_control.is_some_and(|c| c.no_cache());

    let cache_metadata = extract_cache_keys(
        &name,
        supergraph_schema,
        subgraph_enums,
        &mut request,
        is_known_private,
        private_id,
        debug,
        indexes,
        cdn_invalidation_enabled,
    )?;
    let keys_len = cache_metadata.len();

    let cache_keys = cache_metadata
        .iter()
        .map(|k| k.cache_key.as_str())
        .collect::<Vec<&str>>();

    Span::current().set_span_dyn_attribute(
        "cache.keys".into(),
        opentelemetry::Value::Array(Array::String(
            cache_keys
                .iter()
                .map(|ck| StringValue::from(ck.to_string()))
                .collect(),
        )),
    );

    // When no-cache is set, skip using any cached values: treat every entity as a cache miss
    // so that all representations are fetched fresh from the subgraph. We still build the
    // IntermediateResult list (all with cache_entry = None) so that insert_entities_in_result
    // can properly assemble the response in the correct order.
    let cache_result: Vec<Option<CacheEntry>> = if is_no_cache {
        vec![None; keys_len]
    } else {
        match cache.fetch_multiple(&cache_keys, &name).await {
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

                vec![None; keys_len]
            }
        }
    };

    let body = request.subgraph_request.body_mut();

    let representations = body
        .variables
        .get_mut(REPRESENTATIONS)
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");
    // When no-cache is set, skip recording cache metrics: the cache was not consulted so
    // registering every entity as a miss would produce misleading telemetry data.

    // remove from representations the entities we already obtained from the cache
    let (new_representations, cache_result, cache_control) = filter_representations(
        &name,
        &request.id,
        representations,
        cache_metadata,
        cache_result,
        &request.context,
        !is_no_cache,
    )?;

    // Surface cache tags from every usable hit on the supergraph response aggregator. This
    // covers both the full-hit short-circuit below and the partial-hit path where the error
    // branch may rebuild a response from only cached entities.
    //
    // `entry.cdn_invalidation_tags` only reflects what's known before fetching (the
    // schema-derived tag values, plus any `apolloEntityCacheTags` extension values already read
    // for this request) — it's the same for a hit or a miss, and independent of
    // `IndexMode::CacheTag` (unlike `entry.cache_tags`, which only carries these when that
    // index is on). A hit's actually-*stored* tags (including anything recorded from
    // `apolloEntityCacheTags` when the entity was originally written, on a *previous* request)
    // live on `entry.cache_entry.invalidation_labels` instead, so that's what gets merged here,
    // mirroring `cache_lookup_root`'s hit-path handling.
    for entry in cache_result.iter() {
        // Fine-grained tags only ever aggregate onto the live `InvalidationLabels` context
        // when `cdn_invalidation_enabled` — matching `CdnInvalidationConfig`'s documented
        // contract — even though `entry.cdn_invalidation_tags` itself is populated whenever
        // either consumer (Redis `cache_tag` index or CDN invalidation) wants it.
        if cdn_invalidation_enabled && !entry.cdn_invalidation_tags.is_empty() {
            InvalidationLabels::add_tags(&request.context, entry.cdn_invalidation_tags.clone());
        }

        // `hit_labels.tags` is the same kind of fine-grained data (persisted from a previous
        // write's `cdn_invalidation_tags`), so it's gated the same way.
        if cdn_invalidation_enabled
            && let Some(hit_labels) = entry
                .cache_entry
                .as_ref()
                .and_then(|cache_entry| cache_entry.invalidation_labels.clone())
        {
            InvalidationLabels::merge(&request.context, hit_labels);
        }

        InvalidationLabels::add_subgraph(&request.context, &name);
        InvalidationLabels::add_type(&request.context, &name, &entry.typename);
    }

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
                ir.cache_entry.as_ref().map(|cache_entry| {
                    // `cache_entry.invalidation_labels` (reconstructed from storage) only ever
                    // carries the `tags` tier — `types`/`subgraphs` are never persisted, only
                    // computed live per-request — so union in this entry's own subgraph/type
                    // label here to show the full set of strings a customer could purge by.
                    let stored_tags = cache_entry
                        .invalidation_labels
                        .as_ref()
                        .map(|labels| labels.tags.clone())
                        .unwrap_or_default();
                    let has_tags = !stored_tags.is_empty();
                    let invalidation_keys = InvalidationLabels {
                        tags: stored_tags,
                        types: HashSet::from([(name.clone(), ir.typename.clone())]),
                        subgraphs: HashSet::from([name.clone()]),
                    }
                    .user_facing_only();

                    CacheKeyContext {
                        key: ir.key.clone(),
                        has_tags,
                        cdn_invalidation_enabled,
                        hashed_private_id: private_id.map(ToString::to_string),
                        invalidation_keys,
                        kind: CacheEntryKind::Entity {
                            typename: ir.typename.clone(),
                            entity_key: ir.entity_key.clone().unwrap_or_default(),
                        },
                        subgraph_name: name.clone(),
                        subgraph_request: request.subgraph_request.body().clone(),
                        source: CacheKeySource::Cache,
                        cache_control: cache_entry.control.clone(),
                        data: serde_json_bytes::json!({"data": cache_entry.data.clone()}),
                        warnings: Vec::new(),
                        should_store: false,
                        indexes: *indexes,
                    }
                    .update_metadata()
                })
            });
            add_cache_keys_to_context(&request.context, debug_cache_keys_ctx)?;
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
            .id(request.id.clone())
            .extensions(Object::new())
            .subgraph_name(request.subgraph_name)
            .context(request.context)
            .build();

        cache_control
            .unwrap_or_default()
            .update_response_headers(response.response.headers_mut())?;

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

// Keep original cache control for every subgraph request (useful for telemetry)
fn save_original_cache_control(
    req_id: SubgraphRequestId,
    context: &Context,
    cache_control: CacheControl,
) {
    context.extensions().with_lock(|l| {
        l.get_or_default_mut::<CacheControls>()
            .insert(req_id, cache_control)
    });
}

async fn cache_store_root_from_response(
    cache: Storage,
    default_subgraph_ttl: Duration,
    response: &subgraph::Response,
    cache_control: CacheControl,
    cache_key: String,
    cache_tags: Vec<CacheTag>,
    cdn_invalidation_tags: Vec<String>,
) -> Result<(), BoxError> {
    if let Some(data) = response.response.body().data.as_ref() {
        let ttl = cache_control
            .ttl()
            .map(Duration::from_secs)
            .unwrap_or(default_subgraph_ttl);

        if response.response.body().errors.is_empty() && cache_control.should_store() {
            let document = Document {
                key: cache_key,
                data: data.clone(),
                control: cache_control,
                cache_tags,
                cdn_invalidation_tags,
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
    indexes: Arc<InvalidationIndexes>,
    cdn_invalidation_enabled: bool,
) -> Result<(), BoxError> {
    let mut data = response.response.body_mut().data.take();

    if let Some(mut entities) = data
        .as_mut()
        .and_then(|v| v.as_object_mut())
        .and_then(|o| o.remove(ENTITIES))
    {
        // if the scope is private but we do not have a way to differentiate users, do not store anything in the cache
        let should_cache_private = !cache_control.private() || private_id.is_some();

        // We check it's not known as a private query because if it's known as a private query that means the private id is already part of the hash
        let update_key_private = if !is_known_private && cache_control.private() {
            private_id.clone()
        } else {
            None
        };

        // cache tags coming from the subgraph response extension allow for granular invalidation
        // by being very specific about _which_ entities we're invalidating. The nested arrays look
        // like this:
        //
        //   "data": {"_entities": [{"id": 1, ...}, {"id": 2, ...}]}
        //   "extensions": {"apolloEntityCacheTags": [["tag-1"], ["tag-2"]]}
        //
        // where entity with id=1 corresponds to ["tag-1"] and entity with id=2 to ["tag-2"]
        //
        // this nested array design was in response to how things were previously: a single flat
        // array with all the invalidation cache tags, which would invalidate _all_ entities when
        // any tag was matched. With nested arrays, each array positionally corresponds to a
        // specific entity and its tags are treated individually to that entity rather than
        // applying to all entities

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
            private_id,
            update_key_private,
            should_cache_private,
            &response.subgraph_name,
            per_entity_surrogate_keys,
            response.context.clone(),
            subgraph_request,
            indexes,
            cdn_invalidation_enabled,
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
    indexes: &InvalidationIndexes,
) -> (String, Vec<CacheTag>) {
    let _ = subgraph_name; // kept for parity; subgraph context is encoded by the caller
    let entity_type = entity_type_opt.unwrap_or(DEFAULT_ROOT_FIELD_TYPE_NAME);

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
    // The by-type tag is only emitted when the Type index is active for this subgraph.
    // Operators who only invalidate by cache tag opt out via `indexes.type: false`.
    // Redis-only: CacheTag::Type entries only ever feed `cache_tags`, which drives Redis's
    // `ZADD` indexing — the CDN header's own tag sourcing (`cdn_invalidation_tags` and the live
    // `InvalidationLabels` aggregator) never reads from `cache_tags`, so gating this behind
    // IndexMode::Type has no effect on Cache-Tag header completeness. Not to be confused with
    // InvalidationLabels.types/add_type(), the CDN header's own coarse type-tier, which is
    // unconditional and unrelated to this index.
    let mut cache_tags: Vec<CacheTag> = Vec::new();
    if indexes.is_enabled(IndexMode::Type) {
        cache_tags.push(CacheTag::Type(entity_type.to_string()));
    }

    (key, cache_tags)
}

struct CacheMetadata {
    cache_key: String,
    cache_tags: Vec<CacheTag>,
    // Fine-grained tag values for CDN `Cache-Tag` persistence, independent of whether
    // `cache_tags` above was populated (that one is gated on `IndexMode::CacheTag` alone). See
    // `Document::cdn_invalidation_tags`.
    cdn_invalidation_tags: Vec<String>,
    // Only set when debug mode is enabled
    entity_key: Option<serde_json_bytes::Map<ByteString, Value>>,
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
    debug: bool,
    indexes: &InvalidationIndexes,
    cdn_invalidation_enabled: bool,
) -> Result<Vec<CacheMetadata>, BoxError> {
    let context = &request.context;
    let authorization = &request.authorization;
    // hash the query and operation name
    let query_hash = hash_query(&request.query_hash);
    // hash more data like variables and authorization status
    let additional_data_hash = hash_additional_data(
        subgraph_name,
        request.subgraph_request.body_mut(),
        context,
        authorization,
    );

    let representations = request
        .subgraph_request
        .body_mut()
        .variables
        .get_mut(REPRESENTATIONS)
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");

    // Get entity key to only get the right fields in representations
    let mut res = Vec::with_capacity(representations.len());
    let entities = representations.len() as u64;
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

        // Get the entity key from `representation`, only needed in debug for the cache debugger
        let representation_entity_key = if debug {
            let selection_set = find_matching_key_field_set(
                representation,
                typename,
                subgraph_name,
                &supergraph_schema,
                subgraph_enums,
            )?;

            get_entity_key_from_selection_set(representation, &selection_set).into()
        } else {
            None
        };

        // Create primary cache key for an entity
        let key = PrimaryCacheKeyEntity {
            subgraph_name,
            entity_type: typename,
            representation,
            subgraph_query_hash: &query_hash,
            additional_data_hash: &additional_data_hash,
            private_id: if is_known_private { private_id } else { None },
        }
        .hash();

        // The by-type tag is only emitted when the Type index is active for this subgraph.
        // Redis-only: CacheTag::Type never reaches the CDN header (its user_value() is None), so
        // gating this behind IndexMode::Type has no effect on Cache-Tag header completeness. Not
        // to be confused with InvalidationLabels.types/add_type(), the CDN header's own coarse
        // type-tier, which is unconditional and unrelated to this index.
        let mut cache_tags: Vec<CacheTag> = Vec::new();
        if indexes.is_enabled(IndexMode::Type) {
            cache_tags.push(CacheTag::Type(typename.to_string()));
        }

        // Skip the schema traversal entirely when nothing would consume its result: the Redis
        // per-tag index and the CDN `Cache-Tag` header aggregator are independent consumers —
        // mirrors the same gate in `cache_lookup_root`.
        let invalidation_cache_keys =
            if indexes.tracks_invalidation_labels(cdn_invalidation_enabled) {
                get_invalidation_entity_keys_from_schema(
                    &supergraph_schema,
                    subgraph_name,
                    subgraph_enums,
                    typename,
                    representation,
                )?
            } else {
                HashSet::new()
            };

        // `cache_tags` feeds the intermediate result on cache misses which gets ZADDed for redis;
        // it's not part of the CDN-invalidation path; each sink below applies its own gate
        // independently rather than sharing one.
        if indexes.is_enabled(IndexMode::CacheTag) {
            cache_tags.extend(invalidation_cache_keys.iter().cloned().map(CacheTag::Tag));
        }

        // Persisted independently of `cache_tags` above (and thus of `IndexMode::CacheTag`) so
        // a later cache hit can rebuild the CDN header even when Redis per-tag indexing is off.
        // `invalidation_cache_keys` is already empty when neither consumer wants it (see the
        // gate this was computed under), so no extra condition is needed here.
        let cdn_invalidation_tags: Vec<String> = invalidation_cache_keys.iter().cloned().collect();

        if cdn_invalidation_enabled {
            InvalidationLabels::add_tags(context, invalidation_cache_keys.into_iter().collect());
        }

        // Restore the `representation` back whole again
        representation.insert(TYPENAME, typename_value);
        let cache_key_metadata = CacheMetadata {
            cache_key: key,
            cache_tags,
            cdn_invalidation_tags,
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

    u64_histogram_with_unit!(
        "apollo.router.operations.response_cache.fetch.entity",
        "Number of entities per subgraph fetch node",
        "{entity}",
        entities,
        "subgraph.name" = subgraph_name.to_string()
    );

    Ok(res)
}

/// Get invalidation keys from @cacheTag directives in supergraph schema for entities
fn get_invalidation_entity_keys_from_schema(
    supergraph_schema: &Arc<Valid<Schema>>,
    subgraph_name: &str,
    subgraph_enums: &HashMap<String, String>,
    typename: &str,
    representations: &serde_json_bytes::Map<ByteString, Value>,
) -> Result<HashSet<String>, anyhow::Error> {
    // `filter_dir`: Check if the `@join_directive` directives are for the current subgraph's cacheTags
    let filter_dir = |dir: &apollo_compiler::ast::Directive| {
        let Ok(name) = dir.argument_by_name("name", supergraph_schema) else {
            return false;
        };
        let Some(name) = name.as_str() else {
            return false;
        };
        if *name != *CACHE_TAG_DIRECTIVE_NAME {
            return false;
        }
        dir.argument_by_name("graphs", supergraph_schema)
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
            .unwrap_or_default()
    };
    // supports both Object types and Interface types (for interface objects with isInterfaceObject: true)
    let all_directives: Vec<_> = match supergraph_schema.get_interface(typename) {
        // Jumping from an interface object
        Some(iface_type) => {
            // In this case, we can only support jumping from an interface object to another
            // interface object.
            // Note: `@cacheTag` can be different across implementation types. If the target entity
            //       type is a interface type (not interface-object), we don't collect the
            //       directives from its implementation types. Because the actual object type (and
            //       thus cache key) can't be determined based only on interface type name. This
            //       may result in cache misses, but it's inherent limitation of interface objects.
            iface_type
                .directives
                .get_all("join__directive")
                .filter(|dir| filter_dir(dir))
                .cloned()
                .collect()
        }
        // Jumping from a non-interface object
        None => {
            let obj_type = supergraph_schema.get_object(typename).ok_or_else(|| {
                FetchError::MalformedRequest {
                    reason: format!("can't find corresponding type for __typename {typename:?}"),
                }
            })?;

            // Target subgraph may implement an interface object. Handle both interface object case
            // and normal interface/implementations case by chaining the object type's directives
            // and those of its implementing interface types.
            // Note: We also need to look up the interface types because `@cacheTag` directives
            //       applied on interface object type is not propagated to implementation types.
            // Note: We don't really support multiple interface objects overlapping each other.
            //       There are multiple issues preventing that from working. Thus, we don't expect
            //       an object type to implement multiple interface types with `@cacheTag` on them
            //       within the same subgraph. So, we will have at most one `@cacheTag` from
            //       interfaces.
            let obj_directives: Vec<_> = obj_type
                .directives
                .get_all("join__directive")
                .filter(|dir| filter_dir(dir))
                .cloned()
                .collect();
            let iface_directives: Vec<_> = obj_type
                .implements_interfaces
                .iter()
                .flat_map(|iface_name| {
                    supergraph_schema
                        .get_interface(iface_name)
                        .iter()
                        .flat_map(|iface| iface.directives.get_all("join__directive").cloned())
                        .collect::<Vec<_>>()
                })
                .filter(|dir| filter_dir(dir))
                .collect();
            obj_directives.into_iter().chain(iface_directives).collect()
        }
    };
    let cache_keys = all_directives.into_iter().filter_map(|dir| {
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
    // It's safe to use representations variables (not only entity keys) because at the composition level we already checked if it was only using entity keys
    vars.insert("$key".to_string(), Value::Object(representations.clone()));
    let invalidation_cache_keys = cache_keys
        .map(|ck| ck.interpolate(&vars).map(|(res, _)| res))
        .collect::<Result<_, _>>()?;
    Ok(invalidation_cache_keys)
}

pub(in crate::plugins) fn find_matching_key_field_set(
    representation: &serde_json_bytes::Map<ByteString, Value>,
    typename: &str,
    subgraph_name: &str,
    supergraph_schema: &Valid<Schema>,
    subgraph_enums: &HashMap<String, String>,
) -> Result<apollo_compiler::executable::SelectionSet, FetchError> {
    // find an entry in the `key_field_sets` that matches the `representation`.
    collect_key_field_sets(typename, subgraph_name, supergraph_schema, subgraph_enums)?
        .find(|field_set| {
            matches_selection_set(representation, &field_set.selection_set)
        })
        .map(|field_set| field_set.selection_set)
        .ok_or_else(|| {
            tracing::trace!("representation does not match any key field set for typename {typename} in subgraph {subgraph_name}");
            FetchError::MalformedRequest {
                reason: format!("unexpected critical internal error for typename {typename} in subgraph {subgraph_name}"),
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
                                "entity_caching.graphql",
                            )
                            .ok()
                    })
            } else {
                None
            }
        }))
}

/// Whether the entity, represented as JSON, matches the parsed @key fields (`selection_set`)
/// * This function mirrors `get_entity_key_from_selection_set` and make sure the representation
///   matches the the shape of `selection_set`.
/// * This function and `get_entity_key_from_selection_set` are separate because this is called for
///   multiple possible `@key` fields to find the matching one, while
///   `get_entity_key_from_selection_set` is only called once the matching `@key` fields is found.
// NB(nullability-note): We allow nullable fields in selection sets (ie, those fields that
// identify an entity, usually [if not always] listed in `@key`). That _doesn't_ mean that
// entities definitively must allow nullable fields, only that we happen to allow it right now.
// It's probably a bit of a schema-development smell to have an entity identifiable by nullable
// fields, but it makes practical sense if you're wanting to cache partial responses.
pub(in crate::plugins) fn matches_selection_set(
    // the JSON representation of the entity data
    representation: &serde_json_bytes::Map<ByteString, Value>,
    // the parsed @key fields to use for matching
    selection_set: &apollo_compiler::executable::SelectionSet,
) -> bool {
    for field in selection_set.root_fields(&Default::default()) {
        // the heart of finding the match: we take the field from the selection
        // set and try to find it in the entity representation;
        let Some(value) = representation.get(field.name.as_str()) else {
            if field.definition.ty.is_non_null() {
                return false;
            } else {
                // allow missing field to match nullable field type (see NB(nullability-note))
                continue;
            }
        };

        // This field selection is not expecting any subdata.
        if field.selection_set.is_empty() {
            // Scalar (or array of scalars) fields are always a match.
            if !is_scalar_or_array_of_scalar(value) {
                // Mismatch: Scalar value was expected.
                return false;
            }
            continue;
        }

        // The field selection is expecting a subdata. See if given `value` matches the shape of
        // its sub-selection set.
        let result = match value {
            Value::Object(obj) => {
                // Recurse into object value
                matches_selection_set(obj, &field.selection_set)
            }

            Value::Array(arr) => {
                // Recurse into array values, filtering out any `null` objects if we're allowed to do so
                // NB: we have to do this here where the field type is known, as the selection set doesn't
                //  include knowledge of whether the type is nullable
                // See NB(nullability-note)
                let list_item_is_nullable = !field.definition.ty.item_type().is_non_null();
                let exclude_value = |value: &&Value| list_item_is_nullable && value.is_null();
                let arr = arr.iter().filter(|value| !exclude_value(value));
                matches_array_of_objects(arr, &field.selection_set)
            }

            // See NB(nullability-note)
            Value::Null => {
                return true;
            }

            // scalar values
            _other => {
                // Mismatch: object or array value was expected.
                false
            }
        };
        if !result {
            return false;
        }
    }
    true
}

fn is_scalar_or_array_of_scalar(value: &Value) -> bool {
    match value {
        Value::Object(_) => false,
        Value::Array(arr) => arr.iter().all(is_scalar_or_array_of_scalar),
        _ => true,
    }
}

/// See if all array items match the shape of the `selection_set`.
/// * Note: The array can be multi-dimensional. (the @key field set can match any levels of nested
///   arrays)
/// * Precondition: `selection_set` must be non-empty.
fn matches_array_of_objects<'a, I: Iterator<Item = &'a Value>>(
    arr: I,
    selection_set: &apollo_compiler::executable::SelectionSet,
) -> bool {
    for item in arr {
        let result = match item {
            Value::Object(obj) => matches_selection_set(obj, selection_set),
            Value::Array(arr) => matches_array_of_objects(arr.iter(), selection_set),
            _other => false,
        };
        if !result {
            return false;
        }
    }
    true
}

// Get the selection set from `representation` and returns the value corresponding to it.
// - Returns None if the representation doesn't match the selection set.
// Note: This function mirrors `hash_representation_inner` in cache/entity.rs.
fn get_entity_key_from_selection_set(
    representation: &serde_json_bytes::Map<ByteString, Value>,
    selection_set: &apollo_compiler::executable::SelectionSet,
) -> serde_json_bytes::Map<ByteString, Value> {
    fn traverse_object(
        state: &mut serde_json_bytes::Map<ByteString, Value>,
        fields: &serde_json_bytes::Map<ByteString, Value>,
        selection_set: &apollo_compiler::executable::SelectionSet,
    ) {
        let default_document = Default::default();
        let sorted_selections = selection_set
            .root_fields(&default_document)
            .sorted_by(|a, b| a.name.cmp(&b.name));
        for field in sorted_selections {
            let key = field.name.as_str();
            let Some(val) = fields.get(key) else {
                continue;
            };
            match val {
                serde_json_bytes::Value::Object(obj) => {
                    let mut obj_state = serde_json_bytes::Map::new();
                    traverse_object(&mut obj_state, obj, &field.selection_set);
                    state.insert(ByteString::from(key), Value::Object(obj_state));
                }
                Value::Array(arr) => {
                    let mut arr_state = Vec::new();
                    traverse_array(&mut arr_state, arr, &field.selection_set);
                    state.insert(ByteString::from(key), Value::Array(arr_state));
                }
                // scalar value
                val => {
                    state.insert(ByteString::from(key), val.clone());
                }
            }
        }
    }

    fn traverse_array(
        state: &mut Vec<Value>,
        items: &[Value],
        selection_set: &apollo_compiler::executable::SelectionSet,
    ) {
        items.iter().for_each(|v| {
            match v {
                serde_json_bytes::Value::Object(obj) => {
                    let mut obj_state = serde_json_bytes::Map::new();
                    traverse_object(&mut obj_state, obj, selection_set);
                    state.push(Value::Object(obj_state));
                }
                Value::Array(arr) => {
                    let mut arr_state = Vec::new();
                    traverse_array(&mut arr_state, arr, selection_set);
                    state.push(Value::Array(arr_state));
                }
                // scalar value
                _ => {
                    state.push(v.clone());
                }
            }
        });
    }

    let mut state = serde_json_bytes::Map::new();
    traverse_object(&mut state, representation, selection_set);

    state
}

/// represents the result of a cache lookup for an entity type and key
struct IntermediateResult {
    key: String,
    cache_tags: Vec<CacheTag>,
    // See `CacheMetadata::cdn_invalidation_tags`.
    cdn_invalidation_tags: Vec<String>,
    typename: String,
    // Only set when debug mode is enabled
    entity_key: Option<serde_json_bytes::Map<ByteString, Value>>,
    cache_entry: Option<CacheEntry>,
}

// build a new list of representations without the ones we got from the cache
#[allow(clippy::type_complexity)]
fn filter_representations(
    subgraph_name: &str,
    subgraph_req_id: &SubgraphRequestId,
    representations: &mut Vec<Value>,
    // keys: Vec<(String, Vec<String>)>,
    keys: Vec<CacheMetadata>,
    mut cache_result: Vec<Option<CacheEntry>>,
    context: &Context,
    record_metrics: bool,
) -> Result<(Vec<Value>, Vec<IntermediateResult>, Option<CacheControl>), BoxError> {
    let mut new_representations: Vec<Value> = Vec::new();
    let mut result = Vec::new();
    let mut cache_hit: HashMap<String, CacheHitMiss> = HashMap::new();
    let mut cache_control = None;
    // Useful for telemetry
    let mut non_updated_cache_control = None;

    for (
        (
            mut representation,
            CacheMetadata {
                cache_key: key,
                cache_tags,
                cdn_invalidation_tags,
                entity_key,
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
                match non_updated_cache_control.as_mut() {
                    None => non_updated_cache_control = Some(entry.control.clone()),
                    Some(c) => *c = c.merge_without_ttl_update(&entry.control),
                }
            }
        }

        result.push(IntermediateResult {
            key,
            cache_tags,
            cdn_invalidation_tags,
            typename,
            cache_entry,
            entity_key,
        });
    }

    if let Some(non_updated_cache_control) = non_updated_cache_control {
        save_original_cache_control(subgraph_req_id.clone(), context, non_updated_cache_control);
    }

    if record_metrics {
        let _ = context.insert(
            CacheMetricContextKey::new(subgraph_name.to_string()),
            CacheSubgraph(cache_hit),
        );
    }

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
    // The original private id fetched from context and hashed to put it in the debug entry
    private_id_for_dbg: Option<String>,
    update_key_private: Option<String>,
    should_cache_private: bool,
    subgraph_name: &str,
    per_entity_surrogate_keys: &[Value],
    context: Context,
    // Only Some if debug is enabled
    subgraph_request: Option<graphql::Request>,
    indexes: Arc<InvalidationIndexes>,
    cdn_invalidation_enabled: bool,
) -> Result<(Vec<Value>, Vec<Error>), BoxError> {
    let ttl = cache_control
        .ttl()
        .map(Duration::from_secs)
        .unwrap_or(default_subgraph_ttl);

    let mut new_entities = Vec::new();
    let mut new_errors = Vec::new();

    let mut inserted_types: HashMap<String, usize> = HashMap::new();
    let mut to_insert: Vec<_> = Vec::new();
    let mut debug_ctx_entries = Vec::new();
    let mut entities_it = entities.drain(..).enumerate();
    // iterate through per-entity cache tags in parallel with entities; tags are matched
    // positionally, meaning the first tag array applies to the first entity, etc
    let mut per_entity_surrogate_keys_it = per_entity_surrogate_keys.iter();

    // insert requested entities and cached entities in the same order as
    // they were requested
    for (
        new_entity_idx,
        IntermediateResult {
            mut key,
            mut cache_tags,
            mut cdn_invalidation_tags,
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

                // Per-entity cache tags from the subgraph's `apolloEntityCacheTags` extension.
                if indexes.tracks_invalidation_labels(cdn_invalidation_enabled)
                    && let Some(Value::Array(keys)) = specific_surrogate_keys
                {
                    let entity_tags: Vec<String> = keys
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_owned())
                        .collect();

                    if !entity_tags.is_empty() {
                        // Recorded whenever either consumer got us into this block (the outer
                        // `if` above), not just when `cdn_invalidation_enabled` — this also
                        // feeds the cache debugger's per-entry tag display, which should show
                        // these tags whenever the Redis `cache_tag` index is on, independent of
                        // CDN invalidation. See `CacheMetadata::cdn_invalidation_tags` —
                        // persisted independently of `cache_tags`/`IndexMode::CacheTag` below.
                        cdn_invalidation_tags.extend(entity_tags.iter().cloned());

                        if cdn_invalidation_enabled {
                            InvalidationLabels::add_tags(&context, entity_tags.clone());
                        }

                        if indexes.is_enabled(IndexMode::CacheTag) {
                            cache_tags.extend(entity_tags.into_iter().map(CacheTag::Tag));
                        }
                    }
                }

                // Prepend the whole-subgraph index entry when active. The by-type and per-tag
                // entries were already appended above; the Subgraph entry is added here so it
                // only lands on entries that are actually being persisted.
                if indexes.is_enabled(IndexMode::Subgraph) {
                    cache_tags.insert(0, CacheTag::Subgraph);
                }

                // Only in debug mode
                if let Some(subgraph_request) = &subgraph_request {
                    debug_ctx_entries.push(
                        CacheKeyContext {
                            key: key.clone(),
                            hashed_private_id: private_id_for_dbg.clone(),
                            invalidation_keys: InvalidationLabels {
                                tags: cdn_invalidation_tags.iter().cloned().collect(),
                                types: HashSet::from([(
                                    subgraph_name.to_string(),
                                    typename.clone(),
                                )]),
                                subgraphs: HashSet::from([subgraph_name.to_string()]),
                            }
                            .user_facing_only(),
                            has_tags: !cdn_invalidation_tags.is_empty(),
                            cdn_invalidation_enabled,
                            kind: CacheEntryKind::Entity {
                                typename: typename.clone(),
                                entity_key: entity_key.clone().unwrap_or_default(),
                            },
                            subgraph_name: subgraph_name.to_string(),
                            subgraph_request: subgraph_request.clone(),
                            source: CacheKeySource::Subgraph,
                            cache_control: cache_control.clone(),
                            data: serde_json_bytes::json!({"data": value.clone()}),
                            warnings: Vec::new(),
                            should_store: false,
                            indexes: *indexes,
                        }
                        .update_metadata(),
                    );
                }
                if !has_errors && cache_control.should_store() && should_cache_private {
                    to_insert.push(Document {
                        control: cache_control.clone(),
                        data: value.clone(),
                        key,
                        cache_tags,
                        cdn_invalidation_tags,
                        expire: ttl,
                    });
                }

                new_entities.push(value);
            }
        }
    }

    // For debug mode
    if !debug_ctx_entries.is_empty() {
        add_cache_keys_to_context(&context, debug_ctx_entries.into_iter())?;
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

async fn connect_or_spawn_reconnection_task(
    config: storage::redis::Config,
    storage: Arc<OnceLock<Storage>>,
    abort_signal: broadcast::Receiver<()>,
) -> Result<(), BoxError> {
    match attempt_connection(&config, storage.clone(), abort_signal.resubscribe()).await {
        Ok(()) => Ok(()),
        Err(err) if config.required_to_start => Err(err),
        Err(_) => {
            tokio::spawn(reattempt_connection(config.clone(), storage, abort_signal));
            Ok(())
        }
    }
}

async fn attempt_connection(
    config: &storage::redis::Config,
    cache_storage: Arc<OnceLock<Storage>>,
    abort_signal: broadcast::Receiver<()>,
) -> Result<(), BoxError> {
    let storage = Storage::new(config, abort_signal)
        .await
        .inspect_err(|err| {
            tracing::error!(
                cache = "response",
                error = %err,
                "could not open connection to Redis for response caching",
            )
        })?;
    let _ = cache_storage.set(storage);

    Ok(())
}

async fn reattempt_connection(
    config: storage::redis::Config,
    cache_storage: Arc<OnceLock<Storage>>,
    mut abort_signal: broadcast::Receiver<()>,
) {
    let mut interval = IntervalStream::new(tokio::time::interval(Duration::from_secs(30)));
    loop {
        tokio::select! {
            biased;
            _ = abort_signal.recv() => {
                break;
            }
            _ = interval.next() => {
                if attempt_connection(&config, cache_storage.clone(), abort_signal.resubscribe()).await.is_ok() {
                    break;
                }
            }
        }
    }
}

pub(crate) type CacheControls = HashMap<SubgraphRequestId, CacheControl>;

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use apollo_compiler::Schema;
    use apollo_compiler::parser::Parser;
    use serde_json_bytes::json;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    use super::Subgraph;
    use super::Ttl;
    use crate::configuration::subgraph::SubgraphConfiguration;
    use crate::plugin::PluginInit;
    use crate::plugin::PluginPrivate;
    use crate::plugins::response_cache::plugin::ResponseCache;
    use crate::plugins::response_cache::plugin::get_entity_key_from_selection_set;
    use crate::plugins::response_cache::plugin::get_invalidation_entity_keys_from_schema;
    use crate::plugins::response_cache::plugin::get_invalidation_root_keys_from_schema;
    use crate::plugins::response_cache::plugin::matches_selection_set;
    use crate::plugins::response_cache::storage::redis::Config;
    use crate::plugins::response_cache::storage::redis::Storage;
    use crate::plugins::response_cache::tests::create_subgraph_conf;
    use crate::services::OperationKind;
    use crate::services::subgraph;

    const SCHEMA: &str = include_str!("../../testdata/orga_supergraph_cache_key.graphql");

    #[tokio::test]
    async fn test_subgraph_enabled() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let (drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(&Config::test(false, "test_subgraph_enabled"), drop_rx)
            .await
            .unwrap();
        let map = serde_json_bytes::from_value(serde_json_bytes::json!({
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
        }))
        .unwrap();
        let subgraphs_conf = create_subgraph_conf(map);

        let mut response_cache = ResponseCache::for_test(
            storage.clone(),
            subgraphs_conf,
            valid_schema.clone(),
            true,
            drop_tx,
            true,
        )
        .await
        .unwrap();

        assert!(response_cache.subgraph_enabled("user"));
        assert!(!response_cache.subgraph_enabled("archive"));
        let subgraph_config = serde_json_bytes::json!({
            "all": {
                "enabled": false
            },
            "subgraphs": response_cache.subgraphs.subgraphs.clone()
        });
        response_cache.subgraphs = Arc::new(serde_json_bytes::from_value(subgraph_config).unwrap());
        assert!(!response_cache.subgraph_enabled("archive"));
        assert!(response_cache.subgraph_enabled("user"));
        assert!(response_cache.subgraph_enabled("orga"));
    }

    async fn get_response_cache_plugin(
        all_enabled: bool,
        all_invalidation_enabled: bool,
        subgraph_enabled: bool,
        subgraph_invalidation_enabled: bool,
    ) -> ResponseCache {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let (drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(&Config::test(false, &Uuid::new_v4().to_string()), drop_rx)
            .await
            .unwrap();

        ResponseCache::for_test(
            storage.clone(),
            serde_json_bytes::from_value(serde_json_bytes::json!({
                "all": {
                    "enabled": all_enabled,
                    "ttl": "10s",
                    "invalidation": {
                        "enabled": all_invalidation_enabled,
                        "shared_key": "test"
                    }
                },
                "subgraphs": {
                    "user": {
                        "enabled": subgraph_enabled,
                        "invalidation": {
                            "enabled": subgraph_invalidation_enabled,
                            "shared_key": "test"
                        }
                    }
                }
            }))
            .unwrap(),
            valid_schema.clone(),
            true,
            drop_tx,
            true,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_redis_connection_disabled() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let config: super::Config = serde_json_bytes::from_value(serde_json_bytes::json!({
            "enabled": true,
            "subgraph": {
                "all": {
                    "enabled": false,
                    "ttl": "10s",
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "namespace": Uuid::new_v4().to_string(),
                        "pool_size": 1,
                        "required_to_start": true,
                    }
                },
                "subgraphs": {
                    "user": {
                        "enabled": false
                    }
                }
            }
        }))
        .unwrap();
        let response_cache = ResponseCache::new(PluginInit::fake_new(
            config,
            Arc::new(valid_schema.to_string()),
        ))
        .await
        .unwrap();

        assert!(
            response_cache.storage.all.is_none()
                || response_cache.storage.all.as_ref().unwrap().get().is_none(),
            "Redis storage is set globally"
        );
        assert!(
            response_cache.storage.subgraphs.is_empty(),
            "Redis storage is set for a subgraph"
        );
        // ----- Disable globally the plugin ----
        let config: super::Config = serde_json_bytes::from_value(serde_json_bytes::json!({
            "enabled": false,
            "subgraph": {
                "all": {
                    "enabled": true,
                    "ttl": "10s",
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "namespace": Uuid::new_v4().to_string(),
                        "pool_size": 1,
                        "required_to_start": true,
                    }
                },
                "subgraphs": {
                    "user": {
                        "enabled": true
                    }
                }
            }
        }))
        .unwrap();
        let response_cache = ResponseCache::new(PluginInit::fake_new(
            config,
            Arc::new(valid_schema.to_string()),
        ))
        .await
        .unwrap();

        assert!(
            response_cache.storage.all.is_none()
                || response_cache.storage.all.as_ref().unwrap().get().is_none(),
            "Redis storage is set globally"
        );
        assert!(
            response_cache.storage.subgraphs.is_empty(),
            "Redis storage is set for a subgraph"
        );
    }

    #[tokio::test]
    async fn test_no_redis_conf_provided_should_fail() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let config: super::Config = serde_json_bytes::from_value(serde_json_bytes::json!({
            "enabled": true,
            "subgraph": {
                "all": {
                    "enabled": true,
                    "ttl": "10s",
                },
                "subgraphs": {
                    "user": {
                        "enabled": true
                    },
                    "inventory": {
                        "enabled": true
                    }
                }
            }
        }))
        .unwrap();
        assert!(
            ResponseCache::new(PluginInit::fake_new(
                config,
                Arc::new(valid_schema.to_string()),
            ))
            .await
            .is_err(),
            "The plugin should not start properly if caching is enabled but no redis provided"
        );
    }

    #[tokio::test]
    #[rstest::rstest]
    // Globally disabled
    #[case(false, true, true)]
    // Disable for all subgraphs
    #[case(true, false, false)]
    async fn test_no_redis_conf_provided_but_disabled_should_succeed(
        #[case] global_enabled: bool,
        #[case] all_enabled: bool,
        #[case] subgraph_enabled: bool,
    ) {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let config: super::Config = serde_json_bytes::from_value(serde_json_bytes::json!({
            "enabled": global_enabled,
            "subgraph": {
                "all": {
                    "enabled": all_enabled,
                    "ttl": "10s",
                },
                "subgraphs": {
                    "user": {
                        "enabled": subgraph_enabled
                    },
                    "inventory": {
                        "enabled": subgraph_enabled
                    }
                }
            }
        }))
        .unwrap();
        let response_cache = ResponseCache::new(PluginInit::fake_new(
            config,
            Arc::new(valid_schema.to_string()),
        ))
        .await
        .unwrap();
        if !global_enabled {
            assert!(!response_cache.enabled);
        }
        assert!(
            response_cache.storage.all.is_none()
                || response_cache.storage.all.as_ref().unwrap().get().is_none(),
            "Redis storage is set globally"
        );
        assert!(
            response_cache.storage.subgraphs.is_empty(),
            "Redis storage is set for a subgraph"
        );
    }

    #[tokio::test]
    async fn test_redis_connection_enabled_multiple_subgraphs() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let config: super::Config = serde_json_bytes::from_value(serde_json_bytes::json!({
            "enabled": true,
            "subgraph": {
                "all": {
                    "enabled": false,
                    "ttl": "10s",
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "namespace": Uuid::new_v4().to_string(),
                        "pool_size": 1,
                        "required_to_start": true,
                    }
                },
                "subgraphs": {
                    "user": {
                        "enabled": false
                    },
                    "inventory": {
                        "enabled": true
                    }
                }
            }
        }))
        .unwrap();
        let response_cache = ResponseCache::new(PluginInit::fake_new(
            config,
            Arc::new(valid_schema.to_string()),
        ))
        .await
        .unwrap();

        assert!(
            response_cache.storage.all.is_none()
                || response_cache.storage.all.as_ref().unwrap().get().is_none(),
            "Redis storage is set globally"
        );
        assert_eq!(
            response_cache.storage.subgraphs.len(),
            1,
            "Redis storage is not set for a subgraph"
        );
    }

    #[tokio::test]
    #[rstest::rstest]
    // Everything enabled
    #[case(true, true)]
    // Enable caching only for a specific subgraph should enable redis
    #[case(false, true)]
    // Enable caching for all subgraphs should enable redis
    #[case(true, false)]
    async fn test_redis_connection_enabled(
        #[case] all_enabled: bool,
        #[case] subgraph_enabled: bool,
    ) {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let config: super::Config = serde_json_bytes::from_value(serde_json_bytes::json!({
            "enabled": true,
            "subgraph": {
                "all": {
                    "enabled": all_enabled,
                    "ttl": "10s",
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "namespace": Uuid::new_v4().to_string(),
                        "pool_size": 1,
                        "required_to_start": true,
                    }
                },
                "subgraphs": {
                    "user": {
                        "enabled": subgraph_enabled
                    }
                }
            }
        }))
        .unwrap();
        let response_cache = ResponseCache::new(PluginInit::fake_new(
            config,
            Arc::new(valid_schema.to_string()),
        ))
        .await
        .unwrap();

        if all_enabled {
            assert!(
                response_cache.storage.all.is_some()
                    && response_cache.storage.all.as_ref().unwrap().get().is_some(),
                "Redis storage is not set globally"
            );
        } else {
            assert!(
                response_cache.storage.all.is_none()
                    || response_cache.storage.all.as_ref().unwrap().get().is_none(),
                "Redis storage is set globally"
            );
        }
        if subgraph_enabled && !all_enabled {
            assert_eq!(
                response_cache.storage.subgraphs.len(),
                1,
                "Redis storage is set for a subgraph"
            );
        } else {
            assert!(
                response_cache.storage.subgraphs.is_empty(),
                "Redis storage is not set for a subgraph"
            );
        }
    }

    #[tokio::test]
    #[rstest::rstest]
    // Everything enabled
    #[case(true, true, true, true)]
    // Enable invalidation for every subgraphs except for a specific subgraph should enable invalidation endpoint
    #[case(true, true, true, false)]
    // Enable invalidation only for a specific subgraph should enable invalidation endpoint
    #[case(true, false, true, true)]
    // Disable globally both caching and invalidation but enable invalidation only for a specific subgraph should enable invalidation endpoint
    #[case(false, false, true, true)]
    async fn test_invalidation_endpoint_enabled(
        #[case] all_enabled: bool,
        #[case] all_invalidation_enabled: bool,
        #[case] subgraph_enabled: bool,
        #[case] subgraph_invalidation_enabled: bool,
    ) {
        let response_cache = get_response_cache_plugin(
            all_enabled,
            all_invalidation_enabled,
            subgraph_enabled,
            subgraph_invalidation_enabled,
        )
        .await;
        assert!(!response_cache.web_endpoints().is_empty());
    }

    #[tokio::test]
    #[rstest::rstest]
    // Disable everything should disable invalidation endpoint
    #[case(false, false, false, false)]
    // Enable invalidation for a specific subgraph but disable everything else should disable invalidation endpoint
    #[case(false, true, false, false)]
    // Enable invalidation both for a specific subgraph and all subgraphs but disable caching everywhere should disable invalidation endpoint
    #[case(false, true, false, true)]
    // Only enable caching but not invalidation should disable invalidation endpoint
    #[case(true, false, true, false)]
    // Only enable caching for all subgraphs but not invalidation should disable invalidation endpoint
    #[case(true, false, false, false)]
    // Only enable invalidation for a specific subgraph that disabled caching should disable invalidation endpoint
    #[case(true, false, false, true)]
    async fn test_invalidation_endpoint_disabled(
        #[case] all_enabled: bool,
        #[case] all_invalidation_enabled: bool,
        #[case] subgraph_enabled: bool,
        #[case] subgraph_invalidation_enabled: bool,
    ) {
        let response_cache = get_response_cache_plugin(
            all_enabled,
            all_invalidation_enabled,
            subgraph_enabled,
            subgraph_invalidation_enabled,
        )
        .await;
        assert!(response_cache.web_endpoints().is_empty());
    }

    #[tokio::test]
    async fn test_invalidation_endpoint_enabled_multiple_subgraphs() {
        let mut response_cache = get_response_cache_plugin(false, false, true, false).await;
        // Disable invalidation globally with one specific subgraph configuration with invalidation disabled and another one enabled should enable invalidation endpoint
        response_cache.subgraphs = Arc::new(
            serde_json_bytes::from_value(serde_json_bytes::json!({
                "all": {
                    "enabled": false,
                    "ttl": "10s",
                    "invalidation": {
                        "enabled": false,
                        "shared_key": "test"
                    }
                },
                "subgraphs": {
                    "user": {
                        "enabled": true,
                        "invalidation": {
                            "enabled": false,
                            "shared_key": "test"
                        }
                    },
                    "posts": {
                        "enabled": true,
                        "invalidation": {
                            "enabled": true,
                            "shared_key": "test"
                        }
                    }
                }
            }))
            .unwrap(),
        );

        assert!(
            !response_cache.web_endpoints().is_empty(),
            "Disable invalidation globally with one specific subgraph configuration with invalidation disabled and another one enabled should enable invalidation endpoint"
        );
    }

    #[tokio::test]
    async fn test_subgraph_ttl() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let (drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(&Config::test(false, "test_subgraph_ttl"), drop_rx)
            .await
            .unwrap();
        let map = serde_json_bytes::from_value(serde_json_bytes::json!({
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
        }))
        .unwrap();

        let mut response_cache = ResponseCache::for_test(
            storage.clone(),
            create_subgraph_conf(map),
            valid_schema.clone(),
            true,
            drop_tx,
            true,
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

    #[test]
    fn test_matches_selection_set_handles_arrays() {
        // Simulate the real-world Availability type scenario
        let schema_text = r#"
            type Query {
                test: Test
            }
            type Test {
                id: ID!
                locale: String!
                lists: [List!]!
                list: [List!]!
            }
            type List {
                id: ID!
                date: Int!
                quantity: Int!
                location: String!
            }
        "#;
        let schema = Schema::parse_and_validate(schema_text, "test.graphql").unwrap();

        let mut parser = Parser::new();
        let field_set = parser
            .parse_field_set(
                &schema,
                apollo_compiler::ast::NamedType::new("Test").unwrap(),
                "id locale lists { id date quantity location } list { id date quantity location }",
                "test.graphql",
            )
            .unwrap();

        // Test with complex nested array structure
        let representation = json!({
            "id": "TEST123",
            "locale": "en_US",
            "lists": [
                {
                    "id": "LIST1",
                    "date": 20240101,
                    "quantity": 50,
                    "location": "WAREHOUSE_A"
                }
            ],
            "list": [
                {
                    "id": "LIST2",
                    "date": 20240101,
                    "quantity": 100,
                    "location": "WAREHOUSE_A"
                },
                {
                    "id": "LIST3",
                    "date": 20240102,
                    "quantity": 75,
                    "location": "WAREHOUSE_B"
                }
            ]
        })
        .as_object()
        .unwrap()
        .clone();

        assert!(
            matches_selection_set(&representation, &field_set.selection_set),
            "complex nested arrays should match"
        );
    }

    fn repr_matches_selection_set_for_schema(
        schema: &str,
        named_type: &str,
        selection_text: &str,
        representation: serde_json_bytes::Value,
    ) -> bool {
        let schema = Schema::parse_and_validate(schema, "test.graphql")
            .expect("should be able to parse schema");

        let mut parser = Parser::new();
        let field_set = parser
            .parse_field_set(
                &schema,
                apollo_compiler::ast::NamedType::new(named_type).unwrap(),
                selection_text,
                "test.graphql",
            )
            .expect("should be able to parse field set");

        matches_selection_set(
            representation.as_object().expect("must provide an object"),
            &field_set.selection_set,
        )
    }

    #[rstest::rstest]
    #[case::null_list(json!(null))]
    #[case::null_element(json!([null]))]
    #[case::null_element(json!([{"id": "TEST1"}, null]))]
    #[case::null_value_for_nullable_field(json!([{"id": "TEST1"}]))]
    #[case::null_value_for_nullable_field(json!([{"id": "TEST1", "quantity": 5}]))]
    #[case::multiple_differently_null_objects(json!([{"id": "TEST1"}, null, {"id": "TEST3", "quantity": null}]))]
    fn test_matches_selection_set_handles_arrays_with_nullable_elements(
        #[case] list_repr: serde_json_bytes::Value,
    ) {
        let schema_text = r#"
            type Query {
                test: Test
            }
            type Test {
                id: ID!
                list: [NullableListElement]
            }
            type NullableListElement {
                id: ID!
                quantity: Int
                inStock: Boolean
            }
        "#;

        let named_type = "Test";
        let selection_text = "id list { id quantity inStock }";

        let representation = json!({
            "id": "TEST123",
            "list": list_repr
        });

        let matches_selection_set = repr_matches_selection_set_for_schema(
            schema_text,
            named_type,
            selection_text,
            representation,
        );
        assert!(matches_selection_set);
    }

    #[rstest::rstest]
    #[case::null_element(json!([null]))]
    #[case::null_element(json!([{"id": "TEST1"}, null]))]
    #[case::null_value_for_nonnullable_field(json!([{}]))]
    #[case::null_value_for_nonnullable_field(json!([{"quantity": 5}]))]
    #[case::null_value_for_nonnullable_field(json!([{"id": "TEST1"}, {}]))]
    #[case::null_value_for_nonnullable_field(json!([{"id": "TEST1"}, {"quantity": 5}]))]
    fn test_matches_selection_set_handles_arrays_with_non_nullable_elements(
        #[case] list_repr: serde_json_bytes::Value,
    ) {
        // NB: same as test_matches_selection_set_handles_arrays_with_nullable_elements but with a
        //  NonNullableListElement! rather than a NullableListElement
        let schema_text = r#"
            type Query {
                test: Test
            }
            type Test {
                id: ID!
                list: [NonNullableListElement!]
            }
            type NonNullableListElement {
                id: ID!
                quantity: Int
                inStock: Boolean
            }
        "#;

        let named_type = "Test";
        let selection_text = "id list { id quantity inStock }";

        // Test with complex nested array structure
        let representation = json!({
            "id": "TEST123",
            "list": list_repr
        });

        let matches_selection_set = repr_matches_selection_set_for_schema(
            schema_text,
            named_type,
            selection_text,
            representation,
        );
        assert!(!matches_selection_set);
    }

    #[test]
    fn test_matches_selection_subset_handles_arrays() {
        // Simulate the real-world Availability type scenario
        let schema_text = r#"
            type Query {
                test: Test
            }
            type Test {
                id: ID!
                locale: String!
                lists: [List!]!
                list: [List!]!
            }
            type List {
                id: ID!
                date: Int!
                quantity: Int!
                location: String!
            }
        "#;
        let schema = Schema::parse_and_validate(schema_text, "test.graphql").unwrap();

        let mut parser = Parser::new();
        let field_set = parser
            .parse_field_set(
                &schema,
                apollo_compiler::ast::NamedType::new("Test").unwrap(),
                "id locale lists { id date quantity location } list { id date quantity location }",
                "test.graphql",
            )
            .unwrap();

        // Test with complex nested array structure
        let representation = json!({
            "id": "TEST123",
            "locale": "en_US",
            "lists": [
                {
                    "id": "LIST1",
                    "date": 20240101,
                    "quantity": 50
                }
            ],
            "list": [
                {
                    "id": "LIST2",
                    "date": 20240101,
                    "quantity": 100,
                    "location": "WAREHOUSE_A"
                },
                {
                    "id": "LIST3",
                    "date": 20240102,
                    "quantity": 75,
                    "location": "WAREHOUSE_B"
                }
            ]
        })
        .as_object()
        .unwrap()
        .clone();

        assert!(!matches_selection_set(
            &representation,
            &field_set.selection_set
        ),);

        let field_set = parser
            .parse_field_set(
                &schema,
                apollo_compiler::ast::NamedType::new("Test").unwrap(),
                "id locale lists { id date quantity } list { id date quantity location }",
                "test.graphql",
            )
            .unwrap();

        assert!(
            matches_selection_set(&representation, &field_set.selection_set),
            "complex nested arrays should match"
        );
    }

    #[test]
    fn test_matches_selection_set_handles_null() {
        // Note the nullable type, Nullable; this represents when you have some entity you want to
        // identify via nullable fields, which is most useful when you're wanting to cache partial
        // responses (what does it mean to partially identify a thing? Everything is all and only
        // itself, no more or less--be careful in reading this test as saying something important
        // about how entities _should_ be identified with respect to nullable fields)
        let schema_text = r#"
            type Query {
                test: Test
            }
            type Test {
                id: ID!
                nullable: Nullable
            }
            type Nullable {
                id: ID!
            }
        "#;

        let schema = Schema::parse_and_validate(schema_text, "test.graphql").unwrap();
        let mut parser = Parser::new();
        let field_set = parser
            .parse_field_set(
                &schema,
                apollo_compiler::ast::NamedType::new("Test").unwrap(),
                "id nullable { id }",
                "test.graphql",
            )
            .unwrap();

        // Note second location: it's `null`
        let representation = json!({
            "id": "TEST123",
            "nullable": null,
        })
        .as_object()
        .unwrap()
        .clone();

        assert!(
            matches_selection_set(&representation, &field_set.selection_set),
            "complex nested arrays should match"
        );
    }

    #[test]
    fn test_take_selection_set_handles_arrays() {
        // Simulate the real-world Availability type scenario
        let schema_text = r#"
            type Query {
                test: Test
            }
            type Test {
                id: ID!
                locale: String!
                lists: [List!]!
                list: [List!]!
            }
            type List {
                id: ID!
                date: Int!
                quantity: Int!
                location: String!
            }
        "#;
        let schema = Schema::parse_and_validate(schema_text, "test.graphql").unwrap();

        let mut parser = Parser::new();
        let field_set = parser
            .parse_field_set(
                &schema,
                apollo_compiler::ast::NamedType::new("Test").unwrap(),
                "id locale lists { id date quantity location } list { id date quantity location }",
                "test.graphql",
            )
            .unwrap();

        // Test with complex nested array structure
        let representation = json!({
            "id": "TEST123",
            "locale": "en_US",
            "lists": [
                {
                    "id": "LIST1",
                    "date": 20240101,
                    "quantity": 50,
                    "location": "WAREHOUSE_A"
                }
            ],
            "list": [
                {
                    "id": "LIST2",
                    "date": 20240101,
                    "quantity": 100,
                    "location": "WAREHOUSE_A"
                },
                {
                    "id": "LIST3",
                    "date": 20240102,
                    "quantity": 75,
                    "location": "WAREHOUSE_B"
                }
            ]
        })
        .as_object()
        .unwrap()
        .clone();

        assert!(matches_selection_set(
            &representation,
            &field_set.selection_set
        ));
        let entity_key =
            get_entity_key_from_selection_set(&representation, &field_set.selection_set);
        assert_eq!(
            &entity_key,
            json!({
                "id": "TEST123",
                "locale": "en_US",
                "lists": [
                    {
                        "id": "LIST1",
                        "date": 20240101,
                        "quantity": 50,
                        "location": "WAREHOUSE_A"
                    }
                ],
                "list": [
                    {
                        "id": "LIST2",
                        "date": 20240101,
                        "quantity": 100,
                        "location": "WAREHOUSE_A"
                    },
                    {
                        "id": "LIST3",
                        "date": 20240102,
                        "quantity": 75,
                        "location": "WAREHOUSE_B"
                    }
                ]
            })
            .as_object()
            .unwrap()
        );
    }

    #[test]
    fn test_take_selection_subset_handles_arrays() {
        // Simulate the real-world Availability type scenario
        let schema_text = r#"
            type Query {
                test: Test
            }
            type Test {
                id: ID!
                locale: String!
                lists: [List!]!
                list: [List!]!
            }
            type List {
                id: ID!
                date: Int!
                quantity: Int!
                location: String!
            }
        "#;
        let schema = Schema::parse_and_validate(schema_text, "test.graphql").unwrap();

        let mut parser = Parser::new();
        let field_set = parser
            .parse_field_set(
                &schema,
                apollo_compiler::ast::NamedType::new("Test").unwrap(),
                "id locale lists { id date quantity } list { id quantity location }",
                "test.graphql",
            )
            .unwrap();

        // Test with complex nested array structure
        let representation = json!({
            "id": "TEST123",
            "locale": "en_US",
            "lists": [
                {
                    "id": "LIST1",
                    "date": 20240101,
                    "quantity": 50,
                    "location": "WAREHOUSE_A"
                }
            ],
            "list": [
                {
                    "id": "LIST2",
                    "date": 20240101,
                    "quantity": 100,
                    "location": "WAREHOUSE_A"
                },
                {
                    "id": "LIST3",
                    "date": 20240102,
                    "quantity": 75,
                    "location": "WAREHOUSE_B"
                }
            ]
        })
        .as_object()
        .unwrap()
        .clone();

        assert!(matches_selection_set(
            &representation,
            &field_set.selection_set
        ));
        let entity_key =
            get_entity_key_from_selection_set(&representation, &field_set.selection_set);
        assert_eq!(
            &entity_key,
            json!({
                "id": "TEST123",
                "locale": "en_US",
                "lists": [
                    {
                        "id": "LIST1",
                        "date": 20240101,
                        "quantity": 50
                    }
                ],
                "list": [
                    {
                        "id": "LIST2",
                        "quantity": 100,
                        "location": "WAREHOUSE_A"
                    },
                    {
                        "id": "LIST3",
                        "quantity": 75,
                        "location": "WAREHOUSE_B"
                    }
                ]
            })
            .as_object()
            .unwrap()
        );
    }

    #[test]
    fn test_get_invalidation_root_keys_from_schema() {
        // Simulate the real-world Availability type scenario
        let schema_text = r#"
            directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            input join__ContextArgument {
              name: String!
              type: String!
              context: String!
              selection: join__FieldValue!
            }

            scalar join__DirectiveArguments

            scalar join__FieldSet

            scalar join__FieldValue

            enum join__Graph {
              USER @join__graph(name: "USER", url: "none")
              TEST @join__graph(name: "TEST", url: "none")
            }

            scalar link__Import

            enum link__Purpose {
              """
              `SECURITY` features provide metadata necessary to securely resolve fields.
              """
              SECURITY

              """
              `EXECUTION` features provide metadata necessary for operation execution.
              """
              EXECUTION
            }

            type Query {
                test: Test
                testByCountry(id: ID!, country: Country!): Test @join__directive(
                    graphs: [USER]
                    name: "federation__cacheTag"
                    args: { format: "test-{$args.id}-{$args.country}" }
                )
                @join__directive(
                    graphs: [USER]
                    name: "federation__cacheTag"
                    args: { format: "test-{$args.country}" }
                )
                @join__directive(
                    graphs: [USER]
                    name: "federation__cacheTag"
                    args: { format: "test" }
                )
            }

            enum Country {
                BE
                FR
            }

            type Test {
                id: ID!
                locale: String!
                lists: [List!]!
                list: [List!]!
            }
            type List {
                id: ID!
                date: Int!
                quantity: Int!
                location: String!
            }
        "#;
        let schema = Arc::new(Schema::parse_and_validate(schema_text, "test.graphql").unwrap());
        let query = r#"query Test {
          testByCountry(id: "2", country: BE) {
            locale
          }
        }"#;
        let mut sub_request = subgraph::Request::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(
                        crate::graphql::Request::builder()
                            .query(query)
                            .operation_name("Test")
                            .build(),
                    )
                    .unwrap(),
            )
            .operation_kind(OperationKind::Query)
            .subgraph_name("USER")
            .build();
        sub_request.executable_document = Some(Arc::new(
            apollo_compiler::ExecutableDocument::parse_and_validate(&schema, query, "test.graphql")
                .unwrap(),
        ));
        let subgraph_enums: HashMap<String, String> = [("USER".to_string(), "USER".to_string())]
            .into_iter()
            .collect();
        let cache_tags =
            get_invalidation_root_keys_from_schema(&sub_request, &subgraph_enums, schema.clone())
                .unwrap();

        assert_eq!(
            cache_tags,
            [
                "test".to_string(),
                "test-BE".to_string(),
                "test-2-BE".to_string()
            ]
            .into_iter()
            .collect()
        );
    }

    // makes sure interface objects (eg, `interface Item` below) are able to be used for
    // invalidation entity keys
    // Case #1: Jumping into an interface object from a non-interface object subgraph as an object
    // type.
    #[test]
    fn test_interface_object_typename_lookup_inbound() {
        let schema_text = r#"
                 directive @join__type(graph: join__Graph!, key: join__FieldSet, isInterfaceObject: Boolean! = false) repeatable on
     OBJECT | INTERFACE
                 directive @join__graph(name: String!, url: String!) on ENUM_VALUE
                 directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
                 directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
                 scalar join__FieldSet
                 scalar join__DirectiveArguments

                 enum join__Graph {
                  SEARCH @join__graph(name: "search", url: "http://search")
                  INVENTORY @join__graph(name: "inventory", url: "http://inventory")
                }

                type Query { dummy: String }

                interface Item
                    @join__type(graph: SEARCH, key: "id")
                    @join__type(graph: INVENTORY, key: "id", isInterfaceObject: true)
                    @join__directive(graphs: [INVENTORY], name: "federation__cacheTag", args: {format: "Item-{$key.id}"})
                {
                    id: ID!
                }

                type Book implements Item
                    @join__implements(graph: SEARCH, interface: "Item")
                    @join__type(graph: SEARCH, key: "id")
                {
                    id: ID!
                    isbn: String!
                }
              "#;

        let schema = Arc::new(Schema::parse_and_validate(schema_text, "schema.graphql").unwrap());
        let subgraph_enums = HashMap::from([
            ("SEARCH".into(), "search".into()),
            ("INVENTORY".into(), "inventory".into()),
        ]);
        // Jumping from "search" to "inventory" via the object type "Book"
        let representation = serde_json_bytes::json!({"__typename": "Book", "id": "123"})
            .as_object()
            .unwrap()
            .clone();

        let result = get_invalidation_entity_keys_from_schema(
            &schema,
            "inventory",
            &subgraph_enums,
            "Book",
            &representation,
        )
        .expect("should handle interface object typename");
        assert_eq!(result.into_iter().collect::<Vec<_>>(), [r#"Item-123"#]);
    }

    #[test]
    fn test_interface_object_typename_lookup_outbound() {
        let schema_text = r#"
                 directive @join__type(graph: join__Graph!, key: join__FieldSet, isInterfaceObject: Boolean! = false) repeatable on
     OBJECT | INTERFACE
                 directive @join__graph(name: String!, url: String!) on ENUM_VALUE
                 directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
                 directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
                 scalar join__FieldSet
                 scalar join__DirectiveArguments

                 enum join__Graph {
                  SEARCH @join__graph(name: "search", url: "http://search")
                  INVENTORY @join__graph(name: "inventory", url: "http://inventory")
                }

                type Query { dummy: String }

                interface Item
                    @join__type(graph: SEARCH, key: "id")
                    @join__type(graph: INVENTORY, key: "id", isInterfaceObject: true)
                {
                    id: ID!
                }

                type Book implements Item
                    @join__implements(graph: SEARCH, interface: "Item")
                    @join__type(graph: SEARCH, key: "id")
                    @join__directive(graphs: [SEARCH], name: "federation__cacheTag", args: {format: "Book-{$key.id}"})
                {
                    id: ID!
                    isbn: String!
                }
              "#;

        let schema = Arc::new(Schema::parse_and_validate(schema_text, "schema.graphql").unwrap());
        let subgraph_enums = HashMap::from([
            ("SEARCH".into(), "search".into()),
            ("INVENTORY".into(), "inventory".into()),
        ]);
        // Jumping from "search" to "inventory" via the interface object "Item"
        let representation = serde_json_bytes::json!({"__typename": "Item", "id": "123"})
            .as_object()
            .unwrap()
            .clone();

        let result = get_invalidation_entity_keys_from_schema(
            &schema,
            "inventory",
            &subgraph_enums,
            "Item",
            &representation,
        )
        .expect("should handle interface object typename");
        // Currently, nothing matches.
        assert_eq!(result.len(), 0);
    }

    // makes sure interface objects (eg, `interface Item` below) are able to be used for
    // invalidation entity keys
    // Case #1: Jumping into an interface object from a non-interface object subgraph as an object
    // type.
    #[test]
    fn test_interface_object_typename_into_interface_object() {
        let schema_text = r#"
                 directive @join__type(graph: join__Graph!, key: join__FieldSet, isInterfaceObject: Boolean! = false) repeatable on
     OBJECT | INTERFACE
                 directive @join__graph(name: String!, url: String!) on ENUM_VALUE
                 directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
                 directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
                 scalar join__FieldSet
                 scalar join__DirectiveArguments

                 enum join__Graph {
                  SEARCH @join__graph(name: "search", url: "http://search")
                  INVENTORY @join__graph(name: "inventory", url: "http://inventory")
                  IRRELEVANT @join__graph(name: "irrelevant", url: "http://irrelevant")
                }

                type Query { dummy: String }

                interface Item
                    @join__type(graph: SEARCH, key: "id", isInterfaceObject: true)
                    @join__type(graph: INVENTORY, key: "id", isInterfaceObject: true)
                    @join__type(graph: IRRELEVANT, key: "id")
                    @join__directive(graphs: [INVENTORY], name: "federation__cacheTag", args: {format: "Item-{$key.id}"})
                {
                    id: ID!
                }

                type Book implements Item
                    @join__implements(graph: IRRELEVANT, interface: "Item")
                    @join__type(graph: IRRELEVANT, key: "id")
                {
                    id: ID!
                    isbn: String!
                }
              "#;

        let schema = Arc::new(Schema::parse_and_validate(schema_text, "schema.graphql").unwrap());
        let subgraph_enums = HashMap::from([
            ("INVENTORY".into(), "inventory".into()),
            ("SEARCH".into(), "search".into()),
            ("IRRELEVANT".into(), "irrelevant".into()),
        ]);
        // Jumping from "search" to "inventory" via the interface object "Item"
        let representation = serde_json_bytes::json!({"__typename": "Item", "id": "123"})
            .as_object()
            .unwrap()
            .clone();

        let result = get_invalidation_entity_keys_from_schema(
            &schema,
            "inventory",
            &subgraph_enums,
            "Item",
            &representation,
        )
        .expect("should handle interface object typename");
        assert_eq!(result.into_iter().collect::<Vec<_>>(), [r#"Item-123"#]);
    }

    // makes sure that when an interface isn't usable for entity resolution (ie, `isInterfaceObject:
    // false`) the typename is the concrete type and is findable via the object type
    #[test]
    fn test_concrete_type_when_interface_object_is_false() {
        // NB: isInterfaceObject defaults to false
        let schema_text = r#"
            directive @join__type(graph: join__Graph!, key: join__FieldSet, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE
            directive @join__graph(name: String!, url: String!) on ENUM_VALUE
            scalar join__FieldSet

            enum join__Graph {
              PRODUCTS @join__graph(name: "products", url: "http://products")
            }

            type Query { dummy: String }

            # Regular interface (not an interface object)
            interface Item {
              id: ID!
            }

            # Concrete type that implements the interface
            type Product implements Item @join__type(graph: PRODUCTS, key: "id") {
              id: ID!
              name: String
            }
        "#;

        let schema = Arc::new(Schema::parse_and_validate(schema_text, "schema.graphql").unwrap());
        let subgraph_enums = HashMap::from([("PRODUCTS".into(), "products".into())]);

        // when isInterfaceObject: false, typename in _entities is the concrete type "Product"
        let representation = serde_json_bytes::json!({
            "__typename": "Product",  // NB: concrete type, not "Item"
            "id": "123"
        })
        .as_object()
        .unwrap()
        .clone();

        let result = get_invalidation_entity_keys_from_schema(
            &schema,
            "products",
            &subgraph_enums,
            "Product", // concrete object typename (ie, normal case)
            &representation,
        );

        assert!(
            result.is_ok(),
            "should handle concrete type (isInterfaceObject: false)"
        );
    }

    #[test]
    fn config_include_cache_control_header_on_router_response_defaults_to_true() {
        let config: super::Config = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "subgraph": {
                "all": {
                    "enabled": true,
                    "ttl": "24h"
                }
            }
        }))
        .unwrap();
        assert!(config.include_cache_control_header_on_router_response);
    }

    #[test]
    fn config_include_cache_control_header_on_router_response_false() {
        let config: super::Config = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "include_cache_control_header_on_router_response": false,
            "subgraph": {
                "all": {
                    "enabled": true,
                    "ttl": "24h"
                }
            }
        }))
        .unwrap();
        assert!(!config.include_cache_control_header_on_router_response);
    }
}
