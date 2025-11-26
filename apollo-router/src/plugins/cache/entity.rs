use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use http::header;
use http::header::CACHE_CONTROL;
use itertools::Itertools;
use multimap::MultiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use serde_json_bytes::from_value;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::RwLock;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;
use tracing::Level;

use super::cache_control::CacheControl;
use super::invalidation::Invalidation;
use super::invalidation::InvalidationOrigin;
use super::invalidation_endpoint::InvalidationEndpointConfig;
use super::invalidation_endpoint::InvalidationService;
use super::invalidation_endpoint::SubgraphInvalidationConfig;
use super::metrics::CacheMetricContextKey;
use super::metrics::CacheMetricsService;
use crate::Context;
use crate::Endpoint;
use crate::ListenAddr;
use crate::batching::BatchQuery;
use crate::cache::storage::ValueType;
use crate::configuration::RedisCache;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::response_cache::plugin::find_matching_key_field_set;
use crate::query_planner::OperationKind;
use crate::redis;
use crate::services::subgraph;
use crate::services::subgraph::SubgraphRequestId;
use crate::services::supergraph;
use crate::spec::QueryHash;
use crate::spec::TYPENAME;

/// Change this key if you introduce a breaking change in entity caching algorithm to make sure it won't take the previous entries
pub(crate) const ENTITY_CACHE_VERSION: &str = "1.1";
pub(crate) const ENTITIES: &str = "_entities";
pub(crate) const REPRESENTATIONS: &str = "representations";
pub(crate) const CONTEXT_CACHE_KEY: &str = "apollo_entity_cache::key";
/// Context key to enable support of surrogate cache key
pub(crate) const CONTEXT_CACHE_KEYS: &str = "apollo::entity_cache::cached_keys_status";

register_plugin!("apollo", "preview_entity_cache", EntityCache);

#[derive(Clone)]
pub(crate) struct EntityCache {
    storage: Arc<Storage>,
    endpoint_config: Option<Arc<InvalidationEndpointConfig>>,
    subgraphs: Arc<SubgraphConfiguration<Subgraph>>,
    entity_type: Option<String>,
    enabled: bool,
    metrics: Metrics,
    expose_keys_in_context: bool,
    private_queries: Arc<RwLock<HashSet<String>>>,
    pub(crate) invalidation: Invalidation,
    supergraph_schema: Arc<Valid<Schema>>,
    /// map containing the enum GRAPH
    subgraph_enums: Arc<HashMap<String, String>>,
}

pub(crate) struct Storage {
    pub(crate) all: Option<redis::Gateway>,
    pub(crate) subgraphs: HashMap<String, redis::Gateway>,
}

impl Storage {
    pub(crate) fn get(&self, subgraph: &str) -> Option<&redis::Gateway> {
        self.subgraphs.get(subgraph).or(self.all.as_ref())
    }
}

/// Configuration for entity caching
#[derive(Clone, Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct Config {
    /// Enable or disable the entity caching feature
    #[serde(default)]
    pub(crate) enabled: bool,

    #[serde(default)]
    /// Expose cache keys in context
    expose_keys_in_context: bool,

    /// Configure invalidation per subgraph
    pub(crate) subgraph: SubgraphConfiguration<Subgraph>,

    /// Global invalidation configuration
    invalidation: Option<InvalidationEndpointConfig>,

    /// Entity caching evaluation metrics
    #[serde(default)]
    metrics: Metrics,
}

/// Per subgraph configuration for entity caching
#[derive(Clone, Debug, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct Subgraph {
    /// Redis configuration
    pub(crate) redis: Option<RedisCache>,

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

/// Per subgraph configuration for entity caching
#[derive(Clone, Debug, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct Ttl(
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(crate) Duration,
);

/// Per subgraph configuration for entity caching
#[derive(Clone, Debug, Default, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Metrics {
    /// enables metrics evaluating the benefits of entity caching
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Metrics counter TTL
    pub(crate) ttl: Option<Ttl>,
    /// Adds the entity type name to attributes. This can greatly increase the cardinality
    #[serde(default)]
    pub(crate) separate_per_type: bool,
}

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
impl Plugin for EntityCache {
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

        if let Some(redis) = &init.config.subgraph.all.redis {
            let mut redis_config = redis.clone();
            let required_to_start = redis_config.required_to_start;
            // we need to explicitly disable TTL reset because it is managed directly by this plugin
            redis_config.reset_ttl = false;
            all = match redis::Gateway::new(redis_config, "entity").await {
                Ok(storage) => Some(storage),
                Err(e) => {
                    tracing::error!(
                        cache = "entity",
                        e,
                        "could not open connection to Redis for caching",
                    );
                    if required_to_start {
                        return Err(e);
                    }
                    None
                }
            };
        }
        let mut subgraph_storages = HashMap::new();
        for (subgraph, config) in &init.config.subgraph.subgraphs {
            if let Some(redis) = &config.redis {
                let required_to_start = redis.required_to_start;
                // we need to explicitly disable TTL reset because it is managed directly by this plugin
                let mut redis_config = redis.clone();
                redis_config.reset_ttl = false;
                let storage = match redis::Gateway::new(redis_config, "entity").await {
                    Ok(storage) => Some(storage),
                    Err(e) => {
                        tracing::error!(
                            cache = "entity",
                            e,
                            "could not open connection to Redis for caching",
                        );
                        if required_to_start {
                            return Err(e);
                        }
                        None
                    }
                };
                if let Some(storage) = storage {
                    subgraph_storages.insert(subgraph.clone(), storage);
                }
            }
        }

