use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::StringTemplate;
use apollo_federation::connectors::runtime::cache::CacheKeyComponents;
use apollo_federation::connectors::runtime::cache::CacheableDetails;
use apollo_federation::connectors::runtime::cache::CacheableItem;
use futures::future::BoxFuture;
use http::header::CACHE_CONTROL;
use indexmap::IndexMap;
use itertools::Itertools;
use lru::LruCache;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tower::BoxError;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;
use tracing::Span;

use super::cache_control::CacheControl;
use super::plugin::RESPONSE_CACHE_VERSION;
use super::postgres::CacheEntry;
use super::postgres::PostgresCacheStorage;
use crate::Context;
use crate::batching::BatchQuery;
use crate::error::FetchError;
use crate::graphql;
use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::cache_key::ConnectorPrimaryCacheKey;
use crate::plugins::response_cache::plugin::CACHE_TAG_DIRECTIVE_NAME;
use crate::plugins::response_cache::plugin::CONTEXT_DEBUG_CACHE_KEYS;
use crate::plugins::response_cache::plugin::CacheEntryKind;
use crate::plugins::response_cache::plugin::CacheKeyContext;
use crate::plugins::response_cache::plugin::CacheKeySource;
use crate::plugins::response_cache::plugin::CacheKeysContext;
use crate::plugins::response_cache::plugin::INTERNAL_CACHE_TAG_PREFIX;
use crate::plugins::response_cache::plugin::IsDebug;
use crate::plugins::response_cache::plugin::PrivateQueryKey;
use crate::plugins::response_cache::plugin::Storage;
use crate::plugins::response_cache::plugin::update_cache_control;
use crate::plugins::telemetry::LruSizeInstrument;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::span_ext::SpanMarkError;
use crate::services;
use crate::services::connect;
use crate::spec::TYPENAME;

#[derive(Clone)]
pub(super) struct ConnectorCacheService {
    pub(super) service: services::connect::BoxCloneService,
    pub(super) name: String,
    pub(super) storage: Arc<Storage>,
    pub(super) subgraph_ttl: Duration,
    pub(super) debug: bool,
    pub(super) supergraph_schema: Arc<Valid<Schema>>,
    pub(super) subgraph_enums: Arc<HashMap<String, String>>,
    pub(super) private_queries: Arc<RwLock<LruCache<PrivateQueryKey, ()>>>,
    pub(super) private_id: Option<String>,
    pub(super) lru_size_instrument: LruSizeInstrument,
}

impl Service<services::connect::Request> for ConnectorCacheService {
    type Response = services::connect::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: services::connect::Request) -> Self::Future {
        let clone = self.clone();
        let inner = std::mem::replace(self, clone);

        Box::pin(inner.call_inner(request))
    }
}

