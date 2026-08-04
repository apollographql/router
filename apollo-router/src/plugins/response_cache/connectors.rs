use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::StringTemplate;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use lru::LruCache;
use opentelemetry::Array;
use opentelemetry::Key;
use opentelemetry::StringValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tokio::sync::RwLock;
use tower::BoxError;
use tower_service::Service;
use tracing::Instrument;
use tracing::Span;

use super::cache_control::CacheControl;
use super::cache_tag::CacheScope;
use super::cache_tag::CacheTag;
use super::invalidation_endpoint::IndexMode;
use super::invalidation_endpoint::InvalidationIndexes;
use super::invalidation_endpoint::SubgraphInvalidationConfig;
use super::metrics::CacheMetricContextKey;
use super::metrics::record_fetch_error;
use super::plugin::CACHE_DEBUG_HEADER_NAME;
use super::plugin::CACHE_TAG_DIRECTIVE_NAME;
use super::plugin::CacheHitMiss;
use super::plugin::CacheSubgraph;
use super::plugin::ENTITIES;
use super::plugin::IntermediateResult;
use super::plugin::PrivateQueryKey;
use super::plugin::REPRESENTATIONS;
use super::plugin::StorageInterface;
use super::plugin::Ttl;
use super::plugin::assemble_response_from_errors;
use super::plugin::get_invalidation_entity_keys_from_schema;
use super::plugin::hash_private_id;
use super::plugin::update_cache_control;
use super::storage;
use super::storage::CacheEntry;
use super::storage::CacheStorage;
use super::storage::Document;
use super::storage::redis::Storage;
use crate::Context;
use crate::context::OPERATION_KIND;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::connectors::query_plans::get_connectors;
use crate::plugins::response_cache::cache_key::ConnectorCacheKeyEntity;
use crate::plugins::response_cache::cache_key::ConnectorCacheKeyRoot;
use crate::plugins::response_cache::cache_key::hash_connector_additional_data;
use crate::plugins::response_cache::cache_key::hash_operation;
use crate::plugins::response_cache::debugger::CacheEntryKind;
use crate::plugins::response_cache::debugger::CacheKeyContext;
use crate::plugins::response_cache::debugger::CacheKeySource;
use crate::plugins::response_cache::debugger::add_cache_key_to_context;
use crate::plugins::response_cache::debugger::add_cache_keys_to_context;
use crate::plugins::telemetry::LruSizeInstrument;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::span_ext::SpanMarkError;
use crate::query_planner::OperationKind;
use crate::services::connect;
use crate::services::connector;
use crate::spec::TYPENAME;

/// Per connector source configuration for response caching
#[derive(Clone, Debug, Default, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct ConnectorCacheConfiguration {
    /// Options applying to all connector sources
    pub(crate) all: ConnectorCacheSource,

    /// Map of subgraph_name.connector_source_name to configuration
    #[serde(default)]
    pub(crate) sources: HashMap<String, ConnectorCacheSource>,
}

impl ConnectorCacheConfiguration {
    /// Get the configuration for a specific connector source, falling back to `all`.
    pub(crate) fn get(&self, source_name: &str) -> &ConnectorCacheSource {
        self.sources.get(source_name).unwrap_or(&self.all)
    }

    /// Resolve the effective [`InvalidationIndexes`] for `source_name`. Prefers the per-source
    /// `invalidation` block, falls back to the `all` block, and finally to
    /// [`InvalidationIndexes::default`] when neither is configured. Mirrors
    /// `effective_invalidation_indexes` on the subgraph path so the connector write path and the
    /// `/invalidation` endpoint stay in lockstep.
    pub(super) fn effective_indexes(&self, source_name: &str) -> InvalidationIndexes {
        if let Some(source_invalidation) = self
            .sources
            .get(source_name)
            .and_then(|s| s.invalidation.as_ref())
        {
            return source_invalidation.indexes;
        }
        if let Some(all_invalidation) = self.all.invalidation.as_ref() {
            return all_invalidation.indexes;
        }
        InvalidationIndexes::default()
    }

    /// Returns whether caching is enabled for a specific connector source.
    pub(super) fn is_source_enabled(&self, plugin_enabled: bool, source_name: &str) -> bool {
        if !plugin_enabled {
            return false;
        }
        match (self.all.enabled, self.get(source_name).enabled) {
            (_, Some(x)) => x, // explicit per-source setting overrides the `all` default
            (Some(true) | None, None) => true,
            (Some(false), None) => false,
        }
    }
}

/// Per connector source configuration for response caching
#[derive(Clone, Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct ConnectorCacheSource {
    /// Redis configuration
    pub(crate) redis: Option<storage::redis::Config>,

    /// Expiration for all keys for this connector source, unless overridden by the `Cache-Control` header in connector responses
    pub(crate) ttl: Option<Ttl>,

    /// Activates caching for this connector source, overrides the global configuration
    pub(crate) enabled: Option<bool>,

    /// Context key used to separate cache sections per user
    pub(crate) private_id: Option<String>,

    /// Invalidation configuration
    pub(crate) invalidation: Option<SubgraphInvalidationConfig>,
}

// --- Connector Cache Service ---

/// Cached entity results stored in context extensions for merging back into the connector response.
#[derive(Default)]
struct ConnectorCachedEntities {
    /// The intermediate results from the cache lookup, indexed by original position
    results: Vec<IntermediateResult>,
    /// The cache control from cached entries
    cache_control: Option<CacheControl>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct ConnectorCacheService {
    pub(super) service: tower::util::BoxCloneService<connect::Request, connect::Response, BoxError>,
    pub(super) storage: Arc<StorageInterface>,
    pub(super) connectors_config: Arc<ConnectorCacheConfiguration>,
    pub(super) private_queries: Arc<RwLock<LruCache<PrivateQueryKey, ()>>>,
    pub(super) debug: bool,
    pub(super) supergraph_schema: Arc<Valid<Schema>>,
    pub(super) subgraph_enums: Arc<HashMap<String, String>>,
    pub(super) lru_size_instrument: LruSizeInstrument,
}

impl Service<connect::Request> for ConnectorCacheService {
    type Response = connect::Response;
    type Error = BoxError;
    type Future = <connect::BoxService as Service<connect::Request>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: connect::Request) -> Self::Future {
        let clone = self.clone();
        let inner = std::mem::replace(self, clone);

        Box::pin(inner.connector_call_inner(request))
    }
}