        if init
            .config
            .subgraph
            .all
            .redis
            .as_ref()
            .map(|r| r.ttl.is_none())
            .unwrap_or(false)
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

        let storage = Arc::new(Storage {
            all,
            subgraphs: subgraph_storages,
        });

        let invalidation = Invalidation::new(
            storage.clone(),
            init.config
                .invalidation
                .as_ref()
                .map(|i| i.scan_count)
                .unwrap_or(1000),
            init.config
                .invalidation
                .as_ref()
                .map(|i| i.concurrent_requests)
                .unwrap_or(10),
        )
        .await?;

        Ok(Self {
            storage,
            entity_type,
            enabled: init.config.enabled,
            expose_keys_in_context: init.config.expose_keys_in_context,
            endpoint_config: init.config.invalidation.clone().map(Arc::new),
            subgraphs: Arc::new(init.config.subgraph),
            metrics: init.config.metrics,
            private_queries: Arc::new(RwLock::new(HashSet::new())),
            invalidation,
            subgraph_enums: Arc::new(get_subgraph_enums(&init.supergraph_schema)),
            supergraph_schema: init.supergraph_schema,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        ServiceBuilder::new()
            .map_response(|mut response: supergraph::Response| {
                if let Some(cache_control) = response
                    .context
                    .extensions()
                    .with_lock(|lock| lock.get::<CacheControl>().cloned())
                {
                    let _ = cache_control.to_headers(response.response.headers_mut());
                }

                response
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(
        &self,
        name: &str,
        mut service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        let storage = match self.storage.get(name) {
            Some(storage) => storage.clone(),
            None => {
                return ServiceBuilder::new()
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
                    .boxed();
            }
        };

        let subgraph_ttl = self.subgraph_ttl(name, &storage);
        let subgraph_enabled = self.subgraph_enabled(name);
        let private_id = self.subgraphs.get(name).private_id.clone();

        let name = name.to_string();

        if self.metrics.enabled {
            service = CacheMetricsService::create(
                name.to_string(),
                service,
                self.metrics.ttl.as_ref(),
                self.metrics.separate_per_type,
            );
        }

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
                    storage,
                    subgraph_ttl,
                    private_queries,
                    private_id,
                    invalidation: self.invalidation.clone(),
                    expose_keys_in_context: self.expose_keys_in_context,
                    supergraph_schema: self.supergraph_schema.clone(),
                    subgraph_enums: self.subgraph_enums.clone(),
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
                        "Entity caching invalidation endpoint listening on: {}{}",
                        endpoint_config.listen,
                        endpoint_config.path
                    );
                    map.insert(endpoint_config.listen.clone(), endpoint);
                }
                None => {
                    tracing::warn!(
                        "Cannot start entity caching invalidation endpoint because the listen address and endpoint is not configured"
                    );
                }
            }
        }

        map
    }
}

#[cfg(test)]
pub(super) const INVALIDATION_SHARED_KEY: &str = "supersecret";
impl EntityCache {
    #[cfg(test)]
    pub(crate) async fn with_mocks(
        storage: redis::Gateway,
        subgraphs: HashMap<String, Subgraph>,
        supergraph_schema: Arc<Valid<Schema>>,
    ) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        use std::net::IpAddr;
        use std::net::Ipv4Addr;
        use std::net::SocketAddr;

        let storage = Arc::new(Storage {
            all: Some(storage),
            subgraphs: HashMap::new(),
        });
        let invalidation = Invalidation::new(storage.clone(), 1000, 10).await?;

