use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use http::header;
use http::header::CACHE_CONTROL;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
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
use super::metrics::CacheMetricsService;
use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::cache::redis::RedisValue;
use crate::configuration::RedisCache;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::query_planner::fetch::QueryHash;
use crate::query_planner::OperationKind;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::TYPENAME;
use crate::Context;

pub(crate) const ENTITIES: &str = "_entities";
pub(crate) const REPRESENTATIONS: &str = "representations";
pub(crate) const CONTEXT_CACHE_KEY: &str = "apollo_entity_cache::key";

register_plugin!("apollo", "preview_entity_cache", EntityCache);

#[derive(Clone)]
pub(crate) struct EntityCache {
    storage: Option<RedisCacheStorage>,
    subgraphs: Arc<HashMap<String, Subgraph>>,
    enabled: Option<bool>,
    metrics: Metrics,
    private_queries: Arc<RwLock<HashSet<String>>>,
}

/// Configuration for entity caching
#[derive(Clone, Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct Config {
    redis: RedisCache,
    /// activates caching for all subgraphs, unless overriden in subgraph specific configuration
    #[serde(default)]
    enabled: Option<bool>,
    /// Per subgraph configuration
    #[serde(default)]
    subgraphs: HashMap<String, Subgraph>,

    /// Entity caching evaluation metrics
    #[serde(default)]
    metrics: Metrics,
}

/// Per subgraph configuration for entity caching
#[derive(Clone, Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct Subgraph {
    /// expiration for all keys for this subgraph, unless overriden by the `Cache-Control` header in subgraph responses
    #[serde(default)]
    pub(crate) ttl: Option<Ttl>,

    /// activates caching for this subgraph, overrides the global configuration
    #[serde(default)]
    pub(crate) enabled: Option<bool>,

    /// Context key used to separate cache sections per user
    #[serde(default)]
    pub(crate) private_id: Option<String>,
}

/// Per subgraph configuration for entity caching
#[derive(Clone, Debug, JsonSchema, Deserialize)]
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