impl ConnectorCacheService {
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
    async fn call_inner(
        mut self,
        mut request: services::connect::Request,
    ) -> Result<services::connect::Response, BoxError> {
        let subgraph_name = self.name.clone();
        let context = request.context.clone();
        let storage = match self.storage.get(&self.name) {
            Some(storage) => storage.clone(),
            None => {
                u64_counter_with_unit!(
                    "apollo.router.operations.response_cache.fetch.error",
                    "Errors when fetching data from cache",
                    "{error}",
                    1,
                    "subgraph.name" = self.name.clone(),
                    "code" = "NO_STORAGE"
                );
                return self
                    .service
                    .map_response(move |response: services::connect::Response| {
                        update_cache_control(
                            &context,
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
            && request
                .context
                .extensions()
                .with_lock(|l| l.contains_key::<IsDebug>());
        //
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
        let mut cache_control_no_store = None;
        for prepared_request in &request.prepared_requests {
            if let Some(http_req) = prepared_request.transport_request.as_http()
                && http_req.inner.headers().contains_key(CACHE_CONTROL)
            {
                match CacheControl::new(http_req.inner.headers(), None) {
                    Ok(cache_control) => {
                        if cache_control.no_store {
                            cache_control_no_store = Some(cache_control);
                            break;
                        }
                    }
                    Err(err) => {
                        return Ok(connect::Response::new(
                            context,
                            http::Response::new(
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
                            ),
                            Default::default(),
                            None,
                        ));
                    }
                }
            }
        }

        if let Some(cache_control) = cache_control_no_store {
            let mut resp = self.service.call(request).await?;
            cache_control.to_headers(resp.response.headers_mut())?;
            return Ok(resp);
        }

        let private_id = self.get_private_id(&request.context);
        let mut hasher = blake3::Hasher::new();
        hasher.update(
            request
                .cacheable_items()
                .map(|(_c, cache_key)| cache_key.to_string())
                .join("/")
                .as_bytes(),
        );
        // Knowing if there's a private_id or not will differentiate the hash because for a same query it can be both public and private depending if we have private_id set or not
        let private_query_key = PrivateQueryKey {
            query_hash: hasher.finalize().to_hex().to_string(),
            has_private_id: private_id.is_some(),
        };

        let is_known_private = {
            self.private_queries
                .read()
                .await
                .contains(&private_query_key)
        };

        let cacheable_items = request.cacheable_items();

        // the response will have a private scope but we don't have a way to differentiate users, so we know we will not get or store anything in the cache
        if is_known_private && private_id.is_none() {
            let mut resp = self.service.call(request).await?;
            if self.debug
                && let Some(cacheable_items) = resp.cacheable_items()
            {
                let mut debug_cache_keys = Vec::with_capacity(cacheable_items.len());
                for (cacheable_item, cacheable_details) in cacheable_items {
                    let cache_control = CacheControl::new(&cacheable_details.policies, None)?;
                    let kind = match &cacheable_item {
                        CacheableItem::RootFields { output_names, .. } => {
                            CacheEntryKind::RootFields {
                                root_fields: output_names.clone(),
                            }
                        }
                        CacheableItem::Entity {
                            output_type,
                            surrogate_key_data,
                            ..
                        }
                        | CacheableItem::BatchItem {
                            output_type,
                            surrogate_key_data,
                            ..
                        } => {
                            let mut entity_key = surrogate_key_data.clone();
                            entity_key.remove(&ByteString::from(TYPENAME));
                            CacheEntryKind::Entity {
                                typename: output_type.to_string(),
                                entity_key,
                            }
                        }
                    };
                    debug_cache_keys.push(CacheKeyContext {
                        key: "-".to_string(),
                        invalidation_keys: vec![],
                        kind,
                        hashed_private_id: private_id.clone(),
                        subgraph_name: self.name.clone(),
                        subgraph_request: None,
                        connector_request: cacheable_details
                            .cache_key_components
                            .bodies
                            .first()
                            .cloned(),
                        source: CacheKeySource::Connector,
                        cache_control,
                        data: cacheable_details.response(),
                    });
                }

                resp.context.upsert::<_, CacheKeysContext>(
                    CONTEXT_DEBUG_CACHE_KEYS,
                    |mut val| {
                        val.extend(debug_cache_keys);
                        val
                    },
                )?;
            }

            return Ok(resp);
        }

        let mut cached: HashMap<CacheableItem, CacheEntry> = Default::default();
        let mut uncached: HashMap<CacheableItem, (String, Vec<String>)> = Default::default();
        let mut futs: JoinSet<Result<_, BoxError>> = JoinSet::new();
        for (cacheable_item, cache_key_components) in cacheable_items {
            if let CacheableItem::RootFields { operation_type, .. } = cacheable_item
                && operation_type != OperationType::Query
            {
                // Not a query we don't cache this
                return self.service.call(request).await;
            }

            let context = request.context.clone();
            let supergraph_schema = self.supergraph_schema.clone();
            let subgraph_enums = self.subgraph_enums.clone();
            let debug = self.debug;
            let storage = storage.clone();
            let subgraph_name = subgraph_name.clone();
            let private_id = private_id.clone();
            futs.spawn(async move {
                cache_lookup_connector(
                    &subgraph_name,
                    storage,
                    is_known_private,
                    private_id.as_deref(),
                    debug,
                    &context,
                    cacheable_item,
                    cache_key_components,
                    supergraph_schema,
                    &subgraph_enums,
                )
                .await
            });
        }
        let results = futs.join_all().await;
        for result in results {
            let result = result?;
            match result {
                ControlFlow::Continue((cacheable_item, primary_cache_key, invalidation_keys)) => {
                    uncached.insert(cacheable_item, (primary_cache_key, invalidation_keys));
                }
                ControlFlow::Break((cacheable_item, cache_entry)) => {
                    cached.insert(cacheable_item, cache_entry);
                }
            }
        }
        let (cached_items, cache_entries): (Vec<_>, Vec<_>) = cached.into_iter().unzip();
        request.remove_cacheable_items(&cached_items);

        let mut response = self.service.call(request).await?;
        let mut insert_private_query = false;
        if response.response.body().errors.is_empty()
            && let Some(cacheable_items) = response.cacheable_items()
        {
            for (cacheable_item, response_details) in cacheable_items {
                let cache_control = if response_details.policies.contains_key(CACHE_CONTROL) {
                    CacheControl::new(&response_details.policies, self.subgraph_ttl.into())?
                } else {
                    CacheControl::no_store()
                };
                let (primary_cache_key, invalidation_keys) = match uncached.remove(&cacheable_item)
                {
                    Some(found) => found,
                    None => {
                        ::tracing::error!(
                            "cannot find primary cache key and invalidation keys previously built. This is a bug, please contact the support."
                        );
                        continue;
                    }
                };

                if self.debug {
                    let kind = match &cacheable_item {
                        CacheableItem::RootFields { output_names, .. } => {
                            CacheEntryKind::RootFields {
                                root_fields: output_names.clone(),
                            }
                        }
                        CacheableItem::Entity {
                            output_type,
                            surrogate_key_data,
                            ..
                        }
                        | CacheableItem::BatchItem {
                            output_type,
                            surrogate_key_data,
                            ..
                        } => {
                            let mut entity_key = surrogate_key_data.clone();
                            entity_key.remove(&ByteString::from(TYPENAME));
                            CacheEntryKind::Entity {
                                typename: output_type.to_string(),
                                entity_key,
                            }
                        }
                    };

                    context.upsert::<_, CacheKeysContext>(
                        CONTEXT_DEBUG_CACHE_KEYS,
                        |mut val| {
                            val.push(CacheKeyContext {
                                key: primary_cache_key.clone(),
                                hashed_private_id: private_id.clone(),
                                invalidation_keys: invalidation_keys
                                    .clone()
                                    .into_iter()
                                    .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                                    .collect(),
                                kind,
                                subgraph_name: self.name.clone(),
                                subgraph_request: None,
                                source: CacheKeySource::Connector,
                                cache_control: cache_control.clone(),
                                data: response_details.response().clone(),
                                connector_request: response_details
                                    .cache_key_components
                                    .bodies
                                    .first()
                                    .cloned(),
                            });

                            val
                        },
                    )?;
                }

                if !is_known_private && cache_control.private() {
                    insert_private_query = true;
                }

                if cache_control.should_store() {
                    cache_store_from_response(
                        storage.clone(),
                        self.subgraph_ttl,
                        &subgraph_name,
                        cacheable_item,
                        response_details,
                        cache_control,
                        primary_cache_key,
                        invalidation_keys,
                    )?;
                }
            }
        }
        if insert_private_query {
            let size = {
                let mut private_queries = self.private_queries.write().await;
                private_queries.put(private_query_key, ());
                private_queries.len()
            };
            self.lru_size_instrument.update(size as u64);
        }

        let mut cache_control: Option<CacheControl> = None;
        cached_items
            .into_iter()
            .zip(cache_entries)
            .for_each(|(cacheable_item, cache_entry)| {
                match &mut cache_control {
                    Some(global_cache_control) => {
                        *global_cache_control = global_cache_control.merge(&cache_entry.control);
                    }
                    None => cache_control = Some(cache_entry.control),
                }
                response.add_cached_data(&cacheable_item, cache_entry.data);
            });
        if let Some(cache_control) = cache_control {
            update_cache_control(&context, &cache_control);
        }

        Ok(response)
    }
}

#[allow(clippy::too_many_arguments)]
fn cache_store_from_response(
    cache: PostgresCacheStorage,
    default_subgraph_ttl: Duration,
    subgraph_name: &str,
    cacheable_item: CacheableItem,
    cacheable_details: CacheableDetails<'_>,
    cache_control: CacheControl,
    cache_key: String,
    invalidation_keys: Vec<String>,
) -> Result<(), BoxError> {
    let ttl = cache_control
        .ttl()
        .map(|secs| Duration::from_secs(secs as u64))
        .unwrap_or(default_subgraph_ttl);

    let data = cacheable_details.response();
    // Support surrogate keys coming from subgraph response extensions
    // if let Some(Value::Array(cache_tags)) = response
    //     .response
    //     .body()
    //     .extensions
    //     .get(GRAPHQL_RESPONSE_EXTENSION_ROOT_FIELDS_CACHE_TAGS)
    // {
    //     invalidation_keys.extend(
    //         cache_tags
    //             .iter()
    //             .filter_map(|v| v.as_str())
    //             .map(|s| s.to_owned()),
    //     );
    // }
    let data = data.clone();
    let kind = if cacheable_item.is_entity() {
        "entity"
    } else {
        "root"
    };
    let span = tracing::info_span!("response_cache.store", "type" = "connector", "kind" = kind, "subgraph.name" = subgraph_name.to_string(), "ttl" = ?ttl);
    let subgraph_name = subgraph_name.to_string();
    // Write to cache in a non-awaited task so it’s on in the request’s critical path
    tokio::spawn(async move {
        let now = Instant::now();
        if let Err(err) = cache
            .insert(
                &cache_key,
                ttl,
                invalidation_keys,
                data,
                cache_control,
                &subgraph_name,
            )
            .instrument(span)
            .await
        {
            u64_counter_with_unit!(
                "apollo.router.operations.response_cache.insert.error",
                "Errors when inserting data in cache",
                "{error}",
                1,
                "subgraph.name" = subgraph_name.clone(),
                "code" = err.code()
            );
            tracing::debug!(error = %err, "cannot insert data in cache");
        }
        f64_histogram_with_unit!(
            "apollo.router.operations.response_cache.insert",
            "Time to insert new data in cache",
            "s",
            now.elapsed().as_secs_f64(),
            "subgraph.name" = subgraph_name,
            "kind" = "single"
        );
    });

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cache_lookup_connector(
    name: &str,
    cache: PostgresCacheStorage,
    is_known_private: bool,
    private_id: Option<&str>,
    debug: bool,
    context: &Context,
    cacheable_item: CacheableItem,
    cache_key_components: CacheKeyComponents,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
) -> Result<ControlFlow<(CacheableItem, CacheEntry), (CacheableItem, String, Vec<String>)>, BoxError>
{
    let invalidation_cache_keys = get_invalidation_keys_from_schema(
        &cacheable_item,
        &cache_key_components,
        subgraph_enums,
        &supergraph_schema,
    )?;
    let (key, mut invalidation_keys) = extract_cache_key_root(
        name,
        &cacheable_item,
        &cache_key_components,
        context,
        is_known_private,
        private_id,
    );
    invalidation_keys.extend(invalidation_cache_keys);

    let now = Instant::now();
    let cache_result = cache.get(&key).await;
    f64_histogram_with_unit!(
        "apollo.router.operations.response_cache.fetch",
        "Time to fetch data from cache",
        "s",
        now.elapsed().as_secs_f64(),
        "subgraph.name" = name.to_string(),
        "kind" = "single"
    );

    match cache_result {
        Ok(value) => {
            if value.control.can_use() {
                let control = value.control.clone();
                update_cache_control(context, &control);
                if debug {
                    let kind = match &cacheable_item {
                        CacheableItem::RootFields { output_names, .. } => {
                            CacheEntryKind::RootFields {
                                root_fields: output_names.clone(),
                            }
                        }
                        CacheableItem::Entity {
                            output_type,
                            surrogate_key_data,
                            ..
                        }
                        | CacheableItem::BatchItem {
                            output_type,
                            surrogate_key_data,
                            ..
                        } => {
                            let mut entity_key = surrogate_key_data.clone();
                            entity_key.remove(&ByteString::from(TYPENAME));
                            CacheEntryKind::Entity {
                                typename: output_type.to_string(),
                                entity_key,
                            }
                        }
                    };

                    context.upsert::<_, CacheKeysContext>(
                        CONTEXT_DEBUG_CACHE_KEYS,
                        |mut val| {
                            val.push(CacheKeyContext {
                                key: value.cache_key.clone(),
                                hashed_private_id: private_id.map(ToString::to_string),
                                invalidation_keys: invalidation_keys
                                    .clone()
                                    .into_iter()
                                    .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                                    .collect(),
                                kind,
                                subgraph_name: name.to_string(),
                                subgraph_request: None,
                                source: CacheKeySource::Cache,
                                cache_control: value.control.clone(),
                                data: serde_json_bytes::json!({"data": value.data.clone()}),
                                connector_request: cache_key_components.bodies.first().cloned(),
                            });

                            val
                        },
                    )?;
                }
                Span::current().set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("hit".into()),
                );

                Ok(ControlFlow::Break((cacheable_item, value)))
            } else {
                Span::current().set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("miss".into()),
                );
                Ok(ControlFlow::Continue((
                    cacheable_item,
                    key,
                    invalidation_keys,
                )))
            }
        }
        Err(err) => {
            let span = Span::current();
            if !matches!(err, sqlx::Error::RowNotFound) {
                span.mark_as_error(format!("cannot get cache entry: {err}"));

                u64_counter_with_unit!(
                    "apollo.router.operations.response_cache.fetch.error",
                    "Errors when fetching data from cache",
                    "{error}",
                    1,
                    "subgraph.name" = name.to_string(),
                    "code" = err.code()
                );
            }

            span.set_span_dyn_attribute(
                opentelemetry::Key::new("cache.status"),
                opentelemetry::Value::String("miss".into()),
            );
            Ok(ControlFlow::Continue((
                cacheable_item,
                key,
                invalidation_keys,
            )))
        }
    }
}

fn get_invalidation_keys_from_schema(
    cacheable_item: &CacheableItem,
    cache_key_components: &CacheKeyComponents,
    subgraph_enums: &HashMap<String, String>,
    supergraph_schema: &Arc<Valid<Schema>>,
) -> Result<HashSet<String>, anyhow::Error> {
    let syntetic_subgraph_name = &cache_key_components.subgraph_name; //FIXME
    if cacheable_item.is_entity() {
        // Check on types
        get_invalidation_entity_keys_from_schema(
            supergraph_schema,
            syntetic_subgraph_name,
            subgraph_enums,
            cacheable_item,
        )
    } else {
        // Check on root fields
        get_invalidation_root_keys_from_schema(
            syntetic_subgraph_name,
            cacheable_item,
            subgraph_enums,
            supergraph_schema,
        )
    }
}

/// Get invalidation keys from @cacheTag directives in supergraph schema for entities
fn get_invalidation_entity_keys_from_schema(
    supergraph_schema: &Arc<Valid<Schema>>,
    syntetic_subgraph_name: &str,
    subgraph_enums: &HashMap<String, String>,
    cacheable_item: &CacheableItem,
) -> Result<HashSet<String>, anyhow::Error> {
    let (typename, surrogate_key_data) = match cacheable_item {
        CacheableItem::RootFields { .. } => unreachable!(),
        CacheableItem::Entity {
            output_type,
            surrogate_key_data,
            ..
        } => (output_type, surrogate_key_data),
        CacheableItem::BatchItem {
            output_type,
            surrogate_key_data,
            ..
        } => (output_type, surrogate_key_data),
    };
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
                                    == Some(syntetic_subgraph_name)
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
    vars.insert(
        "$key".to_string(),
        Value::Object(surrogate_key_data.clone()),
    );
    let invalidation_cache_keys = cache_keys
        .map(|ck| ck.interpolate(&vars).map(|(res, _)| res))
        .collect::<Result<HashSet<String>, apollo_federation::connectors::StringTemplateError>>()?;
    Ok(invalidation_cache_keys)
}

fn get_invalidation_root_keys_from_schema(
    syntetic_subgraph_name: &str,
    cacheable_item: &CacheableItem,
    subgraph_enums: &HashMap<String, String>,
    supergraph_schema: &Arc<Valid<Schema>>,
) -> Result<HashSet<String>, anyhow::Error> {
    let CacheableItem::RootFields {
        output_names,
        surrogate_key_data,
        ..
    } = cacheable_item
    else {
        return Ok(Default::default());
    };
    let query_object_type_name = supergraph_schema
        .schema_definition
        .as_ref()
        .query
        .as_ref()
        .ok_or_else(|| FetchError::MalformedRequest {
            reason: "cannot get the root operation type from supergraph schema".to_string(),
        })?;

    let query_object_type = supergraph_schema
        .get_object(query_object_type_name.as_str())
        .ok_or_else(|| FetchError::MalformedRequest {
            reason: "cannot get the root operation from supergraph schema".to_string(),
        })?;

    let cache_keys = output_names
        .iter()
        .map(|field_name| {
            // We don't use field.definition because we need the directive set in supergraph schema not in the executable document
            let field_def = query_object_type
                .fields
                .get(
                    &Name::new(field_name.as_str()).map_err(|_| FetchError::MalformedRequest {
                        reason: "invalid root field name".to_string(),
                    })?,
                )
                .ok_or_else(|| FetchError::MalformedRequest {
                    reason: "cannot get the field definition from supergraph schema".to_string(),
                })?;
            let cache_keys = field_def
                .directives
                .get_all("join__directive")
                .filter_map(|dir| {
                    let name = dir.argument_by_name("name", supergraph_schema).ok()?;
                    if name.as_str()? != CACHE_TAG_DIRECTIVE_NAME {
                        return None;
                    }
                    let is_current_subgraph =
                        dir.argument_by_name("graphs", supergraph_schema)
                            .ok()
                            .and_then(|f| {
                                Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
                                    |g| {
                                        subgraph_enums.get(g.as_str()).map(|s| s.as_str())
                                            == Some(syntetic_subgraph_name)
                                    },
                                ))
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
            vars.insert(
                "$args".to_string(),
                Value::Object(surrogate_key_data.clone()),
            );
            cache_keys
                .map(|ck| Ok(ck.interpolate(&vars).map(|(res, _)| res)?))
                .collect::<Result<Vec<String>, anyhow::Error>>()
        })
        .collect::<Result<Vec<Vec<String>>, anyhow::Error>>()?;

    let invalidation_cache_keys: HashSet<String> = cache_keys.into_iter().flatten().collect();

    Ok(invalidation_cache_keys)
}

// build a cache key for the root operation
#[allow(clippy::too_many_arguments)]
fn extract_cache_key_root(
    subgraph_name: &str,
    cacheable_item: &CacheableItem,
    cache_key_components: &CacheKeyComponents,
    context: &Context,
    is_known_private: bool,
    private_id: Option<&str>,
) -> (String, Vec<String>) {
    let entity_type = match cacheable_item {
        CacheableItem::RootFields { output_type, .. }
        | CacheableItem::Entity { output_type, .. }
        | CacheableItem::BatchItem { output_type, .. } => output_type.to_string(),
    };
    let key = ConnectorPrimaryCacheKey {
        subgraph_name,
        graphql_type: entity_type.clone(),
        cache_key_components,
        context,
        private_id: if is_known_private { private_id } else { None },
    }
    .hash();

    let invalidation_keys = vec![format!(
        "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}"
    )];

    (key, invalidation_keys)
}

// /// Returns invalidation keys and typename
// fn get_invalidation_connector_entity_keys_from_schema(
//     subgraph_enums: &HashMap<String, String>,
//     supergraph_schema: &Valid<Schema>,
//     connector_info: &IsConnector,
// ) -> Result<(HashSet<String>, String), anyhow::Error> {
//     let connect_id = &connector_info.connector_id;
//     let subgraph_synthetic_name = connect_id.synthetic_name();
//     let connected_element = connect_id.directive.element(supergraph_schema)?;
//     let res: Result<(HashSet<String>, String), anyhow::Error> = match connected_element {
//         ConnectedElement::Field {
//             field_def,
//             parent_type,
//             ..
//         } => {
//             dbg!(&field_def);
//             let mut root_operation_fields = connector_info
//                 .executable_document
//                 .operations
//                 .iter()
//                 .next()
//                 .ok_or_else(|| FetchError::MalformedRequest {
//                     reason:
//                         "cannot get the operation from executable document for subgraph request"
//                             .to_string(),
//                 })?
//                 .root_fields(&connector_info.executable_document);
//             // FIXME: this doesn't work with entities
//             let field = if root_operation_fields.any(|f| f.name.as_str() == ENTITIES) {
//                 None
//             } else {
//                 Some(
//                     root_operation_fields
//                         .find(|f| f.name == field_def.name)
//                         .ok_or_else(|| FetchError::MalformedRequest {
//                             reason:
//                                 "cannot get the field from executable document for subgraph request"
//                                     .to_string(),
//                         })?,
//                 )
//             };

//             match field {
//                 Some(field) => {
//                     let directives = &supergraph_schema
//                         .get_object(&parent_type.name.to_string())
//                         .ok_or_else(|| {
//                             FetchError::MalformedRequest {
//                     reason:
//                         "cannot get the object from executable document for subgraph request"
//                             .to_string(),
//                 }
//                         })?
//                         .fields
//                         .get(&field_def.name)
//                         .ok_or_else(|| {
//                             FetchError::MalformedRequest {
//                     reason:
//                         "cannot get the object from executable document for subgraph request"
//                             .to_string(),
//                 }
//                         })?
//                         .as_ref()
//                         .directives;

//                     let cache_keys = directives.get_all("join__directive").filter_map(|dir| {
//                         let name = dir.argument_by_name("name", supergraph_schema).ok()?;
//                         if name.as_str()? != CACHE_TAG_DIRECTIVE_NAME {
//                             return None;
//                         }
//                         let is_current_subgraph = dir
//                             .argument_by_name("graphs", supergraph_schema)
//                             .ok()
//                             .and_then(|f| {
//                                 Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
//                                     |g| {
//                                         subgraph_enums.get(g.as_str()).map(|s| s.as_str())
//                                             == Some(&subgraph_synthetic_name)
//                                     },
//                                 ))
//                             })
//                             .unwrap_or_default();
//                         if !is_current_subgraph {
//                             return None;
//                         }
//                         let mut format = None;
//                         for (field_name, value) in dir
//                             .argument_by_name("args", supergraph_schema)
//                             .ok()?
//                             .as_object()?
//                         {
//                             if field_name.as_str() == "format" {
//                                 format = value
//                                     .as_str()
//                                     .and_then(|v| v.parse::<StringTemplate>().ok())
//                             }
//                         }
//                         format
//                     });

//                     let mut errors = Vec::new();
//                     // Query::validate_variables runs before this
//                     let variable_values =
//                         Valid::assume_valid_ref(connector_info.variables.as_ref());
//                     let args = coerce_argument_values(
//                         supergraph_schema,
//                         &connector_info.executable_document,
//                         variable_values,
//                         &mut errors,
//                         Default::default(),
//                         field_def,
//                         field,
//                     )
//                     .map_err(|_| FetchError::MalformedRequest {
//                         reason: format!("cannot argument values for root fields {:?}", field.name),
//                     })?;

//                     if !errors.is_empty() {
//                         return Err(FetchError::MalformedRequest {
//                             reason: format!(
//                                 "cannot coerce argument values for root fields {:?}, errors: {errors:?}",
//                                 field.name,
//                             ),
//                         }
//                         .into());
//                     }

//                     let mut vars = IndexMap::default();
//                     vars.insert("$args".to_string(), Value::Object(args));
//                     let cache_tags: HashSet<String> = cache_keys
//                         .map(|ck| Ok(ck.interpolate(&vars).map(|(res, _)| res)?))
//                         .collect::<Result<HashSet<String>, anyhow::Error>>()?;

//                     Ok((cache_tags, field_def.ty.inner_named_type().to_string()))
//                 }
//                 // If entities
//                 None => {
//                     let directives = &supergraph_schema
//                         .get_object(&dbg!(field_def.ty.inner_named_type().to_string()))
//                         .ok_or_else(|| {
//                             FetchError::MalformedRequest {
//                         reason:
//                             "cannot get the object from executable document for subgraph request"
//                                 .to_string(),
//                     }
//                         })?
//                         .as_ref()
//                         .directives;
//                     dbg!(&directives);
//                     let cache_keys = directives.get_all("join__directive").filter_map(|dir| {
//                         let name = dir.argument_by_name("name", supergraph_schema).ok()?;
//                         if name.as_str()? != CACHE_TAG_DIRECTIVE_NAME {
//                             return None;
//                         }
//                         let is_current_subgraph = dir
//                             .argument_by_name("graphs", supergraph_schema)
//                             .ok()
//                             .and_then(|f| {
//                                 Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
//                                     |g| {
//                                         subgraph_enums.get(g.as_str()).map(|s| s.as_str())
//                                             == Some(&subgraph_synthetic_name)
//                                     },
//                                 ))
//                             })
//                             .unwrap_or_default();
//                         if !is_current_subgraph {
//                             return None;
//                         }
//                         let mut format = None;
//                         for (field_name, value) in dir
//                             .argument_by_name("args", supergraph_schema)
//                             .ok()?
//                             .as_object()?
//                         {
//                             if field_name.as_str() == "format" {
//                                 format = value
//                                     .as_str()
//                                     .and_then(|v| v.parse::<StringTemplate>().ok())
//                             }
//                         }
//                         format
//                     });

//                     let mut vars = IndexMap::default();
//                     vars.insert(
//                         "$key".to_string(),
//                         Value::Object(connector_info.variables.as_ref().clone()),
//                     );
//                     let cache_tags: HashSet<String> = cache_keys
//                         .map(|ck| Ok(ck.interpolate(&vars).map(|(res, _)| res)?))
//                         .collect::<Result<HashSet<String>, anyhow::Error>>()?;

//                     Ok((cache_tags, field_def.ty.inner_named_type().to_string()))
//                 }
//             }
//             // // FIXME: field_def doesn't contain the cacheTag directive I don't know why
//             // let cache_keys =
//             //     directives.get_all("join__directive").filter_map(|dir| {
//             //         let name = dir.argument_by_name("name", supergraph_schema).ok()?;
//             //         if name.as_str()? != CACHE_TAG_DIRECTIVE_NAME {
//             //             return None;
//             //         }
//             //         let is_current_subgraph =
//             //             dir.argument_by_name("graphs", supergraph_schema)
//             //                 .ok()
//             //                 .and_then(|f| {
//             //                     Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
//             //                         |g| {
//             //                             subgraph_enums.get(g.as_str()).map(|s| s.as_str())
//             //                                 == Some(&subgraph_synthetic_name)
//             //                         },
//             //                     ))
//             //                 })
//             //                 .unwrap_or_default();
//             //         if !is_current_subgraph {
//             //             return None;
//             //         }
//             //         let mut format = None;
//             //         for (field_name, value) in dir
//             //             .argument_by_name("args", supergraph_schema)
//             //             .ok()?
//             //             .as_object()?
//             //         {
//             //             if field_name.as_str() == "format" {
//             //                 format = value
//             //                     .as_str()
//             //                     .and_then(|v| v.parse::<StringTemplate>().ok())
//             //             }
//             //         }
//             //         format
//             //     });
//         }
//         ConnectedElement::Type { type_def } => {
//             let field_def = supergraph_schema
//                 .get_object(&type_def.name)
//                 .ok_or_else(|| FetchError::MalformedRequest {
//                     reason: "can't find corresponding type for __typename {typename:?}".to_string(),
//                 })?;
//             let cache_keys = field_def
//                 .directives
//                 .get_all("join__directive")
//                 .filter_map(|dir| {
//                     let name = dir.argument_by_name("name", supergraph_schema).ok()?;
//                     if name.as_str()? != CACHE_TAG_DIRECTIVE_NAME {
//                         return None;
//                     }
//                     let is_current_subgraph =
//                         dir.argument_by_name("graphs", supergraph_schema)
//                             .ok()
//                             .and_then(|f| {
//                                 Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
//                                     |g| {
//                                         subgraph_enums.get(g.as_str()).map(|s| s.as_str())
//                                             == Some(&subgraph_synthetic_name)
//                                     },
//                                 ))
//                             })
//                             .unwrap_or_default();
//                     if !is_current_subgraph {
//                         return None;
//                     }
//                     dir.argument_by_name("args", supergraph_schema)
//                         .ok()?
//                         .as_object()?
//                         .iter()
//                         .find_map(|(field_name, value)| {
//                             if field_name.as_str() == "format" {
//                                 value.as_str()?.parse::<StringTemplate>().ok()
//                             } else {
//                                 None
//                             }
//                         })
//                 });
//             let mut vars = IndexMap::default();

//             let mut cache_tags: HashSet<String> = HashSet::new();
//             for ck in cache_keys {
//                 let representations = connector_info
//                     .variables
//                     .get(REPRESENTATIONS)
//                     .and_then(|repr| repr.as_array());
//                 match representations {
//                     Some(reprs) => {
//                         // As it's an entity we will add different cache tags for every entity
//                         cache_tags.extend(
//                             reprs
//                                 .iter()
//                                 .map(|repr_val| {
//                                     vars.insert("$key".to_string(), repr_val.clone());
//                                     ck.interpolate(&vars).map(|(res, _)| res)
//                                 })
//                                 .collect::<Result<Vec<String>, StringTemplateError>>()?,
//                         );
//                     }
//                     None => {
//                         vars.insert(
//                             "$key".to_string(),
//                             Value::Object(connector_info.variables.as_ref().clone()),
//                         );
//                         cache_tags.insert(ck.interpolate(&vars).map(|(res, _)| res)?);
//                     }
//                 }
//             }

//             Ok((cache_tags, type_def.name.to_string()))
//         }
//     };
//     let (cache_tags, type_name) = res?;

//     // let cache_keys = root_operation_fields
//     //     .map(|field| {
//     //         // We don't use field.definition because we need the directive set in supergraph schema not in the executable document
//     //         let field_def = query_object_type.fields.get(&field.name).ok_or_else(|| {
//     //             FetchError::MalformedRequest {
//     //                 reason: "cannot get the field definition from supergraph schema".to_string(),
//     //             }
//     //         })?;

//     //     })
//     //     .collect::<Result<Vec<Vec<String>>, anyhow::Error>>()?;

//     // let invalidation_cache_keys: HashSet<String> = cache_keys.into_iter().flatten().collect();

//     Ok((cache_tags, type_name))
// }