        Ok(Self {
            storage,
            entity_type: None,
            enabled: true,
            expose_keys_in_context: true,
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
            metrics: Metrics::default(),
            private_queries: Default::default(),
            endpoint_config: Some(Arc::new(InvalidationEndpointConfig {
                path: String::from("/invalidation"),
                listen: ListenAddr::SocketAddr(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                    4000,
                )),
                scan_count: 1000,
                concurrent_requests: 10,
            })),
            invalidation,
            subgraph_enums: Arc::new(get_subgraph_enums(&supergraph_schema)),
            supergraph_schema,
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
    fn subgraph_ttl(&self, subgraph_name: &str, storage: &redis::Gateway) -> Option<Duration> {
        self.subgraphs
            .get(subgraph_name)
            .ttl
            .clone()
            .map(|t| t.0)
            .or_else(|| match self.subgraphs.all.ttl.clone() {
                Some(ttl) => Some(ttl.0),
                None => storage.ttl(),
            })
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
    storage: redis::Gateway,
    subgraph_ttl: Option<Duration>,
    private_queries: Arc<RwLock<HashSet<String>>>,
    private_id: Option<String>,
    expose_keys_in_context: bool,
    invalidation: Invalidation,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: Arc<HashMap<String, String>>,
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
        // Check if the request is part of a batch. If it is, completely bypass entity caching since it
        // will break any request batches which this request is part of.
        // This check is what enables Batching and entity caching to work together, so be very careful
        // before making any changes to it.
        if request
            .context
            .extensions()
            .with_lock(|lock| lock.contains_key::<BatchQuery>())
        {
            return self.service.call(request).await;
        }
        let query = request
            .subgraph_request
            .body()
            .query
            .clone()
            .unwrap_or_default();

        let is_known_private = { self.private_queries.read().await.contains(&query) };
        let private_id = self.get_private_id(&request.context);

        // the response will have a private scope but we don't have a way to differentiate users, so we know we will not get or store anything in the cache
        if is_known_private && private_id.is_none() {
            return self.service.call(request).await;
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
                    self.storage.clone(),
                    is_known_private,
                    private_id.as_deref(),
                    self.expose_keys_in_context,
                    request,
                )
                .instrument(tracing::info_span!("cache.entity.lookup"))
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
                    ControlFlow::Continue((request, mut root_cache_key)) => {
                        cache_hit.insert("Query".to_string(), CacheHitMiss { hit: 0, miss: 1 });
                        let _ = request.context.insert(
                            CacheMetricContextKey::new(request.subgraph_name.clone()),
                            CacheSubgraph(cache_hit),
                        );

                        let mut response = self.service.call(request).await?;
                        let cache_control =
                            if response.response.headers().contains_key(CACHE_CONTROL) {
                                CacheControl::new(response.response.headers(), self.storage.ttl)?
                            } else {
                                CacheControl::no_store()
                            };

                        if cache_control.private() {
                            // we did not know in advance that this was a query with a private scope, so we update the cache key
                            if !is_known_private {
                                self.private_queries.write().await.insert(query.to_string());

                                if let Some(s) = private_id.as_ref() {
                                    root_cache_key = format!("{root_cache_key}:{s}");
                                }
                            }

                            if private_id.is_none() {
                                // the response has a private scope but we don't have a way to differentiate users, so we do not store the response in cache
                                // We don't need to fill the context with this cache key as it will never be cached
                                return Ok(response);
                            }
                        }

                        if let Some(invalidation_extensions) = response
                            .response
                            .body_mut()
                            .extensions
                            .remove("invalidation")
                        {
                            self.handle_invalidation(
                                InvalidationOrigin::Extensions,
                                invalidation_extensions,
                            )
                            .await;
                        }

                        if cache_control.should_store() {
                            cache_store_root_from_response(
                                self.storage,
                                self.subgraph_ttl,
                                &response,
                                cache_control,
                                root_cache_key,
                                self.expose_keys_in_context,
                            )
                            .await?;
                        }

                        Ok(response)
                    }
                }
            } else {
                let mut response = self.service.call(request).await?;
                if let Some(invalidation_extensions) = response
                    .response
                    .body_mut()
                    .extensions
                    .remove("invalidation")
                {
                    self.handle_invalidation(
                        InvalidationOrigin::Extensions,
                        invalidation_extensions,
                    )
                    .await;
                }

                Ok(response)
            }
        } else {
            let request_id = request.id.clone();
            match cache_lookup_entities(
                self.name.clone(),
                self.supergraph_schema.clone(),
                &self.subgraph_enums,
                self.storage.clone(),
                is_known_private,
                private_id.as_deref(),
                request,
                self.expose_keys_in_context,
            )
            .instrument(tracing::info_span!("cache.entity.lookup"))
            .await?
            {
                ControlFlow::Break(response) => Ok(response),
                ControlFlow::Continue((request, mut cache_result)) => {
                    let context = request.context.clone();
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
                            if self.expose_keys_in_context {
                                // Update cache keys needed for surrogate cache key because new data has not been fetched
                                context.upsert::<_, CacheKeysContext>(
                                    CONTEXT_CACHE_KEYS,
                                    |mut value| {
                                        if let Some(cache_keys) = value.get_mut(&request_id) {
                                            cache_keys.retain(|cache_key| {
                                                matches!(cache_key.status, CacheKeyStatus::Cached)
                                            });
                                        }
                                        value
                                    },
                                )?;
                            }

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

                    let mut cache_control =
                        if response.response.headers().contains_key(CACHE_CONTROL) {
                            CacheControl::new(response.response.headers(), self.storage.ttl)?
                        } else {
                            CacheControl::no_store()
                        };

                    if let Some(control_from_cached) = cache_result.1 {
                        cache_control = cache_control.merge(&control_from_cached);
                    }
                    if self.expose_keys_in_context {
                        // Update cache keys needed for surrogate cache key when it's new data and not data from the cache
                        let response_id = response.id.clone();
                        let cache_control_str = cache_control.to_cache_control_header()?;
                        response.context.upsert::<_, CacheKeysContext>(
                            CONTEXT_CACHE_KEYS,
                            |mut value| {
                                if let Some(cache_keys) = value.get_mut(&response_id) {
                                    for cache_key in cache_keys
                                        .iter_mut()
                                        .filter(|c| matches!(c.status, CacheKeyStatus::New))
                                    {
                                        cache_key.cache_control = cache_control_str.clone();
                                    }
                                }
                                value
                            },
                        )?;
                    }

                    if !is_known_private && cache_control.private() {
                        self.private_queries.write().await.insert(query.to_string());
                    }

                    if let Some(invalidation_extensions) = response
                        .response
                        .body_mut()
                        .extensions
                        .remove("invalidation")
                    {
                        self.handle_invalidation(
                            InvalidationOrigin::Extensions,
                            invalidation_extensions,
                        )
                        .await;
                    }

                    cache_store_entities_from_response(
                        self.storage,
                        self.subgraph_ttl,
                        &mut response,
                        cache_control.clone(),
                        cache_result.0,
                        is_known_private,
                        private_id,
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
                    let mut digest = Sha256::new();
                    digest.update(s);
                    hex::encode(digest.finalize().as_slice())
                })
            })
        })
    }

    async fn handle_invalidation(
        &mut self,
        origin: InvalidationOrigin,
        invalidation_extensions: Value,
    ) {
        if let Ok(requests) = from_value(invalidation_extensions)
            && let Err(e) = self.invalidation.invalidate(origin, requests).await
        {
            tracing::error!(error = %e,
               message = "could not invalidate entity cache entries",
            );
        }
    }
}