#[async_trait::async_trait]
impl Plugin for EntityCache {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        let required_to_start = init.config.redis.required_to_start;
        // we need to explicitely disable TTL reset because it is managed directly by this plugin
        let mut redis_config = init.config.redis.clone();
        redis_config.reset_ttl = false;
        let storage = match RedisCacheStorage::new(redis_config).await {
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

        if init.config.redis.ttl.is_none()
            && init.config.subgraphs.values().any(|s| s.ttl.is_none())
        {
            return Err("a TTL must be configured for all subgraphs or globally"
                .to_string()
                .into());
        }

        Ok(Self {
            storage,
            enabled: init.config.enabled,
            subgraphs: Arc::new(init.config.subgraphs),
            metrics: init.config.metrics,
            private_queries: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        ServiceBuilder::new()
            .map_response(|mut response: supergraph::Response| {
                if let Some(cache_control) = {
                    let lock = response.context.extensions().lock();
                    let cache_control = lock.get::<CacheControl>().cloned();
                    cache_control
                } {
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
        let storage = match self.storage.clone() {
            Some(storage) => storage,
            None => return service,
        };

        let (subgraph_ttl, subgraph_enabled, private_id) =
            if let Some(config) = self.subgraphs.get(name) {
                (
                    config.ttl.clone().map(|t| t.0).or_else(|| storage.ttl()),
                    config.enabled.or(self.enabled).unwrap_or(false),
                    config.private_id.clone(),
                )
            } else {
                (storage.ttl(), self.enabled.unwrap_or(false), None)
            };
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
            tower::util::BoxService::new(CacheService(Some(InnerCacheService {
                service,
                name: name.to_string(),
                storage,
                subgraph_ttl,
                private_queries,
                private_id,
            })))
        } else {
            service
        }
    }
}

impl EntityCache {
    #[cfg(test)]
    pub(crate) async fn with_mocks(
        storage: RedisCacheStorage,
        subgraphs: HashMap<String, Subgraph>,
    ) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Ok(Self {
            storage: Some(storage),
            enabled: Some(true),
            subgraphs: Arc::new(subgraphs),
            metrics: Metrics::default(),
            private_queries: Default::default(),
        })
    }
}

struct CacheService(Option<InnerCacheService>);
struct InnerCacheService {
    service: subgraph::BoxService,
    name: String,
    storage: RedisCacheStorage,
    subgraph_ttl: Option<Duration>,
    private_queries: Arc<RwLock<HashSet<String>>>,
    private_id: Option<String>,
}

impl Service<subgraph::Request> for CacheService {
    type Response = subgraph::Response;
    type Error = BoxError;
    type Future = <subgraph::BoxService as Service<subgraph::Request>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        match &mut self.0 {
            Some(s) => s.service.poll_ready(cx),
            None => panic!("service should have been called only once"),
        }
    }

    fn call(&mut self, request: subgraph::Request) -> Self::Future {
        match self.0.take() {
            None => panic!("service should have been called only once"),
            Some(s) => Box::pin(s.call_inner(request)),
        }
    }
}

impl InnerCacheService {
    async fn call_inner(
        mut self,
        request: subgraph::Request,
    ) -> Result<subgraph::Response, BoxError> {
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
                match cache_lookup_root(
                    self.name,
                    self.storage.clone(),
                    is_known_private,
                    private_id.as_deref(),
                    request,
                )
                .instrument(tracing::info_span!("cache_lookup"))
                .await?
                {
                    ControlFlow::Break(response) => Ok(response),
                    ControlFlow::Continue((request, mut root_cache_key)) => {
                        let response = self.service.call(request).await?;

                        let cache_control =
                            if response.response.headers().contains_key(CACHE_CONTROL) {
                                CacheControl::new(response.response.headers(), self.storage.ttl)?
                            } else {
                                let mut c = CacheControl::default();
                                c.no_store = true;
                                c
                            };

                        update_cache_control(&response.context, &cache_control);

                        if cache_control.private() {
                            // we did not know in advance that this was a query with a private scope, so we update the cache key
                            if !is_known_private {
                                self.private_queries.write().await.insert(query.to_string());
                            }

                            if let Some(s) = private_id.as_ref() {
                                root_cache_key = format!("{root_cache_key}:{s}");
                            } else {
                                // the response has a private scope but we don't have a way to differentiate users, so we do not store the response in cache
                                return Ok(response);
                            }
                        }

                        cache_store_root_from_response(
                            self.storage,
                            self.subgraph_ttl,
                            &response,
                            cache_control,
                            root_cache_key,
                        )
                        .await?;

                        Ok(response)
                    }
                }
            } else {
                self.service.call(request).await
            }
        } else {
            match cache_lookup_entities(
                self.name,
                self.storage.clone(),
                is_known_private,
                private_id.as_deref(),
                request,
            )
            .instrument(tracing::info_span!("cache_lookup"))
            .await?
            {
                ControlFlow::Break(response) => Ok(response),
                ControlFlow::Continue((request, cache_result)) => {
                    let mut response = self.service.call(request).await?;

                    let cache_control = if response.response.headers().contains_key(CACHE_CONTROL) {
                        CacheControl::new(response.response.headers(), self.storage.ttl)?
                    } else {
                        let mut c = CacheControl::default();
                        c.no_store = true;
                        c
                    };
                    update_cache_control(&response.context, &cache_control);

                    if !is_known_private && cache_control.private() {
                        self.private_queries.write().await.insert(query.to_string());
                    }

                    cache_store_entities_from_response(
                        self.storage,
                        self.subgraph_ttl,
                        &mut response,
                        cache_control,
                        cache_result.0,
                        is_known_private,
                        private_id,
                    )
                    .await?;
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
}

async fn cache_lookup_root(
    name: String,
    cache: RedisCacheStorage,
    is_known_private: bool,
    private_id: Option<&str>,
    mut request: subgraph::Request,
) -> Result<ControlFlow<subgraph::Response, (subgraph::Request, String)>, BoxError> {
    let body = request.subgraph_request.body_mut();

    let key = extract_cache_key_root(
        &name,
        &request.query_hash,
        body,
        &request.context,
        &request.authorization,
        is_known_private,
        private_id,
    );

    let cache_result: Option<RedisValue<CacheEntry>> = cache.get(RedisKey(key.clone())).await;

    match cache_result {
        Some(value) => {
            request.context.extensions().lock().insert(value.0.control);

            Ok(ControlFlow::Break(
                subgraph::Response::builder()
                    .data(value.0.data)
                    .extensions(Object::new())
                    .context(request.context)
                    .build(),
            ))
        }
        None => Ok(ControlFlow::Continue((request, key))),
    }
}

struct EntityCacheResults(Vec<IntermediateResult>);

async fn cache_lookup_entities(
    name: String,
    cache: RedisCacheStorage,
    is_known_private: bool,
    private_id: Option<&str>,
    mut request: subgraph::Request,
) -> Result<ControlFlow<subgraph::Response, (subgraph::Request, EntityCacheResults)>, BoxError> {
    let body = request.subgraph_request.body_mut();

    let keys = extract_cache_keys(
        &name,
        &request.query_hash,
        body,
        &request.context,
        &request.authorization,
        is_known_private,
        private_id,
    )?;

    let cache_result: Vec<Option<CacheEntry>> = cache
        .get_multiple(keys.iter().map(|k| RedisKey(k.clone())).collect::<Vec<_>>())
        .await
        .map(|res| res.into_iter().map(|r| r.map(|v| v.0)).collect())
        .unwrap_or_else(|| std::iter::repeat(None).take(keys.len()).collect());

    let representations = body
        .variables
        .get_mut(REPRESENTATIONS)
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");
    // remove from representations the entities we already obtained from the cache
    let (new_representations, cache_result, cache_control) =
        filter_representations(&name, representations, keys, cache_result)?;

    if let Some(control) = cache_control {
        update_cache_control(&request.context, &control);
    }

    if !new_representations.is_empty() {
        body.variables
            .insert(REPRESENTATIONS, new_representations.into());

        Ok(ControlFlow::Continue((
            request,
            EntityCacheResults(cache_result),
        )))
    } else {
        let entities = cache_result
            .into_iter()
            .filter_map(|res| res.cache_entry)
            .map(|entry| entry.data)
            .collect::<Vec<_>>();
        let mut data = Object::default();
        data.insert(ENTITIES, entities.into());

        Ok(ControlFlow::Break(
            subgraph::Response::builder()
                .data(data)
                .extensions(Object::new())
                .context(request.context)
                .build(),
        ))
    }
}

fn update_cache_control(context: &Context, cache_control: &CacheControl) {
    if let Some(c) = context.extensions().lock().get_mut::<CacheControl>() {
        *c = c.merge(cache_control);
        return;
    }
    //FIXME: race condition. We need an Entry API for private entries
    context.extensions().lock().insert(cache_control.clone());
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntry {
    control: CacheControl,
    data: Value,
}

async fn cache_store_root_from_response(
    cache: RedisCacheStorage,
    subgraph_ttl: Option<Duration>,
    response: &subgraph::Response,
    cache_control: CacheControl,
    cache_key: String,
) -> Result<(), BoxError> {
    if let Some(data) = response.response.body().data.as_ref() {
        let ttl: Option<Duration> = cache_control
            .ttl()
            .map(|secs| Duration::from_secs(secs as u64))
            .or(subgraph_ttl);

        if response.response.body().errors.is_empty() && cache_control.should_store() {
            let span = tracing::info_span!("cache_store");
            let data = data.clone();
            tokio::spawn(async move {
                cache
                    .insert(
                        RedisKey(cache_key),
                        RedisValue(CacheEntry {
                            control: cache_control,
                            data,
                        }),
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
    cache: RedisCacheStorage,
    subgraph_ttl: Option<Duration>,
    response: &mut subgraph::Response,
    cache_control: CacheControl,
    mut result_from_cache: Vec<IntermediateResult>,
    is_known_private: bool,
    private_id: Option<String>,
) -> Result<(), BoxError> {
    update_cache_control(&response.context, &cache_control);

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

pub(crate) fn hash_query(query_hash: &QueryHash, body: &graphql::Request) -> String {
    let mut digest = Sha256::new();
    digest.update(&query_hash.0);
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
    digest.update(&serde_json::to_vec(&body.variables).unwrap());
    if let Some(representations) = representations {
        body.variables.insert(repr_key, representations);
    }

    digest.update(&serde_json::to_vec(cache_key).unwrap());

    if let Ok(Some(cache_data)) = context.get::<&str, Object>(CONTEXT_CACHE_KEY) {
        if let Some(v) = cache_data.get("all") {
            digest.update(&serde_json::to_vec(v).unwrap())
        }
        if let Some(v) = body
            .operation_name
            .as_ref()
            .and_then(|op| cache_data.get(op.as_str()))
        {
            digest.update(&serde_json::to_vec(v).unwrap())
        }
    }

    hex::encode(digest.finalize().as_slice())
}

// build a cache key for the root operation
fn extract_cache_key_root(
    subgraph_name: &str,
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

    // the cache key is written to easily find keys matching a prefix for deletion:
    // - subgraph name: caching is done per subgraph
    // - query hash: invalidate the entry for a specific query and operation name
    // - additional data: separate cache entries depending on info like authorization status
    let mut key = String::new();
    let _ = write!(
        &mut key,
        "subgraph:{subgraph_name}:Query:{query_hash}:{additional_data_hash}"
    );

    if is_known_private {
        if let Some(id) = private_id {
            let _ = write!(&mut key, ":{id}");
        }
    }
    key
}

// build a list of keys to get from the cache in one query
fn extract_cache_keys(
    subgraph_name: &str,
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

    let mut res = Vec::new();
    for representation in representations {
        let opt_type = representation
            .as_object_mut()
            .and_then(|o| o.remove(TYPENAME))
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "missing __typename in representation".to_string(),
            })?;

        let typename = opt_type.as_str().unwrap_or("-");

        // We have to hash the representation because it can contains PII
        let mut digest = Sha256::new();
        digest.update(serde_json::to_string(&representation).unwrap().as_bytes());
        let hashed_entity_key = hex::encode(digest.finalize().as_slice());

        // the cache key is written to easily find keys matching a prefix for deletion:
        // - subgraph name: caching is done per subgraph
        // - type: can invalidate all instances of a type
        // - entity key: invalidate a specific entity
        // - query hash: invalidate the entry for a specific query and operation name
        // - additional data: separate cache entries depending on info like authorization status
        let mut key = String::new();
        let _ = write!(&mut key,  "subgraph:{subgraph_name}:{typename}:{hashed_entity_key}:{query_hash}:{additional_data_hash}");
        if is_known_private {
            if let Some(id) = private_id {
                let _ = write!(&mut key, ":{id}");
            }
        }

        representation
            .as_object_mut()
            .map(|o| o.insert(TYPENAME, opt_type));
        res.push(key);
    }
    Ok(res)
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
) -> Result<(Vec<Value>, Vec<IntermediateResult>, Option<CacheControl>), BoxError> {
    let mut new_representations: Vec<Value> = Vec::new();
    let mut result = Vec::new();
    let mut cache_hit: HashMap<String, (usize, usize)> = HashMap::new();
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
                cache_hit.entry(typename.clone()).or_default().1 += 1;

                representation
                    .as_object_mut()
                    .map(|o| o.insert(TYPENAME, opt_type));
                new_representations.push(representation);
            }
            Some(entry) => {
                cache_hit.entry(typename.clone()).or_default().0 += 1;
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

    for (ty, (hit, miss)) in cache_hit {
        tracing::info!(
            monotonic_counter.apollo.router.operations.entity.cache = hit as u64,
            entity_type = ty.as_str(),
            hit = %true,
            %subgraph_name
        );
        tracing::info!(
            monotonic_counter.apollo.router.operations.entity.cache = miss as u64,
            entity_type = ty.as_str(),
            miss = %true,
            %subgraph_name
        );
        tracing::event!(
            Level::TRACE,
            entity_type = ty.as_str(),
            cache_hit = hit,
            cache_miss = miss
        );
    }

    Ok((new_representations, result, cache_control))
}

// fill in the entities for the response
#[allow(clippy::too_many_arguments)]
async fn insert_entities_in_result(
    entities: &mut Vec<Value>,
    errors: &[Error],
    cache: RedisCacheStorage,
    subgraph_ttl: Option<Duration>,
    cache_control: CacheControl,
    result: &mut Vec<IntermediateResult>,
    update_key_private: Option<String>,
    should_cache_private: bool,
) -> Result<(Vec<Value>, Vec<Error>), BoxError> {
    let ttl: Option<Duration> = cache_control
        .ttl()
        .map(|secs| Duration::from_secs(secs as u64))
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
                        RedisKey(key),
                        RedisValue(CacheEntry {
                            control: cache_control.clone(),
                            data: value.clone(),
                        }),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Key {
    #[serde(rename = "type")]
    opt_type: Option<Value>,
    id: Value,
}