impl ConnectorCacheService {
    async fn connector_call_inner(
        mut self,
        request: connect::Request,
    ) -> Result<connect::Response, BoxError> {
        // Look up the connector to get the source_config_key
        let connectors = get_connectors(&request.context);
        let connector = connectors
            .as_ref()
            .and_then(|c| c.get(&request.service_name));

        let source_name = connector.map(|c| c.source_config_key()).unwrap_or_default();
        let connector_synthetic_name = connector.map(|c| c.id.synthetic_name()).unwrap_or_default();

        // Check if caching is enabled for this connector source
        let connector_config = self.connectors_config.get(&source_name);
        if !self.connectors_config.is_source_enabled(true, &source_name) {
            return self.service.call(request).await;
        }

        // Skip cache entirely for non-Query operations (mutations, subscriptions)
        if let Ok(Some(operation_kind)) = request.context.get::<_, OperationKind>(OPERATION_KIND)
            && operation_kind != OperationKind::Query
        {
            return self.service.call(request).await;
        }

        // Check if the request is part of a batch. If it is, completely bypass response caching
        // since it will break any request batches which this request is part of.
        // This check is what enables Batching and response caching to work together, so be very
        // careful before making any changes to it.
        if request.is_part_of_batch() {
            return self.service.call(request).await;
        }

        // Gate debug mode on the per-request header, matching the subgraph path
        self.debug = self.debug
            && (request
                .supergraph_request
                .headers()
                .get(CACHE_DEBUG_HEADER_NAME)
                == Some(&HeaderValue::from_static("true")));

        let storage = match self.storage.get_connector(&source_name) {
            Some(storage) => storage.clone(),
            None => {
                record_fetch_error(&storage::Error::NoStorage, &source_name);
                return self.service.call(request).await;
            }
        };

        let connector_ttl = connector_config
            .ttl
            .clone()
            .map(|t| t.0)
            .or_else(|| self.connectors_config.all.ttl.clone().map(|t| t.0))
            .unwrap_or_else(|| Duration::from_secs(60 * 60 * 24));

        let private_id_key = connector_config
            .private_id
            .clone()
            .or_else(|| self.connectors_config.all.private_id.clone());

        let private_id = private_id_key
            .as_ref()
            .and_then(|key| hash_private_id(&request.context, key));

        // Build private query key for LRU tracking
        let operation_str = request.operation.serialize().no_indent().to_string();
        let private_query_key = PrivateQueryKey {
            query_hash: hash_operation(&operation_str),
            has_private_id: private_id.is_some(),
        };

        let is_known_private = {
            self.private_queries
                .read()
                .await
                .contains(&private_query_key)
        };

        // [RFC 9111](https://datatracker.ietf.org/doc/html/rfc9111):
        //  * no-store: allows serving response from cache, but prohibits storing response in cache
        //  * no-cache: prohibits serving response from cache, but allows storing response in cache
        //
        // NB: no-cache actually prohibits serving response from cache _without revalidation_, but
        //  in the router this is the same thing
        let request_cache_control = if request
            .supergraph_request
            .headers()
            .contains_key(&CACHE_CONTROL)
        {
            let cache_control = match CacheControl::try_from(request.supergraph_request.headers()) {
                Ok(cache_control) => cache_control,
                Err(err) => {
                    return Ok(connect::Response {
                        response: http::Response::builder()
                            .body(
                                graphql::Response::builder()
                                    .error(
                                        graphql::Error::builder()
                                            .message(format!(
                                                "cannot get cache-control header: {err}"
                                            ))
                                            .extension_code("INVALID_CACHE_CONTROL_HEADER")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .unwrap(),
                    });
                }
            };

            // Don't use cache at all if both no-store and no-cache are set
            if cache_control.no_cache() && cache_control.no_store() {
                return self.service.call(request).await;
            }
            Some(cache_control)
        } else {
            None
        };

        // Check if this is an entity query (has representations) — needed before private bypass
        // to determine debug entry kind
        let is_entity = request.variables.variables.contains_key(REPRESENTATIONS);

        // The response will have a private scope but we don't have a way to differentiate users,
        // so we know we will not get or store anything in the cache
        if is_known_private && private_id.is_none() {
            let debug_request = if self.debug {
                Some(
                    graphql::Request::builder()
                        .query(operation_str.clone())
                        .variables(request.variables.variables.clone().into_iter().collect())
                        .build(),
                )
            } else {
                None
            };

            let context = request.context.clone();
            let resp = self.service.call(request).await?;

            if self.debug {
                // Use no_store cache control — this is a known-private query without private_id,
                // so we won't be storing anything regardless of what the upstream returns
                let cache_control = CacheControl::default_no_store();
                let kind = if is_entity {
                    CacheEntryKind::Entity {
                        typename: "".to_string(),
                        entity_key: Default::default(),
                    }
                } else {
                    CacheEntryKind::RootFields {
                        root_fields: Vec::new(),
                    }
                };

                let cache_key_context = CacheKeyContext {
                    key: "-".to_string(),
                    invalidation_keys: vec![],
                    // TODO: connector debugger tag/CDN flags are undecided (pending router-team decision)
                    has_tags: false,
                    cdn_invalidation_enabled: false,
                    indexes: self.connectors_config.effective_indexes(&source_name),
                    kind,
                    hashed_private_id: None,
                    subgraph_name: source_name.to_string(),
                    subgraph_request: debug_request.unwrap_or_default(),
                    source: CacheKeySource::Connector,
                    cache_control,
                    data: serde_json_bytes::to_value(resp.response.body().clone())
                        .unwrap_or_default(),
                    warnings: Vec::new(),
                    should_store: false,
                }
                .update_metadata();
                add_cache_key_to_context(&context, cache_key_context)?;
            }

            return Ok(resp);
        }

        if is_entity {
            let source_name_span = source_name.clone();
            let private_id_exists = private_id.is_some();
            let is_debug = self.debug;
            let indexes = self.connectors_config.effective_indexes(&source_name);
            // Snapshot the connector's referenced $context/$request.headers inputs for the cache
            // key (request-scoped, shared by every representation in the batch).
            let key_inputs = connector_key_inputs(
                connector,
                &request.context,
                request.supergraph_request.headers(),
            );
            self.handle_entity_query(
                request,
                storage,
                source_name,
                connector_synthetic_name,
                connector_ttl,
                private_id,
                request_cache_control,
                is_known_private,
                private_query_key,
                indexes,
                key_inputs,
            )
            .instrument(tracing::info_span!(
                "response_cache.lookup",
                kind = "entity",
                "connector.source" = source_name_span.as_str(),
                debug = is_debug,
                private = is_known_private,
                contains_private_id = private_id_exists,
            ))
            .await
        } else {
            // Root field queries are handled at the connector_request_service level
            self.service.call(request).await
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_entity_query(
        mut self,
        mut request: connect::Request,
        storage: Storage,
        source_name: String,
        connector_synthetic_name: String,
        connector_ttl: Duration,
        private_id: Option<String>,
        request_cache_control: Option<CacheControl>,
        is_known_private: bool,
        private_query_key: PrivateQueryKey,
        indexes: InvalidationIndexes,
        key_inputs: Value,
    ) -> Result<connect::Response, BoxError> {
        // Get auth metadata from context
        let auth_metadata = request
            .context
            .extensions()
            .with_lock(|lock| lock.get::<CacheKeyMetadata>().cloned())
            .unwrap_or_default();

        // Hash the operation for use in cache keys
        let operation_str = request.operation.serialize().no_indent().to_string();
        let operation_hash = hash_operation(&operation_str);

        // Hash additional data (variables minus representations + auth metadata + referenced
        // per-request $context/$request.headers inputs). The referenced inputs are request-scoped,
        // so all representation keys in this batch share them (computed once by the caller).
        let additional_data_hash = hash_connector_additional_data(
            &source_name,
            &request.variables.variables,
            &request.context,
            &auth_metadata,
            &key_inputs,
        );

        // Build debug request before representations are mutably borrowed
        let debug_request = if self.debug {
            Some(
                graphql::Request::builder()
                    .query(operation_str.clone())
                    .variables(request.variables.variables.clone().into_iter().collect())
                    .build(),
            )
        } else {
            None
        };

        let representations = request
            .variables
            .variables
            .get_mut(REPRESENTATIONS)
            .and_then(|value| value.as_array_mut());

        let Some(representations) = representations else {
            // No representations found, pass through
            return self.service.call(request).await;
        };

        // Build cache keys for each representation
        let mut cache_keys = Vec::with_capacity(representations.len());
        for representation in representations.iter_mut() {
            let representation_obj =
                representation
                    .as_object_mut()
                    .ok_or_else(|| FetchError::MalformedRequest {
                        reason: "representation variable should be an array of objects".to_string(),
                    })?;

            let typename_value = representation_obj
                .get(TYPENAME)
                .ok_or_else(|| FetchError::MalformedRequest {
                    reason: "missing __typename in representation".to_string(),
                })?
                .clone();

            let typename = typename_value
                .as_str()
                .ok_or_else(|| FetchError::MalformedRequest {
                    reason: "__typename in representation is not a string".to_string(),
                })?;

            // Temporarily remove __typename for hashing (same as subgraph flow)
            representation_obj.remove(TYPENAME);

            // Get the entity key from `representation`, only needed in debug for the cache debugger.
            // Connectors don't use @key directives — their key fields are derived from variable
            // references ($args, $this, $batch) and stored on ConnectRequest.keys as a FieldSet.
            // We extract key field values directly rather than using
            // get_entity_key_from_selection_set.
            let representation_entity_key = if self.debug {
                request.keys.as_ref().map(|keys| {
                    let default_document = Default::default();
                    let mut entity_key = serde_json_bytes::Map::new();
                    for field in keys.selection_set.root_fields(&default_document) {
                        let key = field.name.as_str();
                        if let Some(val) = representation_obj.get(key) {
                            entity_key.insert(ByteString::from(key), val.clone());
                        }
                    }
                    entity_key
                })
            } else {
                None
            };

            let cache_key = ConnectorCacheKeyEntity {
                source_name: &source_name,
                entity_type: typename,
                representation: representation_obj,
                operation_hash: &operation_hash,
                additional_data_hash: &additional_data_hash,
                private_id: if is_known_private {
                    private_id.as_deref()
                } else {
                    None
                },
            }
            .hash();

            // Build typed cache tags, gated on the connector's resolved invalidation indexes.
            // The whole-source (`Subgraph`) tag is prepended at store time, mirroring dev's
            // subgraph path.
            let mut cache_tags: Vec<CacheTag> = Vec::new();
            if indexes.is_enabled(IndexMode::Type) {
                cache_tags.push(CacheTag::Type(typename.to_string()));
            }
            // Extract @cacheTag invalidation keys from the supergraph schema.
            if indexes.is_enabled(IndexMode::CacheTag)
                && let Ok(cache_tag_keys) = get_invalidation_entity_keys_from_schema(
                    &self.supergraph_schema,
                    &connector_synthetic_name,
                    &self.subgraph_enums,
                    typename,
                    representation_obj,
                )
            {
                cache_tags.extend(cache_tag_keys.into_iter().map(CacheTag::Tag));
            }

            // Restore __typename
            representation_obj.insert(TYPENAME, typename_value);

            cache_keys.push(CacheMetadata {
                cache_key,
                cache_tags,
                entity_key: representation_entity_key,
            });
        }

        let entities = cache_keys.len() as u64;
        u64_histogram_with_unit!(
            "apollo.router.operations.response_cache.fetch.entity",
            "Number of entities per subgraph fetch node",
            "{entity}",
            entities,
            "connector.source" = source_name.to_string()
        );

        // Record cache keys on the lookup span
        Span::current().set_span_dyn_attribute(
            "cache.keys".into(),
            opentelemetry::Value::Array(Array::String(
                cache_keys
                    .iter()
                    .map(|k| StringValue::from(k.cache_key.clone()))
                    .collect(),
            )),
        );

        // Batch fetch from cache
        // Skip cache lookup if request had no-cache — we have no means of revalidating entries
        // without just performing the query, so there's no benefit to hitting the cache
        let cache_result: Vec<Option<CacheEntry>> =
            if request_cache_control.as_ref().is_some_and(|c| c.no_cache()) {
                std::iter::repeat_n(None, cache_keys.len()).collect()
            } else {
                let keys_for_fetch: Vec<&str> =
                    cache_keys.iter().map(|k| k.cache_key.as_str()).collect();
                let cache_result = storage.fetch_multiple(&keys_for_fetch, &source_name).await;

                match cache_result {
                    Ok(res) => res
                        .into_iter()
                        .map(|v| match v {
                            Some(v) if v.control.can_use() => Some(v),
                            _ => None,
                        })
                        .collect(),
                    Err(err) => {
                        if !err.is_row_not_found() {
                            Span::current().mark_as_error(format!("cannot get cache entry: {err}"));
                            tracing::warn!(error = %err, "cannot get connector cache entries");
                        }
                        std::iter::repeat_n(None, cache_keys.len()).collect()
                    }
                }
            };

        // Filter representations: remove cached ones
        let mut new_representations: Vec<Value> = Vec::new();
        let mut intermediate_results: Vec<IntermediateResult> = Vec::new();
        let mut cache_control: Option<CacheControl> = None;
        let mut cache_hit_miss: HashMap<String, CacheHitMiss> = HashMap::new();

        let representations = request
            .variables
            .variables
            .get_mut(REPRESENTATIONS)
            .and_then(|value| value.as_array_mut())
            .expect("representations should exist");

        // Record graphql.types on the lookup span (deduplicated typenames)
        let typenames_for_span: HashSet<String> = representations
            .iter()
            .filter_map(|r| {
                r.as_object()
                    .and_then(|o| o.get(TYPENAME))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .collect();
        Span::current().set_span_dyn_attribute(
            Key::from_static_str("graphql.types"),
            opentelemetry::Value::Array(
                typenames_for_span
                    .into_iter()
                    .map(StringValue::from)
                    .collect::<Vec<StringValue>>()
                    .into(),
            ),
        );

        for ((representation, metadata), entry) in
            representations.drain(..).zip(cache_keys).zip(cache_result)
        {
            let typename = representation
                .as_object()
                .and_then(|o| o.get(TYPENAME))
                .and_then(|v| v.as_str())
                .unwrap_or("-")
                .to_string();

            match &entry {
                Some(cache_entry) => {
                    // Cache hit - merge cache control
                    cache_hit_miss.entry(typename.clone()).or_default().hit += 1;
                    match cache_control.as_mut() {
                        None => cache_control = Some(cache_entry.control.clone()),
                        Some(c) => *c = c.merge(&cache_entry.control),
                    }
                }
                None => {
                    // Cache miss - keep for downstream
                    cache_hit_miss.entry(typename.clone()).or_default().miss += 1;
                    new_representations.push(representation);
                }
            }

            intermediate_results.push(IntermediateResult {
                key: metadata.cache_key,
                cache_tags: metadata.cache_tags,
                typename,
                // TODO: connector CDN cache-tag population is undecided (pending router-team decision)
                cdn_invalidation_tags: Vec::new(),
                entity_key: metadata.entity_key,
                cache_entry: entry,
            });
        }

        // Record cache.status on the lookup span
        let cache_status = if new_representations.is_empty() {
            "hit"
        } else if intermediate_results
            .iter()
            .any(|ir| ir.cache_entry.is_some())
        {
            "partial_hit"
        } else {
            "miss"
        };
        Span::current().set_span_dyn_attribute(
            opentelemetry::Key::new("cache.status"),
            opentelemetry::Value::String(cache_status.into()),
        );

        // Store cache hit/miss metrics in context for telemetry
        let _ = request.context.insert(
            CacheMetricContextKey::new(source_name.clone()),
            CacheSubgraph(cache_hit_miss),
        );

        // Add debug entries for cache hits
        if self.debug
            && let Some(ref debug_req) = debug_request
        {
            let debug_cache_keys_ctx = intermediate_results.iter().filter_map(|ir| {
                ir.cache_entry.as_ref().map(|cache_entry| {
                    CacheKeyContext {
                        key: ir.key.clone(),
                        hashed_private_id: private_id.clone(),
                        invalidation_keys: ir
                            .cache_tags
                            .iter()
                            .filter_map(|t| t.user_value().map(String::from))
                            .collect(),
                        // TODO: connector debugger tag/CDN flags are undecided (pending router-team decision)
                        has_tags: false,
                        cdn_invalidation_enabled: false,
                        indexes,
                        kind: CacheEntryKind::Entity {
                            typename: ir.typename.clone(),
                            entity_key: ir.entity_key.clone().unwrap_or_default(),
                        },
                        subgraph_name: source_name.clone(),
                        subgraph_request: debug_req.clone(),
                        source: CacheKeySource::Cache,
                        cache_control: cache_entry.control.clone(),
                        data: serde_json_bytes::json!({
                            "data": cache_entry.data.clone()
                        }),
                        warnings: Vec::new(),
                        should_store: false,
                    }
                    .update_metadata()
                })
            });
            add_cache_keys_to_context(&request.context, debug_cache_keys_ctx)?;
        }

        if !new_representations.is_empty() {
            // Partial or full miss - update representations and continue
            request
                .variables
                .variables
                .insert(REPRESENTATIONS, new_representations.into());

            // Store cached results for merging on response
            let mut cached_entities = ConnectorCachedEntities {
                results: intermediate_results,
                cache_control,
            };

            let debug = self.debug;
            let context = request.context.clone();
            let mut response = match self.service.call(request).await {
                Ok(response) => response,
                Err(e) => {
                    let e = match e.downcast::<FetchError>() {
                        Ok(inner) => match *inner {
                            FetchError::SubrequestHttpError { .. } => *inner,
                            _ => FetchError::SubrequestHttpError {
                                status_code: None,
                                service: source_name.clone(),
                                reason: inner.to_string(),
                            },
                        },
                        Err(e) => FetchError::SubrequestHttpError {
                            status_code: None,
                            service: source_name.clone(),
                            reason: e.to_string(),
                        },
                    };

                    let graphql_error = e.to_graphql_error(None);

                    let (new_entities, new_errors) = assemble_response_from_errors(
                        &[graphql_error],
                        &mut cached_entities.results,
                    );

                    let mut data = Object::default();
                    data.insert(ENTITIES, new_entities.into());

                    let response = connect::Response {
                        response: http::Response::builder()
                            .body(
                                graphql::Response::builder()
                                    .data(data)
                                    .errors(new_errors)
                                    .build(),
                            )
                            .unwrap(),
                    };

                    update_cache_control(&context, &CacheControl::default_no_store());

                    return Ok(response);
                }
            };

            // Merge cached entities back into the response
            Self::merge_cached_entities(
                &mut response,
                &context,
                cached_entities,
                &storage,
                &source_name,
                &connector_synthetic_name,
                connector_ttl,
                debug,
                debug_request,
                private_id,
                request_cache_control,
                is_known_private,
                private_query_key,
                &self.private_queries,
                &self.lru_size_instrument,
                indexes,
            )
            .await?;

            Ok(response)
        } else {
            // All entities cached - build response directly
            let entities: Vec<Value> = intermediate_results
                .iter()
                .filter_map(|r| r.cache_entry.as_ref())
                .map(|entry| entry.data.clone())
                .collect();

            let mut data = Object::default();
            data.insert(ENTITIES, entities.into());

            let response = connect::Response {
                response: http::Response::builder()
                    .body(graphql::Response::builder().data(data).build())
                    .unwrap(),
            };

            if let Some(cc) = cache_control {
                update_cache_control(&request.context, &cc);
            }

            Ok(response)
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn merge_cached_entities(
        response: &mut connect::Response,
        context: &Context,
        cached: ConnectorCachedEntities,
        storage: &Storage,
        source_name: &str,
        connector_synthetic_name: &str,
        connector_ttl: Duration,
        debug: bool,
        debug_request: Option<graphql::Request>,
        private_id: Option<String>,
        request_cache_control: Option<CacheControl>,
        is_known_private: bool,
        private_query_key: PrivateQueryKey,
        private_queries: &Arc<RwLock<LruCache<PrivateQueryKey, ()>>>,
        lru_size_instrument: &LruSizeInstrument,
        indexes: InvalidationIndexes,
    ) -> Result<(), BoxError> {
        let ConnectorCachedEntities {
            mut results,
            cache_control: cached_cache_control,
        } = cached;

        // Get this connector's own response cache-control (recorded per HTTP response by the
        // connector request service, merged min-TTL across the batch). Never read the
        // request-wide aggregate here: it merges unrelated fetches (e.g. a headerless subgraph
        // response upstream in the plan) and would poison storability/TTL/privacy for these
        // entities.
        let mut response_cache_control =
            get_connector_cache_control(context, connector_synthetic_name)
                .unwrap_or_else(CacheControl::default_no_store);

        // If the request had no-store, propagate that to the response cache control
        if let Some(ref req_cc) = request_cache_control {
            response_cache_control.merge_no_store(req_cc);
        }

        // Track private queries in the LRU so future requests can short-circuit
        if response_cache_control.private() && !is_known_private {
            let size = {
                let mut pq = private_queries.write().await;
                pq.put(private_query_key, ());
                pq.len()
            };
            lru_size_instrument.update(size as u64);
        }

        // The response has a private scope but we don't have a way to differentiate
        // users, so we do not store the response in cache
        let unstorable_private_response = response_cache_control.private() && private_id.is_none();

        // If the response is private but wasn't known-private before, we need to append
        // the private_id to cache keys before storing (matching the subgraph pattern in
        // insert_entities_in_result). This ensures the stored keys include the private_id
        // suffix that will be used in subsequent lookups (when is_known_private is true).
        let update_key_private = if !is_known_private && response_cache_control.private() {
            private_id.clone()
        } else {
            None
        };

        // Merge the cached and response cache controls
        let merged_cache_control = match cached_cache_control {
            Some(cached_cc) => cached_cc.merge(&response_cache_control),
            None => response_cache_control.clone(),
        };

        // Take the response data out to avoid borrow issues
        let mut response_data = response.response.body_mut().data.take();

        let entities = response_data
            .as_mut()
            .and_then(|v| v.as_object_mut())
            .and_then(|o| o.remove(ENTITIES))
            .and_then(|v| match v {
                Value::Array(arr) => Some(arr),
                _ => None,
            });

        let Some(mut entities) = entities else {
            // No _entities in response (e.g., connector returned HTTP error).
            // Build a partial response with cached entities + null/errors for misses,
            // matching the subgraph path in cache_store_entities_from_response.
            let (new_entities, new_errors) =
                assemble_response_from_errors(&response.response.body().errors, &mut results);

            let mut data = Object::default();
            data.insert(ENTITIES, new_entities.into());

            response.response.body_mut().data = Some(Value::Object(data));
            response.response.body_mut().errors = new_errors;

            update_cache_control(context, &CacheControl::default_no_store());

            return Ok(());
        };

        let ttl = response_cache_control
            .ttl()
            .map(Duration::from_secs)
            .unwrap_or(connector_ttl);

        // Merge: iterate through results, inserting cached entities at correct positions
        let errors = response.response.body().errors.clone();
        let mut new_entities = Vec::new();
        let mut new_errors = Vec::new();
        let mut to_insert: Vec<Document> = Vec::new();
        let mut debug_ctx_entries: Vec<CacheKeyContext> = Vec::new();
        let mut entities_iter = entities.drain(..).enumerate();

        for (
            new_entity_idx,
            IntermediateResult {
                key,
                cache_tags,
                typename,
                entity_key,
                cache_entry,
                ..
            },
        ) in results.drain(..).enumerate()
        {
            match cache_entry {
                Some(entry) => {
                    // Was cached - use cached value
                    new_entities.push(entry.data);
                }
                None => {
                    // Was not cached - take from response and store. A response with fewer
                    // entities than requested representations is malformed: erroring here (like
                    // the subgraph path) beats silently mis-merging positionally-keyed data.
                    let (entity_idx, value) =
                        entities_iter
                            .next()
                            .ok_or_else(|| FetchError::MalformedResponse {
                                reason: "invalid number of entities".to_string(),
                            })?;
                    {
                        // Check for per-entity errors (matching the subgraph pattern in
                        // insert_entities_in_result). Entities with errors should not be cached
                        // to avoid persisting error data until TTL expires.
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
                            // Update the entity index to match the merged position
                            let mut e = error.clone();
                            if let Some(path) = e.path.as_mut() {
                                path.0[1] = PathElement::Index(new_entity_idx);
                            }
                            new_errors.push(e);
                            has_errors = true;
                        }

                        // Append private_id to cache key if response was discovered
                        // to be private mid-flight (matching subgraph pattern)
                        let key = if let Some(ref id) = update_key_private {
                            format!("{key}:{id}")
                        } else {
                            key
                        };

                        // Surface user-facing tags for the debugger before `cache_tags` is moved
                        // into the stored document.
                        let user_tags: Vec<String> = cache_tags
                            .iter()
                            .filter_map(|t| t.user_value().map(String::from))
                            .collect();

                        // Never cache a null entity value. On the connector `$batch` path an item
                        // the upstream omitted is padded to `Value::Null` (match-by-id), with no
                        // GraphQL error — caching it would freeze a transient omission as a null
                        // for the whole TTL. A legitimately-present entity is a JSON object, so
                        // this only skips genuinely-unresolved entities; they re-fetch next time.
                        let is_null_entity = value.is_null();

                        if !has_errors
                            && !is_null_entity
                            && !unstorable_private_response
                            && response_cache_control.should_store()
                        {
                            // Prepend the whole-source (`Subgraph`) tag at store time, gated on
                            // the connector's resolved invalidation indexes (mirrors dev's
                            // subgraph store path).
                            let mut doc_cache_tags = cache_tags;
                            if indexes.is_enabled(IndexMode::Subgraph) {
                                doc_cache_tags.insert(0, CacheTag::Subgraph);
                            }
                            to_insert.push(Document {
                                control: response_cache_control.clone(),
                                data: value.clone(),
                                key: key.clone(),
                                cache_tags: doc_cache_tags,
                                expire: ttl,
                                // TODO: connector CDN cache-tag population is undecided (pending router-team decision)
                                cdn_invalidation_tags: Vec::new(),
                                scope: CacheScope::Connector,
                            });
                        }

                        if debug && let Some(ref debug_req) = debug_request {
                            debug_ctx_entries.push(
                                CacheKeyContext {
                                    key,
                                    hashed_private_id: private_id.clone(),
                                    invalidation_keys: user_tags,
                                    // TODO: connector debugger tag/CDN flags are undecided (pending router-team decision)
                                    has_tags: false,
                                    cdn_invalidation_enabled: false,
                                    indexes,
                                    kind: CacheEntryKind::Entity {
                                        typename,
                                        entity_key: entity_key.unwrap_or_default(),
                                    },
                                    subgraph_name: source_name.to_string(),
                                    subgraph_request: debug_req.clone(),
                                    source: CacheKeySource::Connector,
                                    cache_control: response_cache_control.clone(),
                                    data: serde_json_bytes::json!({"data": value.clone()}),
                                    warnings: Vec::new(),
                                    should_store: false,
                                }
                                .update_metadata(),
                            );
                        }

                        new_entities.push(value);
                    }
                }
            }
        }

        if !debug_ctx_entries.is_empty() {
            add_cache_keys_to_context(context, debug_ctx_entries.into_iter())?;
        }

        // Put the merged entities back into the response data
        if let Some(data_obj) = response_data.as_mut().and_then(|v| v.as_object_mut()) {
            data_obj.insert(ENTITIES, new_entities.into());
        }
        response.response.body_mut().data = response_data;

        // Update errors with reindexed paths (entity indices changed due to cache merge)
        if !new_errors.is_empty() {
            response.response.body_mut().errors = new_errors;
        }

        // Store new entities in cache asynchronously
        if !to_insert.is_empty() {
            let cache = storage.clone();
            let source = source_name.to_string();
            let span = tracing::info_span!(
                "response_cache.store",
                "kind" = "entity",
                "connector.source" = source_name,
                "ttl" = ?ttl,
                "batch.size" = to_insert.len()
            );
            tokio::spawn(async move {
                let _ = cache
                    .insert_in_batch(to_insert, &source)
                    .instrument(span)
                    .await;
            });
        }

        update_cache_control(context, &merged_cache_control);

        Ok(())
    }
}

// --- Connector Request Cache Service ---
// Handles root field cache lookup/store and Cache-Control extraction at the individual HTTP request level.

pub(super) type ConnectorRequestBoxCloneService = tower::util::BoxCloneService<
    connector::request_service::Request,
    connector::request_service::Response,
    BoxError,
>;

#[derive(Clone)]
pub(super) struct ConnectorRequestCacheService {
    pub(super) service: ConnectorRequestBoxCloneService,
    pub(super) storage: Arc<StorageInterface>,
    pub(super) source_name: String,
    pub(super) connector_ttl: Duration,
    pub(super) private_id_key: Option<String>,
    pub(super) debug: bool,
    pub(super) supergraph_schema: Arc<Valid<Schema>>,
    pub(super) subgraph_enums: Arc<HashMap<String, String>>,
    pub(super) private_queries: Arc<RwLock<LruCache<PrivateQueryKey, ()>>>,
    pub(super) lru_size_instrument: LruSizeInstrument,
    /// Resolved invalidation indexes for this connector source, gating which cache-tag layers
    /// are written on the store path (mirrors the subgraph path).
    pub(super) indexes: InvalidationIndexes,
}

impl Service<connector::request_service::Request> for ConnectorRequestCacheService {
    type Response = connector::request_service::Response;
    type Error = BoxError;
    type Future = <connector::request_service::BoxService as Service<
        connector::request_service::Request,
    >>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: connector::request_service::Request) -> Self::Future {
        let clone = self.clone();
        let inner = std::mem::replace(self, clone);

        Box::pin(inner.call_inner(request))
    }
}

impl ConnectorRequestCacheService {
    fn get_private_id(&self, context: &Context) -> Option<String> {
        hash_private_id(context, self.private_id_key.as_ref()?)
    }

    async fn call_inner(
        mut self,
        request: connector::request_service::Request,
    ) -> Result<connector::request_service::Response, BoxError> {
        // Check if the request is part of a batch. If it is, completely bypass response caching
        // since it will break any request batches which this request is part of.
        // This check is what enables Batching and response caching to work together, so be very
        // careful before making any changes to it.
        if request.is_part_of_batch() {
            return self.service.call(request).await;
        }

        // Skip cache entirely for non-Query operations (mutations, subscriptions)
        if let Ok(Some(operation_kind)) = request.context.get::<_, OperationKind>(OPERATION_KIND)
            && operation_kind != OperationKind::Query
        {
            return self.service.call(request).await;
        }

        // Gate debug mode on the per-request header, matching the subgraph path
        self.debug = self.debug
            && (request
                .supergraph_request
                .headers()
                .get(CACHE_DEBUG_HEADER_NAME)
                == Some(&HeaderValue::from_static("true")));

        let is_root_field = matches!(
            request.key,
            apollo_federation::connectors::runtime::key::ResponseKey::RootField { .. }
        );

        if is_root_field {
            self.handle_root_field(request).await
        } else {
            // Entity requests: just extract Cache-Control from the response
            self.handle_with_cache_control_extraction(request).await
        }
    }

    /// Handle root field requests with full cache lookup/store
    async fn handle_root_field(
        self,
        request: connector::request_service::Request,
    ) -> Result<connector::request_service::Response, BoxError> {
        let storage = match self.storage.get_connector(&self.source_name) {
            Some(s) => s.clone(),
            None => {
                record_fetch_error(&storage::Error::NoStorage, &self.source_name);
                return self.handle_with_cache_control_extraction(request).await;
            }
        };

        // [RFC 9111](https://datatracker.ietf.org/doc/html/rfc9111):
        //  * no-store: allows serving response from cache, but prohibits storing response in cache
        //  * no-cache: prohibits serving response from cache, but allows storing response in cache
        let request_cache_control = if request
            .supergraph_request
            .headers()
            .contains_key(&CACHE_CONTROL)
        {
            let cache_control = match CacheControl::try_from(request.supergraph_request.headers()) {
                Ok(cache_control) => cache_control,
                Err(err) => {
                    let message = format!("cannot get cache-control header: {err}");
                    let runtime_error =
                        apollo_federation::connectors::runtime::errors::RuntimeError::new(
                            &message,
                            &request.key,
                        )
                        .with_code("INVALID_CACHE_CONTROL_HEADER");
                    let subgraph_name = request.connector.id.subgraph_name.to_string();
                    return Ok(connector::request_service::Response {
                            context: request.context,
                            subgraph_name,
                            transport_result: Err(
                                apollo_federation::connectors::runtime::errors::Error::InvalidCacheControl(message),
                            ),
                            mapped_response:
                                apollo_federation::connectors::runtime::responses::MappedResponse::Error {
                                    error: runtime_error,
                                    key: request.key,
                                    problems: Vec::new(),
                                },
                        });
                }
            };

            // Don't use cache at all if both no-store and no-cache are set
            if cache_control.no_cache() && cache_control.no_store() {
                return self.handle_with_cache_control_extraction(request).await;
            }
            Some(cache_control)
        } else {
            None
        };

        let private_id = self.get_private_id(&request.context);

        // Build operation hash early — needed for both cache key and private query LRU key
        let operation_hash = request
            .operation
            .as_ref()
            .map(|op| hash_operation(&op.serialize().no_indent().to_string()))
            .unwrap_or_default();

        // Build private query key for LRU tracking
        let private_query_key = PrivateQueryKey {
            query_hash: operation_hash.clone(),
            has_private_id: private_id.is_some(),
        };

        let is_known_private = {
            self.private_queries
                .read()
                .await
                .contains(&private_query_key)
        };

        // Capture root field name before the private query bypass — needed for debug entries
        let root_field_name = match &request.key {
            apollo_federation::connectors::runtime::key::ResponseKey::RootField {
                name, ..
            } => name.clone(),
            _ => String::new(),
        };

        // The response will have a private scope but we don't have a way to differentiate users,
        // so we know we will not get or store anything in the cache
        if is_known_private && private_id.is_none() {
            let debug_request = if self.debug {
                let query_str = request
                    .operation
                    .as_ref()
                    .map(|op| op.serialize().no_indent().to_string())
                    .unwrap_or_default();
                Some(graphql::Request::builder().query(query_str).build())
            } else {
                None
            };

            let context = request.context.clone();
            let source_name = self.source_name.clone();
            let debug = self.debug;
            let indexes = self.indexes;
            let resp = self.handle_with_cache_control_extraction(request).await?;

            if debug {
                let cache_key_context = CacheKeyContext {
                    key: "-".to_string(),
                    invalidation_keys: vec![],
                    // TODO: connector debugger tag/CDN flags are undecided (pending router-team decision)
                    has_tags: false,
                    cdn_invalidation_enabled: false,
                    indexes,
                    kind: CacheEntryKind::RootFields {
                        root_fields: vec![root_field_name],
                    },
                    hashed_private_id: None,
                    subgraph_name: source_name,
                    subgraph_request: debug_request.unwrap_or_default(),
                    source: CacheKeySource::Connector,
                    cache_control: CacheControl::default_no_store(),
                    data: serde_json_bytes::Value::Null,
                    warnings: Vec::new(),
                    should_store: false,
                }
                .update_metadata();
                add_cache_key_to_context(&context, cache_key_context)?;
            }

            return Ok(resp);
        }

        // Get auth metadata from context
        let auth_metadata = request
            .context
            .extensions()
            .with_lock(|lock| lock.get::<CacheKeyMetadata>().cloned())
            .unwrap_or_default();

        // Capture connector info for cache tag extraction before request is consumed
        let connector_synthetic_name = request.connector.id.synthetic_name();

        // Snapshot the connector's referenced $context/$request.headers inputs for the cache key.
        let key_inputs = connector_key_inputs(
            Some(request.connector.as_ref()),
            &request.context,
            request.supergraph_request.headers(),
        );

        // Build a variables object from the request inputs for hashing
        let inputs = request.key.inputs();
        let cache_tag_args = inputs.args.clone();
        let mut variables = Object::default();
        for (k, v) in inputs.args.iter() {
            variables.insert(k.clone(), v.clone());
        }
        for (k, v) in inputs.this.iter() {
            variables.insert(k.clone(), v.clone());
        }
        let additional_data_hash = hash_connector_additional_data(
            &self.source_name,
            &variables,
            &request.context,
            &auth_metadata,
            &key_inputs,
        );

        let mut cache_key = ConnectorCacheKeyRoot {
            source_name: &self.source_name,
            graphql_type: "Query",
            operation_hash: &operation_hash,
            additional_data_hash: &additional_data_hash,
            private_id: if is_known_private {
                private_id.as_deref()
            } else {
                None
            },
        }
        .hash();

        // Build debug request and root fields list before request is consumed
        let debug_request = if self.debug {
            let query_str = request
                .operation
                .as_ref()
                .map(|op| op.serialize().no_indent().to_string())
                .unwrap_or_default();
            Some(
                graphql::Request::builder()
                    .query(query_str)
                    .variables(variables.into_iter().collect())
                    .build(),
            )
        } else {
            None
        };

        // Try cache lookup
        // Skip cache lookup if request had no-cache — we have no means of revalidating entries
        // without just performing the query, so there's no benefit to hitting the cache
        let skip_cache_lookup = request_cache_control.as_ref().is_some_and(|c| c.no_cache());

        let lookup_span = tracing::info_span!(
            "response_cache.lookup",
            kind = "root",
            "connector.source" = self.source_name.as_str(),
            debug = self.debug,
            private = is_known_private,
            contains_private_id = private_id.is_some(),
            "cache.key" = cache_key.as_str(),
        );

        // Don't touch storage at all when the client requested no-cache: the result could never
        // be served anyway, and the fetch would skew the lookup metrics.
        let fetch_result = if skip_cache_lookup {
            None
        } else {
            Some(
                storage
                    .fetch(&cache_key, &self.source_name)
                    .instrument(lookup_span.clone())
                    .await,
            )
        };

        // Mark span as error for non-trivial fetch failures
        if let Some(Err(ref err)) = fetch_result
            && !err.is_row_not_found()
        {
            lookup_span.mark_as_error(format!("cannot get cache entry: {err}"));
        }

        match fetch_result {
            Some(Ok(entry)) if entry.control.can_use() => {
                // Cache hit - build a response from cached data
                lookup_span.set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("hit".into()),
                );
                update_cache_control(&request.context, &entry.control);

                // Store cache hit metric in context for telemetry
                let mut cache_hit = HashMap::new();
                cache_hit.insert("Query".to_string(), CacheHitMiss { hit: 1, miss: 0 });
                let _ = request.context.insert(
                    CacheMetricContextKey::new(self.source_name.clone()),
                    CacheSubgraph(cache_hit),
                );

                if self.debug
                    && let Some(debug_req) = debug_request
                {
                    let cache_key_context = CacheKeyContext {
                        key: cache_key.clone(),
                        hashed_private_id: private_id,
                        invalidation_keys: entry
                            .invalidation_labels
                            .as_ref()
                            .map(|labels| labels.user_facing_only())
                            .unwrap_or_default(),
                        // TODO: connector debugger tag/CDN flags are undecided (pending router-team decision)
                        has_tags: false,
                        cdn_invalidation_enabled: false,
                        indexes: self.indexes,
                        kind: CacheEntryKind::RootFields {
                            root_fields: vec![root_field_name.clone()],
                        },
                        subgraph_name: self.source_name.clone(),
                        subgraph_request: debug_req,
                        source: CacheKeySource::Cache,
                        cache_control: entry.control.clone(),
                        data: serde_json_bytes::json!({"data": entry.data.clone()}),
                        warnings: Vec::new(),
                        should_store: false,
                    }
                    .update_metadata();
                    add_cache_key_to_context(&request.context, cache_key_context)?;
                }

                let subgraph_name = request.connector.id.subgraph_name.to_string();
                let cached_response = connector::request_service::Response {
                    context: request.context,
                    subgraph_name,
                    transport_result: Ok(
                        apollo_federation::connectors::runtime::http_json_transport::TransportResponse::CacheHit,
                    ),
                    mapped_response:
                        apollo_federation::connectors::runtime::responses::MappedResponse::Data {
                            data: entry.data,
                            key: request.key,
                            problems: Vec::new(),
                        },
                };

                Ok(cached_response)
            }
            _ => {
                // Cache miss - call inner service and cache the response
                lookup_span.set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("miss".into()),
                );
                let mut cache_miss = HashMap::new();
                cache_miss.insert("Query".to_string(), CacheHitMiss { hit: 0, miss: 1 });
                let _ = request.context.insert(
                    CacheMetricContextKey::new(self.source_name.clone()),
                    CacheSubgraph(cache_miss),
                );

                let debug = self.debug;
                let context = request.context.clone();
                let source_name = self.source_name.clone();
                let connector_ttl = self.connector_ttl;
                let supergraph_schema = self.supergraph_schema.clone();
                let subgraph_enums = self.subgraph_enums.clone();
                let private_queries = self.private_queries.clone();
                let lru_size_instrument = self.lru_size_instrument.clone();
                let indexes = self.indexes;
                let response = self.handle_with_cache_control_extraction(request).await?;

                // Store in cache if appropriate
                if let apollo_federation::connectors::runtime::responses::MappedResponse::Data {
                    ref data,
                    ..
                } = response.mapped_response
                {
                    // Base the store decision on THIS response's own Cache-Control (parsed
                    // straight from the transport response), never on the request-wide
                    // aggregate, which merges unrelated fetches and would let them poison
                    // this entry's storability/TTL/privacy.
                    let mut cache_control =
                        connector_response_cache_control(&response, connector_ttl)
                            .unwrap_or_else(CacheControl::default_no_store);

                    // If the request had no-store, propagate that to the response cache control
                    if let Some(ref req_cc) = request_cache_control {
                        cache_control.merge_no_store(req_cc);
                    }

                    // Track private queries in the LRU so future requests can short-circuit
                    if cache_control.private() && !is_known_private {
                        let size = {
                            let mut pq = private_queries.write().await;
                            pq.put(private_query_key, ());
                            pq.len()
                        };
                        lru_size_instrument.update(size as u64);

                        // Update cache key with private_id suffix now that we know the
                        // response is private (matching subgraph pattern at line 1278)
                        if let Some(ref s) = private_id {
                            cache_key = format!("{cache_key}:{s}");
                        }
                    }

                    // The response has a private scope but we don't have a way to differentiate
                    // users, so we do not store the response in cache
                    let unstorable_private_response =
                        cache_control.private() && private_id.is_none();

                    if !unstorable_private_response && cache_control.should_store() {
                        let ttl = cache_control
                            .ttl()
                            .map(Duration::from_secs)
                            .unwrap_or(connector_ttl);

                        // Build typed cache tags for the root document, gated on the connector's
                        // resolved invalidation indexes (mirrors dev's subgraph root path). The
                        // root type tag is `Type("Query")`; `Subgraph` is prepended at store time.
                        let mut cache_tags: Vec<CacheTag> = Vec::new();
                        if indexes.is_enabled(IndexMode::Type) {
                            cache_tags.push(CacheTag::Type("Query".to_string()));
                        }
                        // Extract @cacheTag invalidation keys from the supergraph schema
                        if indexes.is_enabled(IndexMode::CacheTag)
                            && let Ok(cache_tag_keys) = get_connector_root_cache_tags(
                                &supergraph_schema,
                                &subgraph_enums,
                                &connector_synthetic_name,
                                &root_field_name,
                                &cache_tag_args,
                            )
                        {
                            cache_tags.extend(cache_tag_keys.into_iter().map(CacheTag::Tag));
                        }

                        if debug && let Some(debug_req) = debug_request {
                            let cache_key_context = CacheKeyContext {
                                key: cache_key.clone(),
                                hashed_private_id: private_id,
                                invalidation_keys: cache_tags
                                    .iter()
                                    .filter_map(|t| t.user_value().map(String::from))
                                    .collect(),
                                // TODO: connector debugger tag/CDN flags are undecided (pending router-team decision)
                                has_tags: false,
                                cdn_invalidation_enabled: false,
                                indexes,
                                kind: CacheEntryKind::RootFields {
                                    root_fields: vec![root_field_name.clone()],
                                },
                                subgraph_name: source_name.clone(),
                                subgraph_request: debug_req,
                                source: CacheKeySource::Connector,
                                cache_control: cache_control.clone(),
                                data: serde_json_bytes::json!({"data": data.clone()}),
                                warnings: Vec::new(),
                                should_store: false,
                            }
                            .update_metadata();
                            add_cache_key_to_context(&context, cache_key_context)?;
                        }

                        // Prepend the whole-source (`Subgraph`) tag at store time, gated on the
                        // resolved invalidation indexes.
                        if indexes.is_enabled(IndexMode::Subgraph) {
                            cache_tags.insert(0, CacheTag::Subgraph);
                        }

                        let document = Document {
                            key: cache_key,
                            data: data.clone(),
                            control: cache_control,
                            cache_tags,
                            expire: ttl,
                            // TODO: connector CDN cache-tag population is undecided (pending router-team decision)
                            cdn_invalidation_tags: Vec::new(),
                            scope: CacheScope::Connector,
                        };

                        let source = source_name;
                        let span = tracing::info_span!(
                            "response_cache.store",
                            "kind" = "root",
                            "connector.source" = source.as_str(),
                            "ttl" = ?ttl
                        );
                        tokio::spawn(async move {
                            let _ = storage.insert(document, &source).instrument(span).await;
                        });
                    }
                }

                Ok(response)
            }
        }
    }

    /// Pass through to inner service, extracting Cache-Control from the transport response
    async fn handle_with_cache_control_extraction(
        mut self,
        request: connector::request_service::Request,
    ) -> Result<connector::request_service::Response, BoxError> {
        let context = request.context.clone();
        let connector_synthetic_name = request.connector.id.synthetic_name();
        let response = self.service.call(request).await?;

        // Extract Cache-Control from the transport response headers
        if let Some(cache_control) = connector_response_cache_control(&response, self.connector_ttl)
        {
            // Merge into the request-wide aggregate used for the client-facing Cache-Control
            // header ...
            update_cache_control(&context, &cache_control);
            // ... and record it under this connector's identity, so store decisions (root and
            // entity paths) only ever consult cache-control from THIS connector's own upstream
            // responses, never from unrelated fetches in the same client request.
            record_connector_cache_control(&context, &connector_synthetic_name, &cache_control);
        }

        Ok(response)
    }
}

/// Per-connector cache-control accumulator, stored in context extensions.
///
/// `update_cache_control` maintains a single request-wide aggregate for the client-facing header,
/// which merges every fetch (subgraph and connector) in the request. Basing *store* decisions on
/// that aggregate would let unrelated fetches (e.g. a headerless subgraph response) poison a
/// connector's storability/TTL/privacy. This map keeps a separate most-restrictive merge per
/// connector (keyed by the connector's synthetic name), fed only by that connector's own HTTP
/// responses. Multiple HTTP requests fanned out for one entity batch merge together here (min
/// TTL across the batch), which is the intended semantics.
#[derive(Default)]
struct ConnectorCacheControls(HashMap<String, CacheControl>);

fn record_connector_cache_control(
    context: &Context,
    connector_synthetic_name: &str,
    cache_control: &CacheControl,
) {
    context.extensions().with_lock(|lock| {
        let map = lock.get_or_default_mut::<ConnectorCacheControls>();
        match map.0.get_mut(connector_synthetic_name) {
            Some(existing) => *existing = existing.merge(cache_control),
            None => {
                map.0
                    .insert(connector_synthetic_name.to_string(), cache_control.clone());
            }
        }
    });
}

fn get_connector_cache_control(
    context: &Context,
    connector_synthetic_name: &str,
) -> Option<CacheControl> {
    context.extensions().with_lock(|lock| {
        lock.get::<ConnectorCacheControls>()
            .and_then(|map| map.0.get(connector_synthetic_name).cloned())
    })
}

/// Compute the cache-control for a single connector HTTP response.
///
/// Returns `None` when there was no HTTP response (transport error, mapping-only, cache hit).
///
/// Deliberate divergence from the subgraph path (documented in the response-caching docs): a
/// response with **no** `Cache-Control` header is cacheable with the configured TTL, instead of
/// being treated as no-store. A *present but unparseable* header still falls back to no-store.
fn connector_response_cache_control(
    response: &connector::request_service::Response,
    connector_ttl: Duration,
) -> Option<CacheControl> {
    let Ok(apollo_federation::connectors::runtime::http_json_transport::TransportResponse::Http(
        http_response,
    )) = &response.transport_result
    else {
        return None;
    };

    let cache_control = if http_response.inner.headers.contains_key(CACHE_CONTROL) {
        CacheControl::try_from(&http_response.inner.headers)
            .unwrap_or_else(|_| CacheControl::default_no_store())
            .with_default_ttl(Some(connector_ttl))
    } else {
        // No Cache-Control header at all: use the configured TTL as the fallback.
        CacheControl::default().with_default_ttl(Some(connector_ttl))
    };
    Some(cache_control)
}

/// Extract `@cacheTag` invalidation keys for a connector root field from the supergraph schema.
///
/// Looks for `@join__directive(name: "federation__cacheTag")` on the given root field,
/// filters by `graphs` matching the connector's synthetic subgraph name, and interpolates
/// the format template with the field's `$args`.
fn get_connector_root_cache_tags(
    supergraph_schema: &Valid<Schema>,
    subgraph_enums: &HashMap<String, String>,
    connector_synthetic_name: &str,
    field_name: &str,
    args: &serde_json_bytes::Map<ByteString, Value>,
) -> Result<HashSet<String>, anyhow::Error> {
    let root_query_type = supergraph_schema
        .root_operation(apollo_compiler::ast::OperationType::Query)
        .ok_or_else(|| anyhow::anyhow!("no root query type in supergraph schema"))?;
    let query_object_type = supergraph_schema
        .get_object(root_query_type.as_str())
        .ok_or_else(|| anyhow::anyhow!("cannot get root query type from supergraph schema"))?;
    let field_def = match query_object_type.fields.get(field_name) {
        Some(f) => f,
        None => return Ok(HashSet::new()),
    };

    let templates = field_def
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
                                    == Some(connector_synthetic_name)
                            }),
                    )
                })
                .unwrap_or_default();
            if !is_current_subgraph {
                return None;
            }
            let mut format = None;
            for (field_name, value) in dir
                .argument_by_name("args", supergraph_schema)
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
    vars.insert("$args".to_string(), Value::Object(args.clone()));

    let mut keys = HashSet::new();
    for template in templates {
        match template.interpolate(&vars) {
            Ok((key, _)) => {
                keys.insert(key);
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to interpolate @cacheTag format for connector root field");
            }
        }
    }
    Ok(keys)
}