async fn cache_lookup_root(
    name: String,
    entity_type_opt: Option<&str>,
    cache: redis::Gateway,
    is_known_private: bool,
    private_id: Option<&str>,
    expose_keys_in_context: bool,
    mut request: subgraph::Request,
) -> Result<ControlFlow<subgraph::Response, (subgraph::Request, String)>, BoxError> {
    let body = request.subgraph_request.body_mut();

    let key = extract_cache_key_root(
        &name,
        entity_type_opt,
        &request.query_hash,
        body,
        &request.context,
        &request.authorization,
        is_known_private,
        private_id,
    );

    let cache_result: Result<Option<CacheEntry>, _> = cache.get(key.clone()).await;

    match cache_result {
        Ok(Some(value)) => {
            if value.control.can_use() {
                let control = value.control.clone();
                update_cache_control(&request.context, &control);
                if expose_keys_in_context {
                    let request_id = request.id.clone();
                    let cache_control_header = value.control.to_cache_control_header()?;
                    request.context.upsert::<_, CacheKeysContext>(
                        CONTEXT_CACHE_KEYS,
                        |mut val| {
                            match val.get_mut(&request_id) {
                                Some(v) => {
                                    v.push(CacheKeyContext {
                                        key: key.clone(),
                                        status: CacheKeyStatus::Cached,
                                        cache_control: cache_control_header,
                                    });
                                }
                                None => {
                                    val.insert(
                                        request_id,
                                        vec![CacheKeyContext {
                                            key: key.clone(),
                                            status: CacheKeyStatus::Cached,
                                            cache_control: cache_control_header,
                                        }],
                                    );
                                }
                            }

                            val
                        },
                    )?;
                }

                let mut response = subgraph::Response::builder()
                    .data(value.data)
                    .extensions(Object::new())
                    .context(request.context)
                    .subgraph_name(request.subgraph_name.clone())
                    .build();

                value.control.to_headers(response.response.headers_mut())?;
                Ok(ControlFlow::Break(response))
            } else {
                Ok(ControlFlow::Continue((request, key)))
            }
        }
        Ok(None) | Err(_) => Ok(ControlFlow::Continue((request, key))),
    }
}

struct EntityCacheResults(Vec<IntermediateResult>, Option<CacheControl>);

#[allow(clippy::too_many_arguments)]
async fn cache_lookup_entities(
    name: String,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
    cache: redis::Gateway,
    is_known_private: bool,
    private_id: Option<&str>,
    mut request: subgraph::Request,
    expose_keys_in_context: bool,
) -> Result<ControlFlow<subgraph::Response, (subgraph::Request, EntityCacheResults)>, BoxError> {
    let body = request.subgraph_request.body_mut();
    let keys = extract_cache_keys(
        &name,
        supergraph_schema,
        subgraph_enums,
        &request.query_hash,
        body,
        &request.context,
        &request.authorization,
        is_known_private,
        private_id,
    )?;

    let cache_result: Vec<Option<CacheEntry>> = cache
        .get_multiple(keys.clone())
        .await
        .into_iter()
        .map(|v: Option<CacheEntry>| match v {
            None => None,
            Some(v) => {
                if v.control.can_use() {
                    Some(v)
                } else {
                    None
                }
            }
        })
        .collect();

    let representations = body
        .variables
        .get_mut(REPRESENTATIONS)
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");
    // remove from representations the entities we already obtained from the cache
    let (new_representations, cache_result, cache_control) =
        filter_representations(&name, representations, keys, cache_result, &request.context)?;

    if expose_keys_in_context {
        let mut cache_entries = Vec::with_capacity(cache_result.len());
        for intermediate_result in &cache_result {
            match &intermediate_result.cache_entry {
                Some(cache_entry) => {
                    cache_entries.push(CacheKeyContext {
                        key: intermediate_result.key.clone(),
                        status: CacheKeyStatus::Cached,
                        cache_control: cache_entry.control.to_cache_control_header()?,
                    });
                }
                None => {
                    cache_entries.push(CacheKeyContext {
                        key: intermediate_result.key.clone(),
                        status: CacheKeyStatus::New,
                        cache_control: match &cache_control {
                            Some(cc) => cc.to_cache_control_header()?,
                            None => CacheControl::default().to_cache_control_header()?,
                        },
                    });
                }
            }
        }
        let request_id = request.id.clone();
        request
            .context
            .upsert::<_, CacheKeysContext>(CONTEXT_CACHE_KEYS, |mut v| {
                match v.get_mut(&request_id) {
                    Some(cache_keys) => {
                        cache_keys.append(&mut cache_entries);
                    }
                    None => {
                        v.insert(request_id, cache_entries);
                    }
                }

                v
            })?;
    }

    if !new_representations.is_empty() {
        body.variables
            .insert(REPRESENTATIONS, new_representations.into());

        Ok(ControlFlow::Continue((
            request,
            EntityCacheResults(cache_result, cache_control),
        )))
    } else {
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
            lock.insert(cache_control.merge(cache_control));
        }
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntry {
    control: CacheControl,
    data: Value,
}

impl redis::ValueType for CacheEntry {}

impl ValueType for CacheEntry {
    fn estimated_size(&self) -> Option<usize> {
        None
    }
}

async fn cache_store_root_from_response(
    cache: redis::Gateway,
    subgraph_ttl: Option<Duration>,
    response: &subgraph::Response,
    cache_control: CacheControl,
    cache_key: String,
    expose_keys_in_context: bool,
) -> Result<(), BoxError> {
    if let Some(data) = response.response.body().data.as_ref() {
        let ttl: Option<Duration> = cache_control
            .ttl()
            .map(Duration::from_secs)
            .or(subgraph_ttl);

        if response.response.body().errors.is_empty() && cache_control.should_store() {
            let span = tracing::info_span!("cache.entity.store");
            let data = data.clone();
            if expose_keys_in_context {
                let response_id = response.id.clone();
                let cache_control_header = cache_control.to_cache_control_header()?;

                response
                    .context
                    .upsert::<_, CacheKeysContext>(CONTEXT_CACHE_KEYS, |mut val| {
                        match val.get_mut(&response_id) {
                            Some(v) => {
                                v.push(CacheKeyContext {
                                    key: cache_key.clone(),
                                    status: CacheKeyStatus::New,
                                    cache_control: cache_control_header,
                                });
                            }
                            None => {
                                val.insert(
                                    response_id,
                                    vec![CacheKeyContext {
                                        key: cache_key.clone(),
                                        status: CacheKeyStatus::New,
                                        cache_control: cache_control_header,
                                    }],
                                );
                            }
                        }

                        val
                    })?;
            }

            tokio::spawn(async move {
                cache
                    .insert(
                        cache_key,
                        CacheEntry {
                            control: cache_control,
                            data,
                        },
                        ttl,
                    )
                    .instrument(span)
                    .await;
            });
        }
    }

    Ok(())
}

async fn cache_store_entities_from_response(
    cache: redis::Gateway,
    subgraph_ttl: Option<Duration>,
    response: &mut subgraph::Response,
    cache_control: CacheControl,
    mut result_from_cache: Vec<IntermediateResult>,
    is_known_private: bool,
    private_id: Option<String>,
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

        let (new_entities, new_errors) = insert_entities_in_result(
            entities
                .as_array_mut()
                .ok_or_else(|| FetchError::MalformedResponse {
                    reason: "expected an array of entities".to_string(),
                })?,
            &response.response.body().errors,
            cache,
            subgraph_ttl,
            cache_control,
            &mut result_from_cache,
            update_key_private,
            should_cache_private,
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

pub(crate) fn hash_vary_headers(headers: &http::HeaderMap) -> String {
    let mut digest = Sha256::new();

    for vary_header_value in headers.get_all(header::VARY).into_iter() {
        if vary_header_value == "*" {
            return String::from("*");
        } else {
            let header_names = match vary_header_value.to_str() {
                Ok(header_val) => header_val.split(", "),
                Err(_) => continue,
            };
            header_names.for_each(|header_name| {
                if let Some(header_value) = headers.get(header_name).and_then(|h| h.to_str().ok()) {
                    digest.update(header_value);
                    digest.update(&[0u8; 1][..]);
                }
            });
        }
    }

    hex::encode(digest.finalize().as_slice())
}

// XXX(@goto-bus-stop): this doesn't make much sense: QueryHash already includes the operation name.
// This function can be removed outright later at the cost of invalidating all entity caches.
pub(crate) fn hash_query(query_hash: &QueryHash, body: &graphql::Request) -> String {
    let mut digest = Sha256::new();
    digest.update(query_hash.as_bytes());
    digest.update(&[0u8; 1][..]);
    digest.update(body.operation_name.as_deref().unwrap_or("-").as_bytes());
    digest.update(&[0u8; 1][..]);

    hex::encode(digest.finalize().as_slice())
}

pub(crate) fn hash_additional_data(
    body: &mut graphql::Request,
    context: &Context,
    cache_key: &CacheKeyMetadata,
) -> String {
    let mut digest = Sha256::new();

    let repr_key = ByteString::from(REPRESENTATIONS);
    // Removing the representations variable because it's already part of the cache key
    let representations = body.variables.remove(&repr_key);
    body.variables.sort_keys();
    digest.update(serde_json::to_vec(&body.variables).unwrap());
    if let Some(representations) = representations {
        body.variables.insert(repr_key, representations);
    }

    digest.update(serde_json::to_vec(cache_key).unwrap());

    if let Ok(Some(cache_data)) = context.get::<&str, Object>(CONTEXT_CACHE_KEY) {
        if let Some(v) = cache_data.get("all") {
            digest.update(serde_json::to_vec(v).unwrap())
        }
        if let Some(v) = body
            .operation_name
            .as_ref()
            .and_then(|op| cache_data.get(op.as_str()))
        {
            digest.update(serde_json::to_vec(v).unwrap())
        }
    }

    hex::encode(digest.finalize().as_slice())
}

// build a cache key for the root operation
#[allow(clippy::too_many_arguments)]
fn extract_cache_key_root(
    subgraph_name: &str,
    entity_type_opt: Option<&str>,
    query_hash: &QueryHash,
    body: &mut graphql::Request,
    context: &Context,
    cache_key: &CacheKeyMetadata,
    is_known_private: bool,
    private_id: Option<&str>,
) -> String {
    // hash the query and operation name
    let query_hash = hash_query(query_hash, body);
    // hash more data like variables and authorization status
    let additional_data_hash = hash_additional_data(body, context, cache_key);

    let entity_type = entity_type_opt.unwrap_or("Query");

    // the cache key is written to easily find keys matching a prefix for deletion:
    // - entity cache version: current version of the hash
    // - subgraph name: subgraph name
    // - entity type: entity type
    // - query hash: invalidate the entry for a specific query and operation name
    // - additional data: separate cache entries depending on info like authorization status
    let mut key = String::new();
    let _ = write!(
        &mut key,
        "version:{ENTITY_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}:hash:{query_hash}:data:{additional_data_hash}"
    );

    if is_known_private && let Some(id) = private_id {
        let _ = write!(&mut key, ":{id}");
    }
    key
}

// build a list of keys to get from the cache in one query
#[allow(clippy::too_many_arguments)]
fn extract_cache_keys(
    subgraph_name: &str,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
    query_hash: &QueryHash,
    body: &mut graphql::Request,
    context: &Context,
    cache_key: &CacheKeyMetadata,
    is_known_private: bool,
    private_id: Option<&str>,
) -> Result<Vec<String>, BoxError> {
    // hash the query and operation name
    let query_hash = hash_query(query_hash, body);
    // hash more data like variables and authorization status
    let additional_data_hash = hash_additional_data(body, context, cache_key);

    let representations = body
        .variables
        .get_mut(REPRESENTATIONS)
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");

    // Get entity key to only get the right fields in representations

    let mut res = Vec::new();
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

        let key_field_set = find_matching_key_field_set(
            representation,
            typename,
            subgraph_name,
            &supergraph_schema,
            subgraph_enums,
        )?;

        let hashed_representation = if representation.is_empty() {
            String::new()
        } else {
            hash_representation(representation)
        };
        let hashed_entity_key = hash_entity_key(representation, &key_field_set);

        // the cache key is written to easily find keys matching a prefix for deletion:
        // - entity cache version: current version of the hash
        // - subgraph name: caching is done per subgraph
        // - type: can invalidate all instances of a type
        // - entity key: invalidate a specific entity
        // - query hash: invalidate the entry for a specific query and operation name
        // - additional data: separate cache entries depending on info like authorization status
        let mut key = format!(
            "version:{ENTITY_CACHE_VERSION}:subgraph:{subgraph_name}:type:{typename}:entity:{hashed_entity_key}:representation:{hashed_representation}:hash:{query_hash}:data:{additional_data_hash}"
        );
        if is_known_private && let Some(id) = private_id {
            let _ = write!(&mut key, ":{id}");
        }

        // Restore the `representation` back whole again
        representation.insert(TYPENAME, typename_value);

        res.push(key);
    }
    Ok(res)
}

// Order-insensitive structural hash of the representation value
// * representation: key fields & other required fields
// * selection_set: key fields selection set (if any)
// * if selection_set is provided, only fields present in the selection set are hashed.
// Note: This function mirrors `get_entity_key_from_selection_set` in response_cache/plugin.rs.
fn hash_representation_inner(
    representation: &serde_json_bytes::Map<ByteString, Value>,
    selection_set: Option<&apollo_compiler::executable::SelectionSet>,
) -> String {
    fn hash_object(
        state: &mut Sha256,
        fields: &serde_json_bytes::Map<ByteString, Value>,
        selection_set: Option<&apollo_compiler::executable::SelectionSet>,
    ) {
        state.update("{".as_bytes());
        let default_document = Default::default();
        let sorted_selections = if let Some(selection_set) = selection_set {
            // Select only fields present in the selection set
            selection_set
                .root_fields(&default_document)
                .filter_map(|f| {
                    let key = f.name.as_str();
                    let val = fields.get(key)?;
                    Some((key, val, Some(&f.selection_set)))
                })
                .sorted_by(|a, b| a.0.cmp(b.0))
        } else {
            // Select all fields
            fields
                .iter()
                .map(|(k, v)| (k.as_str(), v, None))
                .sorted_by(|a, b| a.0.cmp(b.0))
        };
        for (key, val, selection_set) in sorted_selections {
            state.update(serde_json::to_string(key).unwrap().as_bytes());
            state.update(":".as_bytes());
            match val {
                serde_json_bytes::Value::Object(obj) => {
                    hash_object(state, obj, selection_set);
                }
                Value::Array(arr) => {
                    hash_array(state, arr, selection_set);
                }
                // scalar value
                _ => state.update(serde_json::to_string(val).unwrap().as_bytes()),
            }
            state.update(",".as_bytes());
        }
        state.update("}".as_bytes());
    }

    fn hash_array(
        state: &mut Sha256,
        items: &[Value],
        selection_set: Option<&apollo_compiler::executable::SelectionSet>,
    ) {
        state.update("[".as_bytes());
        items.iter().for_each(|v| {
            match v {
                serde_json_bytes::Value::Object(obj) => {
                    hash_object(state, obj, selection_set);
                }
                Value::Array(arr) => {
                    hash_array(state, arr, selection_set);
                }
                // scalar value
                _ => state.update(serde_json::to_string(v).unwrap().as_bytes()),
            }
            state.update(",".as_bytes());
        });
        state.update("]".as_bytes());
    }

    let mut digest = Sha256::new();
    hash_object(&mut digest, representation, selection_set);
    hex::encode(digest.finalize().as_slice())
}

// Hash the whole representation
pub(crate) fn hash_representation(
    representation: &serde_json_bytes::Map<ByteString, Value>,
) -> String {
    hash_representation_inner(representation, None)
}

// Hash only the entity key part of representation
fn hash_entity_key(
    representation: &serde_json_bytes::Map<ByteString, Value>,
    key_field_set: &apollo_compiler::executable::SelectionSet,
) -> String {
    hash_representation_inner(representation, Some(key_field_set))
}

/// represents the result of a cache lookup for an entity type and key
struct IntermediateResult {
    key: String,
    typename: String,
    cache_entry: Option<CacheEntry>,
}

// build a new list of representations without the ones we got from the cache
#[allow(clippy::type_complexity)]
fn filter_representations(
    subgraph_name: &str,
    representations: &mut Vec<Value>,
    keys: Vec<String>,
    mut cache_result: Vec<Option<CacheEntry>>,
    context: &Context,
) -> Result<(Vec<Value>, Vec<IntermediateResult>, Option<CacheControl>), BoxError> {
    let mut new_representations: Vec<Value> = Vec::new();
    let mut result = Vec::new();
    let mut cache_hit: HashMap<String, CacheHitMiss> = HashMap::new();
    let mut cache_control = None;

    for ((mut representation, key), mut cache_entry) in representations
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
            typename,
            cache_entry,
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
    cache: redis::Gateway,
    subgraph_ttl: Option<Duration>,
    cache_control: CacheControl,
    result: &mut Vec<IntermediateResult>,
    update_key_private: Option<String>,
    should_cache_private: bool,
) -> Result<(Vec<Value>, Vec<Error>), BoxError> {
    let ttl: Option<Duration> = cache_control
        .ttl()
        .map(Duration::from_secs)
        .or(subgraph_ttl);

    let mut new_entities = Vec::new();
    let mut new_errors = Vec::new();

    let mut inserted_types: HashMap<String, usize> = HashMap::new();
    let mut to_insert: Vec<_> = Vec::new();
    let mut entities_it = entities.drain(..).enumerate();

    // insert requested entities and cached entities in the same order as
    // they were requested
    for (
        new_entity_idx,
        IntermediateResult {
            mut key,
            typename,
            cache_entry,
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

                *inserted_types.entry(typename).or_default() += 1;

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

                if !has_errors && cache_control.should_store() && should_cache_private {
                    to_insert.push((
                        key,
                        CacheEntry {
                            control: cache_control.clone(),
                            data: value.clone(),
                        },
                    ));
                }

                new_entities.push(value);
            }
        }
    }

    if !to_insert.is_empty() {
        let span = tracing::info_span!("cache_store");

        tokio::spawn(async move {
            cache
                .insert_multiple(&to_insert, ttl)
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

pub(crate) type CacheKeysContext = HashMap<SubgraphRequestId, Vec<CacheKeyContext>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq, Hash, PartialOrd, Ord))]
pub(crate) struct CacheKeyContext {
    pub(super) key: String,
    pub(super) status: CacheKeyStatus,
    pub(super) cache_control: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq, Hash))]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheKeyStatus {
    /// New cache key inserted in the cache
    New,
    /// Key that was already in the cache
    Cached,
}