struct CacheMetadata {
    cache_key: String,
    cache_tags: Vec<CacheTag>,
    // Only set when debug mode is enabled
    entity_key: Option<serde_json_bytes::Map<ByteString, Value>>,
}

/// Build a stable, sorted snapshot of the connector's *used* per-request inputs — the `$context`
/// keys and client request headers that shape the upstream HTTP request or the response mapping —
/// for inclusion in the cache key via [`hash_connector_additional_data`].
///
/// Client headers reach the upstream request through TWO distinct routes, and both must be keyed:
/// 1. **Interpolation**: `{$request.headers.x}` in URL/body/header-value templates — collected via
///    `Connector::{request,response}_headers` (variable references).
/// 2. **Forwarding**: `http: {headers: [{name: ..., from: "x"}]}` — no variable reference exists,
///    so the `from:` client header names are collected from the transport's header list directly.
///
/// Deliberately excluded (deployment-static, identical for every request): static header `value:`
/// templates without variable references, `$config`, and `$env`. A `value:` template that DOES
/// reference `$context`/`$request` is covered by route 1.
///
/// Only used inputs are included, so unreferenced/unforwarded headers never over-partition the
/// cache. A connector that uses nothing here yields an empty object.
///
/// Without this, two client requests that produce *different* upstream connector requests because
/// a forwarded header or context value differs would resolve to the SAME cache key — a cross-user
/// data leak.
fn connector_key_inputs(
    connector: Option<&apollo_federation::connectors::Connector>,
    context: &Context,
    headers: &http::HeaderMap,
) -> Value {
    use apollo_federation::connectors::HeaderSource;
    use apollo_federation::connectors::Namespace;

    let Some(connector) = connector else {
        return Value::Object(Default::default());
    };

    // Referenced $context keys (union of request- and response-mapping references). A referenced
    // context key changes either what is fetched or how the response maps, so both matter.
    let mut context_keys: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for map in [
        &connector.request_variable_keys,
        &connector.response_variable_keys,
    ] {
        if let Some(keys) = map.get(&Namespace::Context) {
            context_keys.extend(keys.iter().map(|s| s.as_str()));
        }
    }
    let mut context_obj = serde_json_bytes::Map::new();
    for key in context_keys {
        let value = context.get_json_value(key).unwrap_or(Value::Null);
        context_obj.insert(ByteString::from(key), value);
    }

    // Used client-request headers by name (case-insensitive; all values, sorted):
    // interpolated (`$request.headers.*` references) ...
    let mut header_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    header_names.extend(connector.request_headers.iter().cloned());
    header_names.extend(connector.response_headers.iter().cloned());
    // ... plus forwarded (`headers: [{from: ...}]`); the upstream request copies the CLIENT
    // header named by `from`, so that is the name whose values must partition the key.
    if let Some(transport) = connector.transport.as_ref() {
        for header in &transport.headers {
            if let HeaderSource::From(from) = &header.source {
                header_names.insert(from.as_str().to_string());
            }
        }
    }
    let mut headers_obj = serde_json_bytes::Map::new();
    for name in &header_names {
        let mut values: Vec<String> = headers
            .get_all(name.as_str())
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_string))
            .collect();
        values.sort();
        headers_obj.insert(
            ByteString::from(name.as_str()),
            Value::Array(
                values
                    .into_iter()
                    .map(|v| Value::String(v.into()))
                    .collect(),
            ),
        );
    }

    let mut root = serde_json_bytes::Map::new();
    root.insert(ByteString::from("context"), Value::Object(context_obj));
    root.insert(ByteString::from("headers"), Value::Object(headers_obj));
    Value::Object(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_connector(
        transport_headers: Vec<apollo_federation::connectors::Header>,
        interpolated_request_headers: &[&str],
    ) -> apollo_federation::connectors::Connector {
        use apollo_compiler::name;
        use apollo_federation::connectors::ConnectId;
        use apollo_federation::connectors::ConnectSpec;
        use apollo_federation::connectors::Connector;
        use apollo_federation::connectors::HttpJsonTransport;
        use apollo_federation::connectors::JSONSelection;

        let schema =
            apollo_compiler::Schema::parse_and_validate("type Query { b: String }", "./").unwrap();
        Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Connector::subtypes_map_from_schema(&schema),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(b),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                headers: transport_headers,
                ..Default::default()
            }),
            selection: JSONSelection::parse("$").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: interpolated_request_headers
                .iter()
                .map(|s| s.to_string())
                .collect(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            label: "test label".into(),
        }
    }

    /// F9 regression: forwarded headers (`headers: [{from: ...}]`) must contribute to the key
    /// inputs; interpolated (`$request.headers.*`) headers must too; static `value:` headers and
    /// unrelated request headers must NOT (no over-partitioning).
    #[test]
    fn key_inputs_cover_forwarded_and_interpolated_headers_only() {
        use apollo_federation::connectors::Header;
        use apollo_federation::connectors::HeaderSource;
        use apollo_federation::connectors::OriginatingDirective;

        let connector = test_connector(
            vec![
                // forwarded: upstream name differs from the client (`from`) name on purpose —
                // the CLIENT name is what must be keyed
                Header::from_values(
                    "x-upstream-user".parse().unwrap(),
                    HeaderSource::From("x-user".parse().unwrap()),
                    OriginatingDirective::Connect,
                ),
                // static value: deployment-static, must NOT be keyed
                Header::from_values(
                    "x-static".parse().unwrap(),
                    HeaderSource::Value(
                        apollo_federation::connectors::header::HeaderValue::parse_with_spec(
                            "fixed",
                            apollo_federation::connectors::ConnectSpec::V0_1,
                        )
                        .unwrap(),
                    ),
                    OriginatingDirective::Source,
                ),
            ],
            &["x-lang"], // interpolated via {$request.headers.x-lang}
        );

        let context = Context::new();
        let mut headers = http::HeaderMap::new();
        headers.insert("x-user", http::HeaderValue::from_static("alice"));
        headers.insert("x-lang", http::HeaderValue::from_static("fr"));
        headers.insert("x-unrelated", http::HeaderValue::from_static("noise"));
        headers.insert("x-static", http::HeaderValue::from_static("client-noise"));

        let inputs = connector_key_inputs(Some(&connector), &context, &headers);
        let headers_obj = inputs
            .as_object()
            .unwrap()
            .get("headers")
            .unwrap()
            .as_object()
            .unwrap();

        assert!(
            headers_obj.contains_key("x-user"),
            "forwarded header (from:) must be keyed, got: {inputs:?}"
        );
        assert!(
            headers_obj.contains_key("x-lang"),
            "interpolated header must be keyed, got: {inputs:?}"
        );
        assert!(
            !headers_obj.contains_key("x-unrelated"),
            "unrelated header must not over-partition, got: {inputs:?}"
        );
        assert!(
            !headers_obj.contains_key("x-static"),
            "static value: header must not be keyed, got: {inputs:?}"
        );
        assert!(
            !headers_obj.contains_key("x-upstream-user"),
            "the upstream rename must not be keyed — the client `from` name is, got: {inputs:?}"
        );

        // Different forwarded value ⇒ different snapshot; unrelated header change ⇒ identical.
        let mut headers_bob = headers.clone();
        headers_bob.insert("x-user", http::HeaderValue::from_static("bob"));
        assert_ne!(
            inputs,
            connector_key_inputs(Some(&connector), &context, &headers_bob)
        );
        let mut headers_noise = headers.clone();
        headers_noise.insert("x-unrelated", http::HeaderValue::from_static("other"));
        assert_eq!(
            inputs,
            connector_key_inputs(Some(&connector), &context, &headers_noise)
        );
    }

    fn config_with(
        all_enabled: Option<bool>,
        sources: Vec<(&str, Option<bool>)>,
    ) -> ConnectorCacheConfiguration {
        let mut source_map = HashMap::new();
        for (name, enabled) in sources {
            source_map.insert(
                name.to_string(),
                ConnectorCacheSource {
                    enabled,
                    ..Default::default()
                },
            );
        }
        ConnectorCacheConfiguration {
            all: ConnectorCacheSource {
                enabled: all_enabled,
                ..Default::default()
            },
            sources: source_map,
        }
    }

    #[test]
    fn plugin_disabled_returns_false() {
        let config = config_with(None, vec![]);
        assert!(!config.is_source_enabled(false, "any"));
    }

    #[test]
    fn default_enabled_when_plugin_enabled() {
        let config = config_with(None, vec![]);
        assert!(config.is_source_enabled(true, "any"));
    }

    #[test]
    fn all_explicitly_enabled() {
        let config = config_with(Some(true), vec![]);
        assert!(config.is_source_enabled(true, "any"));
    }

    #[test]
    fn all_explicitly_disabled() {
        let config = config_with(Some(false), vec![]);
        assert!(!config.is_source_enabled(true, "any"));
    }

    #[test]
    fn per_source_true_overrides_all_disabled() {
        let config = config_with(Some(false), vec![("src", Some(true))]);
        assert!(config.is_source_enabled(true, "src"));
    }

    #[test]
    fn per_source_false_overrides_all_enabled() {
        let config = config_with(Some(true), vec![("src", Some(false))]);
        assert!(!config.is_source_enabled(true, "src"));
    }

    #[test]
    fn plugin_disabled_overrides_per_source_true() {
        let config = config_with(None, vec![("src", Some(true))]);
        assert!(!config.is_source_enabled(false, "src"));
    }

    #[test]
    fn unknown_source_uses_all_defaults() {
        let config = config_with(None, vec![("known", Some(false))]);
        assert!(config.is_source_enabled(true, "unknown"));
    }
}