#[cfg(test)]
impl PartialOrd for CacheKeyStatus {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
impl Ord for CacheKeyStatus {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (CacheKeyStatus::New, CacheKeyStatus::New) => std::cmp::Ordering::Equal,
            (CacheKeyStatus::New, CacheKeyStatus::Cached) => std::cmp::Ordering::Greater,
            (CacheKeyStatus::Cached, CacheKeyStatus::New) => std::cmp::Ordering::Less,
            (CacheKeyStatus::Cached, CacheKeyStatus::Cached) => std::cmp::Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::parser::Parser;
    use serde_json_bytes::json;

    use super::*;
    use crate::plugins::cache::tests::MockStore;
    use crate::plugins::cache::tests::SCHEMA;
    use crate::plugins::response_cache::plugin::matches_selection_set;

    #[tokio::test]
    async fn test_subgraph_enabled() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let redis_cache = redis::Gateway::from_mocks(Arc::new(MockStore::new()))
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

        let mut entity_cache = EntityCache::with_mocks(
            redis_cache.clone(),
            serde_json::from_value(map).unwrap(),
            valid_schema.clone(),
        )
        .await
        .unwrap();

        assert!(entity_cache.subgraph_enabled("user"));
        assert!(!entity_cache.subgraph_enabled("archive"));
        let subgraph_config = serde_json::json!({
            "all": {
                "enabled": false
            },
            "subgraphs": entity_cache.subgraphs.subgraphs.clone()
        });
        entity_cache.subgraphs = Arc::new(serde_json::from_value(subgraph_config).unwrap());
        assert!(!entity_cache.subgraph_enabled("archive"));
        assert!(entity_cache.subgraph_enabled("user"));
        assert!(entity_cache.subgraph_enabled("orga"));
    }

    #[tokio::test]
    async fn test_subgraph_ttl() {
        let valid_schema = Arc::new(Schema::parse_and_validate(SCHEMA, "test.graphql").unwrap());
        let mut redis_cache = redis::Gateway::from_mocks(Arc::new(MockStore::new()))
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

        let mut entity_cache = EntityCache::with_mocks(
            redis_cache.clone(),
            serde_json::from_value(map).unwrap(),
            valid_schema.clone(),
        )
        .await
        .unwrap();

        assert_eq!(
            entity_cache.subgraph_ttl("user", &redis_cache),
            Some(Duration::from_secs(2))
        );
        assert!(entity_cache.subgraph_ttl("orga", &redis_cache).is_none());
        assert_eq!(
            entity_cache.subgraph_ttl("archive", &redis_cache),
            Some(Duration::from_millis(5000))
        );
        // update global storage TTL
        redis_cache.ttl = Some(Duration::from_secs(25));
        assert_eq!(
            entity_cache.subgraph_ttl("user", &redis_cache),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            entity_cache.subgraph_ttl("orga", &redis_cache),
            Some(Duration::from_secs(25))
        );
        assert_eq!(
            entity_cache.subgraph_ttl("archive", &redis_cache),
            Some(Duration::from_millis(5000))
        );
        entity_cache.subgraphs = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: Some(Ttl(Duration::from_secs(42))),
                ..Default::default()
            },
            subgraphs: entity_cache.subgraphs.subgraphs.clone(),
        });
        assert_eq!(
            entity_cache.subgraph_ttl("user", &redis_cache),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            entity_cache.subgraph_ttl("orga", &redis_cache),
            Some(Duration::from_secs(42))
        );
        assert_eq!(
            entity_cache.subgraph_ttl("archive", &redis_cache),
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
        let hash = hash_entity_key(&representation, &field_set.selection_set);
        assert_eq!(
            hash,
            "e5faa4c491214ed07f53acc65189e6efacc8b7eedc0d88055d86a5307671f0e3"
        );
    }
}
