use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::runtime::cache::CacheKey;
use apollo_federation::connectors::runtime::cache::CacheKeyComponents;
use apollo_federation::connectors::runtime::cache::CachePolicy;
use futures::future::BoxFuture;
use http::HeaderMap;
use http::HeaderValue;
use http::Response;
use http::header::CACHE_CONTROL;
use itertools::Itertools;
use lru::LruCache;
use opentelemetry::Key;
use opentelemetry::StringValue;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::RwLock;
use tower::BoxError;
use tower::ServiceExt;
use tower_service::Service;
use tracing::Instrument;
use tracing::Level;
use tracing::Span;

use super::cache_control::CacheControl;
use super::metrics::CacheMetricContextKey;
use super::plugin::RESPONSE_CACHE_VERSION;
use super::postgres::BatchDocument;
use super::postgres::CacheEntry;
use super::postgres::PostgresCacheStorage;
use crate::Context;
use crate::batching::BatchQuery;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::plugin::CONTEXT_CACHE_KEY;
use crate::plugins::response_cache::plugin::CONTEXT_DEBUG_CACHE_KEYS;
use crate::plugins::response_cache::plugin::CacheEntryKind;
use crate::plugins::response_cache::plugin::CacheHitMiss;
use crate::plugins::response_cache::plugin::CacheKeyContext;
use crate::plugins::response_cache::plugin::CacheKeySource;
use crate::plugins::response_cache::plugin::CacheKeysContext;
use crate::plugins::response_cache::plugin::CacheSubgraph;
use crate::plugins::response_cache::plugin::ENTITIES;
use crate::plugins::response_cache::plugin::GRAPHQL_RESPONSE_EXTENSION_ENTITY_CACHE_TAGS;
use crate::plugins::response_cache::plugin::INTERNAL_CACHE_TAG_PREFIX;
use crate::plugins::response_cache::plugin::IsDebug;
use crate::plugins::response_cache::plugin::PrivateQueryKey;
use crate::plugins::response_cache::plugin::Storage;
use crate::plugins::response_cache::plugin::update_cache_control;
use crate::plugins::telemetry::config_new::connector::ConnectorRequest;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::span_ext::SpanMarkError;
use crate::services;
use crate::services::connect;

/// represents the result of a cache lookup for an entity type and key
struct IntermediateResult {
    key: String,
    invalidation_keys: Vec<String>,
    typename: String,
    entity_key: serde_json_bytes::Map<ByteString, Value>,
    // Optional because None if debug is disabled, to avoid cloning
    prepared_request: Option<ConnectorRequest>,
    cache_entry: Option<CacheEntry>,
}

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
                    let mut digest = Sha256::new();
                    digest.update(s);
                    hex::encode(digest.finalize().as_slice())
                })
            })
        })
    }

    async fn call_inner(
        mut self,
        mut request: services::connect::Request,
    ) -> Result<services::connect::Response, BoxError> {
        let subgraph_name = self.name.clone();
        let storage = match self.storage.get(&self.name) {
            Some(storage) => storage.clone(),
            None => {
                u64_counter_with_unit!(
                    "apollo.router.operations.response_cache.fetch.error",
                    "Errors when fetching data from cache",
                    "{error}",
                    1,
                    "subgraph.name" = "connector",
                    "code" = "NO_STORAGE"
                );
                return self
                    .service
                    .map_response(move |response: services::connect::Response| {
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

        let private_id = self.get_private_id(&request.context);
        let mut hasher = Sha256::new();
        hasher.update(
            request
                .get_cache_key()
                .cache_key_components()
                .iter()
                .map(|c| c.to_string())
                .join("/"),
        );
        // Knowing if there's a private_id or not will differentiate the hash because for a same query it can be both public and private depending if we have private_id set or not
        let private_query_key = PrivateQueryKey {
            query_hash: hex::encode(hasher.finalize()),
            has_private_id: private_id.is_some(),
        };

        let is_known_private = {
            self.private_queries
                .read()
                .await
                .contains(&private_query_key)
        };
        let cache_keys = request
            .cache_key
            .as_ref()
            .ok_or_else(|| BoxError::from("cannot get cache keys for connectors"))?;
        let is_root_field = matches!(cache_keys, CacheKey::Roots { .. });
        if is_root_field {
            let items = cache_keys.cache_key_components();
            let item = items.first().ok_or_else(|| {
                BoxError::from("roots cache keys must contain at least 1 element")
            })?;
            let (operation_type, entity_type) = item
                .fetch_details
                .as_root()
                .ok_or_else(|| BoxError::from("fetch details must be entity"))?;
            if operation_type.is_query() {
                let typename = entity_type.to_string();
                match cache_lookup_root_connector(
                    &subgraph_name,
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
                    "type" = "connector",
                    kind = "root",
                    "graphql.type" = typename,
                    debug = self.debug,
                    private = is_known_private,
                    contains_private_id = private_id.is_some()
                ))
                .await?
                {
                    ControlFlow::Break(response) => {
                        // cache_hit.insert("Query".to_string(), CacheHitMiss { hit: 1, miss: 0 });
                        // let _ = response.context.insert(
                        //     CacheMetricContextKey::new(response.subgraph_name.clone()),
                        //     CacheSubgraph(cache_hit),
                        // );
                        Ok(response)
                    }
                    ControlFlow::Continue((request, mut root_cache_key, invalidation_keys)) => {
                        // cache_hit.insert("Query".to_string(), CacheHitMiss { hit: 0, miss: 1 });
                        // let _ = request.context.insert(
                        //     CacheMetricContextKey::new(request.subgraph_name.clone()),
                        //     CacheSubgraph(cache_hit),
                        // );
                        // let mut root_operation_fields: Vec<String> = Vec::new();
                        // let mut debug_subgraph_request = None;
                        // TODO: missing root fields information
                        // if self.debug {
                        //     root_operation_fields = request
                        //         .executable_document
                        //         .as_ref()
                        //         .and_then(|executable_document| {
                        //             let operation_name =
                        //                 request.subgraph_request.body().operation_name.as_deref();
                        //             Some(
                        //                 executable_document
                        //                     .operations
                        //                     .get(operation_name)
                        //                     .ok()?
                        //                     .root_fields(executable_document)
                        //                     .map(|f| f.name.to_string())
                        //                     .collect(),
                        //             )
                        //         })
                        //         .unwrap_or_default();
                        //     debug_subgraph_request = Some(request.subgraph_request.body().clone());
                        // }
                        let response = self.service.call(request).await?;
                        let header_map =
                            response.cache_policy.headers().first().ok_or_else(|| {
                                BoxError::from("cannot get cache policy from root fields connector")
                            })?;
                        let cache_control = if header_map.contains_key(CACHE_CONTROL) {
                            CacheControl::new(header_map, self.subgraph_ttl.into())?
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
                                // self.lru_size_instrument.update(size as u64);

                                if let Some(s) = private_id.as_ref() {
                                    root_cache_key = format!("{root_cache_key}:{s}");
                                }
                            }

                            // if self.debug {
                            //     response.context.upsert::<_, CacheKeysContext>(
                            //         CONTEXT_DEBUG_CACHE_KEYS,
                            //         |mut val| {
                            //             val.push(CacheKeyContext {
                            //                 key: root_cache_key.clone(),
                            //                 hashed_private_id: private_id.clone(),
                            //                 invalidation_keys: invalidation_keys
                            //                     .clone()
                            //                     .into_iter()
                            //                     .filter(|k| {
                            //                         !k.starts_with(INTERNAL_CACHE_TAG_PREFIX)
                            //                     })
                            //                     .collect(),
                            //                 kind: CacheEntryKind::RootFields {
                            //                     root_fields: root_operation_fields,
                            //                 },
                            //                 subgraph_name: self.name.clone(),
                            //                 subgraph_request: debug_subgraph_request
                            //                     .unwrap_or_default(),
                            //                 source: CacheKeySource::Subgraph,
                            //                 cache_control: cache_control.clone(),
                            //                 data: serde_json_bytes::to_value(
                            //                     response.response.body().clone(),
                            //                 )
                            //                 .unwrap_or_default(),
                            //             });

                            //             val
                            //         },
                            //     )?;
                            // }

                            if private_id.is_none() {
                                // the response has a private scope but we don't have a way to differentiate users, so we do not store the response in cache
                                // We don't need to fill the context with this cache key as it will never be cached
                                return Ok(response);
                            }
                        }
                        // else if self.debug {
                        //     response.context.upsert::<_, CacheKeysContext>(
                        //         CONTEXT_DEBUG_CACHE_KEYS,
                        //         |mut val| {
                        //             val.push(CacheKeyContext {
                        //                 key: root_cache_key.clone(),
                        //                 hashed_private_id: private_id.clone(),
                        //                 invalidation_keys: invalidation_keys
                        //                     .clone()
                        //                     .into_iter()
                        //                     .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                        //                     .collect(),
                        //                 kind: CacheEntryKind::RootFields {
                        //                     root_fields: root_operation_fields,
                        //                 },
                        //                 subgraph_name: self.name.clone(),
                        //                 subgraph_request: debug_subgraph_request
                        //                     .unwrap_or_default(),
                        //                 source: CacheKeySource::Subgraph,
                        //                 cache_control: cache_control.clone(),
                        //                 data: serde_json_bytes::to_value(
                        //                     response.response.body().clone(),
                        //                 )
                        //                 .unwrap_or_default(),
                        //             });

                        //             val
                        //         },
                        //     )?;
                        // }

                        if cache_control.should_store() {
                            cache_store_root_from_response(
                                storage,
                                self.subgraph_ttl,
                                &self.name,
                                &response,
                                cache_control,
                                root_cache_key,
                                invalidation_keys,
                            )
                            .await?;
                        }

                        Ok(response)
                    }
                }
            } else {
                self.service.call(request).await
            }
        } else {
            // for entities
            match cache_lookup_entities(
                self.name.clone(),
                request,
                self.supergraph_schema.clone(),
                &self.subgraph_enums,
                storage.clone(),
                is_known_private,
                private_id.as_deref(),
                self.debug,
            )
            .instrument(tracing::info_span!(
                "response_cache.lookup",
                "type" = "connector",
                kind = "entity",
                debug = self.debug,
                private = is_known_private,
                contains_private_id = private_id.is_some()
            ))
            .await?
            {
                ControlFlow::Break(response) => Ok(response),
                ControlFlow::Continue((mut request, mut cache_result)) => {
                    let context = request.context.clone();
                    // let mut debug_connector_requests: Vec<String> = Vec::new();
                    if self.debug {
                        // debug_connector_requests = request
                        //     .prepared_requests
                        //     .iter()
                        //     .filter_map(|r| r.transport_request.as_http())
                        //     .map(|r| r.inner.body().clone())
                        //     .collect::<Vec<_>>();
                        let debug_cache_keys_ctx = cache_result.iter().filter_map(|ir| {
                            ir.cache_entry.as_ref().map(|cache_entry| CacheKeyContext {
                                hashed_private_id: None, //FIXME
                                key: cache_entry.cache_key.clone(),
                                invalidation_keys: ir.invalidation_keys.clone().into_iter()
                                .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                                .collect(),
                                kind: CacheEntryKind::Entity {
                                    typename: ir.typename.clone(),
                                    entity_key: ir.entity_key.clone(),
                                },
                                subgraph_name: self.name.clone(),
                                connector_request: ir.prepared_request.as_ref().and_then(|pr| pr.transport_request.as_http()).map(|r| r.inner.body().clone()),
                                subgraph_request: None,
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
                    // Only keep prepared_requests that needs to be fetched from connector
                    if self.debug {
                        // Clone it because we will need it for debug info
                        request.prepared_requests = cache_result
                            .iter()
                            .filter_map(|ir| {
                                if ir.cache_entry.is_none() {
                                    ir.prepared_request.clone()
                                } else {
                                    None
                                }
                            })
                            .collect();
                    } else {
                        request.prepared_requests = cache_result
                            .iter_mut()
                            .filter_map(|ir| {
                                if ir.cache_entry.is_none() {
                                    ir.prepared_request.take()
                                } else {
                                    None
                                }
                            })
                            .collect();
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
                            let (new_entities, new_errors) =
                                assemble_response_from_errors(&[graphql_error], &mut cache_result);

                            let mut data = Object::default();
                            data.insert(ENTITIES, new_entities.into());

                            let mut response = connect::Response {
                                response: http::Response::new(
                                    graphql::Response::builder()
                                        .data(Value::Object(data))
                                        .errors(new_errors)
                                        .build(),
                                ),
                                cache_policy: CachePolicy::Entities(Vec::new()),
                                context,
                            };
                            CacheControl::no_store().to_headers(response.response.headers_mut())?;

                            return Ok(response);
                        }
                    };

                    // dbg!(&response.response.headers());
                    // dbg!(response.cache_policy.headers());
                    // let mut cache_control: Option<CacheControl> = None;
                    // for headers in response.cache_policy.headers() {
                    //     match &mut cache_control {
                    //         Some(cache_control) => {
                    //             if headers.contains_key(CACHE_CONTROL) {
                    //                 let new_cc = cache_control.merge(&CacheControl::new(
                    //                     response.response.headers(),
                    //                     self.subgraph_ttl.into(),
                    //                 )?);
                    //                 *cache_control = new_cc;
                    //             } else {
                    //                 // Set no store because no cache-control header in one of the response which means we can't cache it
                    //             }
                    //         }
                    //         _ => {
                    //             if headers.contains_key(CACHE_CONTROL) {
                    //                 cache_control = Some(CacheControl::new(
                    //                     response.response.headers(),
                    //                     self.subgraph_ttl.into(),
                    //                 )?);
                    //             }
                    //         }
                    //     }
                    // }
                    // if cache_control.is_none() {
                    //     cache_control = Some(CacheControl::no_store());
                    // }
                    // let mut cache_control = cache_control.unwrap();

                    // if let Some(control_from_cached) = cache_result.1 {
                    //     cache_control = cache_control.merge(&control_from_cached);
                    // }

                    // TODO
                    // if !is_known_private && cache_control.private() {
                    //     self.private_queries
                    //         .write()
                    //         .await
                    //         .put(private_query_key, ());
                    // }

                    // dbg!(&cache_control);
                    let cache_result = cache_result
                        .into_iter()
                        .zip(response.cache_policy.headers())
                        .map(|(cache_res, headers)| {
                            let cache_control = if headers.contains_key(CACHE_CONTROL) {
                                // FIXME: handle error properly
                                CacheControl::new(headers, self.subgraph_ttl.into())
                                    .unwrap_or_else(|_| CacheControl::no_store())
                            } else {
                                CacheControl::no_store()
                            };
                            (cache_res, cache_control)
                        })
                        .collect::<Vec<_>>();
                    cache_store_entities_from_response(
                        storage,
                        &self.name,
                        self.subgraph_ttl,
                        &mut response,
                        cache_result,
                        is_known_private,
                        private_id,
                        self.debug,
                    )
                    .await?;
                    // TODO need this I think
                    // cache_control.to_headers(response.response.headers_mut())?;

                    Ok(response)
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn cache_store_root_from_response(
    cache: PostgresCacheStorage,
    default_subgraph_ttl: Duration,
    subgraph_name: &str,
    response: &connect::Response,
    cache_control: CacheControl,
    cache_key: String,
    mut invalidation_keys: Vec<String>,
) -> Result<(), BoxError> {
    if let Some(data) = response.response.body().data.as_ref() {
        let ttl = cache_control
            .ttl()
            .map(|secs| Duration::from_secs(secs as u64))
            .unwrap_or(default_subgraph_ttl);

        if response.response.body().errors.is_empty() && cache_control.should_store() {
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

            let span = tracing::info_span!("response_cache.store", "kind" = "root", "subgraph.name" = subgraph_name.to_string(), "ttl" = ?ttl);
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
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cache_lookup_root_connector(
    name: &str,
    cache: PostgresCacheStorage,
    is_known_private: bool,
    private_id: Option<&str>,
    debug: bool,
    mut request: connect::Request,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
) -> Result<ControlFlow<connect::Response, (connect::Request, String, Vec<String>)>, BoxError> {
    // TODO
    // let invalidation_cache_keys =
    //     get_invalidation_root_keys_from_schema(&request, subgraph_enums, supergraph_schema)?;
    let invalidation_cache_keys = Vec::new();
    let context = request.context.clone();
    let cache_keys = request.get_cache_key().cache_key_components();
    let nb_cache_key_components = cache_keys.len();
    let cache_key_components = cache_keys.first().ok_or_else(|| {
        BoxError::from("cannot get cache key components for root fields connector")
    })?;
    let (key, mut invalidation_keys) = extract_cache_key_root(
        name,
        cache_key_components,
        &context,
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
        "subgraph.name" = request.subgraph_name.clone(),
        "kind" = "single"
    );

    match cache_result {
        Ok(value) => {
            if value.control.can_use() {
                let control = value.control.clone();
                update_cache_control(&request.context, &control);
                // if debug {
                //     let root_operation_fields: Vec<String> = request
                //         .executable_document
                //         .as_ref()
                //         .and_then(|executable_document| {
                //             Some(
                //                 executable_document
                //                     .operations
                //                     .iter()
                //                     .next()?
                //                     .root_fields(executable_document)
                //                     .map(|f| f.name.to_string())
                //                     .collect(),
                //             )
                //         })
                //         .unwrap_or_default();

                //     request.context.upsert::<_, CacheKeysContext>(
                //         CONTEXT_DEBUG_CACHE_KEYS,
                //         |mut val| {
                //             val.push(CacheKeyContext {
                //                 key: value.cache_key.clone(),
                //                 hashed_private_id: private_id.map(ToString::to_string),
                //                 invalidation_keys: invalidation_keys
                //                     .clone()
                //                     .into_iter()
                //                     .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
                //                     .collect(),
                //                 kind: CacheEntryKind::RootFields {
                //                     root_fields: root_operation_fields,
                //                 },
                //                 subgraph_name: request.subgraph_name.clone(),
                //                 subgraph_request: request.subgraph_request.body().clone(),
                //                 source: CacheKeySource::Cache,
                //                 cache_control: value.control.clone(),
                //                 data: serde_json_bytes::json!({"data": value.data.clone()}),
                //             });

                //             val
                //         },
                //     )?;
                // }
                let mut header_map = HeaderMap::new();
                control.to_headers(&mut header_map)?;
                Span::current().set_span_dyn_attribute(
                    opentelemetry::Key::new("cache.status"),
                    opentelemetry::Value::String("hit".into()),
                );
                let mut response = connect::Response {
                    response: Response::new(graphql::Response::builder().data(value.data).build()),
                    cache_policy: CachePolicy::Roots(
                        (0..nb_cache_key_components)
                            .map(|_| header_map.clone())
                            .collect(),
                    ),
                    context,
                };

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
            Ok(ControlFlow::Continue((request, key, invalidation_keys)))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn cache_lookup_entities(
    name: String,
    mut request: connect::Request,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
    cache: PostgresCacheStorage,
    is_known_private: bool,
    private_id: Option<&str>,
    debug: bool,
) -> Result<ControlFlow<connect::Response, (connect::Request, Vec<IntermediateResult>)>, BoxError> {
    let cache_key = request.get_cache_key();
    let cache_metadata = extract_cache_keys(
        &name,
        supergraph_schema,
        subgraph_enums,
        cache_key.cache_key_components(),
        is_known_private,
        private_id,
    )?;
    let keys_len = cache_metadata.len();

    let now = Instant::now();
    let cache_result = cache
        .get_multiple(
            &cache_metadata
                .iter()
                .map(|k| k.cache_key.as_str())
                .collect::<Vec<&str>>(),
        )
        .await;

    f64_histogram_with_unit!(
        "apollo.router.operations.response_cache.fetch",
        "Time to fetch data from cache",
        "s",
        now.elapsed().as_secs_f64(),
        "subgraph.name" = name.clone(),
        "kind" = "batch"
    );

    let cache_result: Vec<Option<CacheEntry>> = match cache_result {
        Ok(res) => {
            Span::current().set_span_dyn_attribute(
                opentelemetry::Key::new("cache.status"),
                opentelemetry::Value::String("hit".into()),
            );
            res.into_iter()
                .map(|v| match v {
                    Some(v) if v.control.can_use() => Some(v),
                    _ => None,
                })
                .collect()
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
                    "subgraph.name" = name.clone(),
                    "code" = err.code()
                );
            }
            span.set_span_dyn_attribute(
                opentelemetry::Key::new("cache.status"),
                opentelemetry::Value::String("miss".into()),
            );

            std::iter::repeat_n(None, keys_len).collect()
        }
    };

    // remove from representations the entities we already obtained from the cache
    let cache_result = filter_requests(
        &name,
        &mut request.prepared_requests,
        cache_metadata,
        cache_result,
        &request.context,
    )?;

    let contains_uncached_entries = cache_result.iter().any(|cr| cr.cache_entry.is_none());
    if contains_uncached_entries {
        // request.prepared_requests =
        let cache_status = if cache_result.is_empty() {
            opentelemetry::Value::String("miss".into())
        } else {
            opentelemetry::Value::String("partial_hit".into())
        };
        Span::current()
            .set_span_dyn_attribute(opentelemetry::Key::new("cache.status"), cache_status);

        Ok(ControlFlow::Continue((request, cache_result)))
    } else {
        if debug {
            let debug_cache_keys_ctx = cache_result.iter().enumerate().filter_map(|(idx, ir)| {
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
                    subgraph_request: None,
                    connector_request: ir
                        .prepared_request
                        .as_ref()
                        .and_then(|pr| pr.transport_request.as_http())
                        .map(|http_req| http_req.inner.body().clone()),
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

        let (entities, cache_policy): (Vec<Value>, Vec<HeaderMap>) = cache_result
            .into_iter()
            .filter_map(|res| res.cache_entry)
            .map(|entry| {
                let mut header_map = HeaderMap::new();
                header_map.insert(
                    CACHE_CONTROL,
                    HeaderValue::from_str(
                        &entry.control.to_cache_control_header().unwrap_or_default(),
                    )
                    .unwrap_or_else(|_| HeaderValue::from_static("nostore")),
                );
                (entry.data, header_map)
            })
            .multiunzip();
        let mut data = Object::default();
        data.insert(ENTITIES, entities.into());

        let mut response = connect::Response {
            response: http::Response::builder()
                .body(graphql::Response::builder().data(data).build())?,
            cache_policy: CachePolicy::Entities(cache_policy),
            context: request.context,
        };

        // FIXME: probably return a computed cache-control from all cache_policy
        // cache_control
        //     .unwrap_or_default()
        //     .to_headers(response.response.headers_mut())?;

        Ok(ControlFlow::Break(response))
    }
}

#[allow(clippy::too_many_arguments)]
async fn cache_store_entities_from_response(
    cache: PostgresCacheStorage,
    subgraph_name: &str,
    default_subgraph_ttl: Duration,
    response: &mut connect::Response,
    mut result_from_cache: Vec<(IntermediateResult, CacheControl)>,
    is_known_private: bool,
    private_id: Option<String>,
    debug: bool,
) -> Result<(), BoxError> {
    let mut data = response.response.body_mut().data.take();
    if let Some(mut entities) = data
        .as_mut()
        .and_then(|v| v.as_object_mut())
        .and_then(|o| o.remove(ENTITIES))
    {
        // // if the scope is private but we do not have a way to differentiate users, do not store anything in the cache
        // let should_cache_private = !cache_control.private() || private_id.is_some();

        // let update_key_private = if !is_known_private && cache_control.private() {
        //     private_id
        // } else {
        //     None
        // };

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
            &mut result_from_cache,
            is_known_private,
            private_id,
            subgraph_name,
            per_entity_surrogate_keys,
            response.context.clone(),
            debug,
        )
        .await?;

        data.as_mut()
            .and_then(|v| v.as_object_mut())
            .map(|o| o.insert(ENTITIES, new_entities.into()));
        response.response.body_mut().data = data;
        response.response.body_mut().errors = new_errors;
    } else {
        let mut result_from_cache = result_from_cache
            .drain(..)
            .map(|(ir, _)| ir)
            .collect::<Vec<_>>();
        let (new_entities, new_errors) =
            assemble_response_from_errors(&response.response.body().errors, &mut result_from_cache);

        let mut data = Object::default();
        data.insert(ENTITIES, new_entities.into());

        response.response.body_mut().data = Some(Value::Object(data));
        response.response.body_mut().errors = new_errors;
    }

    Ok(())
}

struct ConnectorCacheMetadata {
    cache_key: String,
    invalidation_keys: Vec<String>,
}

// build a cache key for the root operation
#[allow(clippy::too_many_arguments)]
fn extract_cache_key_root(
    subgraph_name: &str,
    cache_key_components: &CacheKeyComponents,
    context: &Context,
    is_known_private: bool,
    private_id: Option<&str>,
) -> (String, Vec<String>) {
    let entity_type = cache_key_components.fetch_details.typename();
    // hash the query and operation name
    let mut query_hash = Sha256::new();
    query_hash.update(cache_key_components.to_string());
    let query_hash = hex::encode(query_hash.finalize());
    // hash more data like context cache data and authorization status
    let additional_data_hash = hash_additional_data(context);

    // the cache key is written to easily find keys matching a prefix for deletion:
    // - response cache version: current version of the hash
    // - subgraph name: subgraph name
    // - entity type: entity type
    // - query hash: invalidate the entry for a specific query and operation name
    // - additional data: separate cache entries depending on info like authorization status
    let mut key = String::new();
    let _ = write!(
        &mut key,
        "version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}:hash:{query_hash}:data:{additional_data_hash}"
    );
    let invalidation_keys = vec![format!(
        "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}"
    )];

    if is_known_private && let Some(id) = private_id {
        let _ = write!(&mut key, ":{id}");
    }
    (key, invalidation_keys)
}

pub(crate) fn hash_additional_data(context: &Context) -> String {
    let mut digest = Sha256::new();

    // digest.update(serde_json::to_vec(cache_key).unwrap());

    if let Ok(Some(cache_data)) = context.get::<&str, Object>(CONTEXT_CACHE_KEY)
        && let Some(v) = cache_data.get("all")
    {
        digest.update(serde_json::to_vec(v).unwrap())
    }

    hex::encode(digest.finalize().as_slice())
}

// build a list of keys to get from the cache in one query
#[allow(clippy::too_many_arguments)]
fn extract_cache_keys(
    subgraph_name: &str,
    supergraph_schema: Arc<Valid<Schema>>,
    subgraph_enums: &HashMap<String, String>,
    cache_keys: &[CacheKeyComponents],
    is_known_private: bool,
    private_id: Option<&str>,
) -> Result<Vec<ConnectorCacheMetadata>, BoxError> {
    let mut primary_cache_keys = Vec::with_capacity(cache_keys.len());
    let mut entities = HashMap::new();
    // TODO/ refactor to .map
    for item in cache_keys {
        let entity_type = item
            .fetch_details
            .as_entity()
            .ok_or_else(|| BoxError::from("fetch details must be entity"))?;
        let mut hasher = Sha256::new();
        hasher.update(&item.to_string());
        let request_hash = hex::encode(hasher.finalize());
        // We don't need entity key as part of the hash as it's already part of request_hash.
        let primary_cache_key = format!(
            "version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}:request:{request_hash}"
        );
        let invalidation_keys = vec![format!(
            "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{entity_type}"
        )];

        let typename = entity_type.to_string();
        match entities.get_mut(&typename) {
            Some(entity_nb) => *entity_nb += 1,
            None => {
                entities.insert(typename, 1u64);
            }
        }
        // -------------- TODO
        // let (mut invalidation_keys, typename) = get_invalidation_connector_entity_keys_from_schema(
        //     subgraph_enums,
        //     &supergraph_schema,
        //     request,
        // )?;

        primary_cache_keys.push(ConnectorCacheMetadata {
            cache_key: primary_cache_key,
            invalidation_keys,
        });
    }

    Span::current().set_span_dyn_attribute(
        Key::from_static_str("graphql.types"),
        opentelemetry::Value::Array(
            entities
                .keys()
                .cloned()
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

    Ok(primary_cache_keys)
}

// build a new list of prepared_requests for connectors
#[allow(clippy::type_complexity)]
fn filter_requests(
    subgraph_name: &str,
    prepared_requests: &mut Vec<ConnectorRequest>,
    keys: Vec<ConnectorCacheMetadata>,
    mut cache_result: Vec<Option<CacheEntry>>,
    context: &Context,
) -> Result<Vec<IntermediateResult>, BoxError> {
    let mut result = Vec::new();
    let mut cache_hit: HashMap<String, CacheHitMiss> = HashMap::new();
    for (
        (
            prepared_request,
            ConnectorCacheMetadata {
                cache_key: key,
                invalidation_keys,
                ..
            },
        ),
        mut cache_entry,
    ) in prepared_requests
        .drain(..)
        .zip(keys)
        .zip(cache_result.drain(..))
    {
        let typename = prepared_request.connector.base_type_name().to_string();
        // do not use that cache entry if it is stale
        if let Some(false) = cache_entry.as_ref().map(|c| c.control.can_use()) {
            cache_entry = None;
        }
        match cache_entry.as_ref() {
            None => {
                cache_hit.entry(typename.clone()).or_default().miss += 1;
            }
            Some(entry) => {
                cache_hit.entry(typename.clone()).or_default().hit += 1;
            }
        }

        result.push(IntermediateResult {
            key,
            invalidation_keys,
            typename,
            cache_entry,
            prepared_request: prepared_request.into(),
            // TODO: set the right entity keys here, when it will be available from connector service request
            entity_key: Default::default(),
        });
    }

    let _ = context.insert(
        CacheMetricContextKey::new(subgraph_name.to_string()),
        CacheSubgraph(cache_hit),
    );

    Ok(result)
}

// fill in the entities for the response connector
#[allow(clippy::too_many_arguments)]
async fn insert_entities_in_result(
    entities: &mut Vec<Value>,
    errors: &[Error],
    cache: PostgresCacheStorage,
    default_subgraph_ttl: Duration,
    result: &mut Vec<(IntermediateResult, CacheControl)>,
    is_known_private: bool,
    private_id: Option<String>,
    subgraph_name: &str,
    per_entity_surrogate_keys: &[Value],
    context: Context,
    debug: bool,
) -> Result<(Vec<Value>, Vec<Error>), BoxError> {
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
        (
            IntermediateResult {
                mut key,
                mut invalidation_keys,
                typename,
                cache_entry,
                prepared_request,
                entity_key, // TODO: for now it will always be empty
            },
            cache_control,
        ),
    ) in result.drain(..).enumerate()
    {
        let ttl = cache_control
            .ttl()
            .map(|secs| Duration::from_secs(secs as u64))
            .unwrap_or(default_subgraph_ttl);
        let should_cache_private = !cache_control.private() || private_id.is_some();

        let update_key_private = if !is_known_private && cache_control.private() {
            private_id.clone()
        } else {
            None
        };

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
                if debug {
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
                        connector_request: prepared_request
                            .as_ref()
                            .and_then(|pr| pr.transport_request.as_http())
                            .map(|r| r.inner.body().clone()),
                        subgraph_request: None,
                        source: CacheKeySource::Connector,
                        cache_control: cache_control.clone(),
                        data: serde_json_bytes::json!({"data": value.clone()}),
                    });
                }
                if !has_errors && cache_control.should_store() && should_cache_private {
                    if let Some(Value::Array(keys)) = specific_surrogate_keys {
                        invalidation_keys
                            .extend(keys.iter().filter_map(|v| v.as_str()).map(|s| s.to_owned()));
                    }
                    to_insert.push(BatchDocument {
                        control: serde_json::to_string(&cache_control)?,
                        data: serde_json::to_string(&value)?,
                        cache_key: key,
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
        let span = tracing::info_span!("response_cache.store", "kind" = "entity", "subgraph.name" = subgraph_name, "batch.size" = %batch_size);

        let batch_size_str = if batch_size <= 10 {
            "1-10"
        } else if batch_size <= 20 {
            "11-20"
        } else if batch_size <= 50 {
            "21-50"
        } else {
            "50+"
        };

        let subgraph_name = subgraph_name.to_string();
        // Write to cache in a non-awaited task so it’s on in the request’s critical path
        tokio::spawn(async move {
            let now = Instant::now();
            if let Err(err) = cache
                .insert_in_batch(to_insert, &subgraph_name)
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
                "kind" = "batch",
                "batch.size" = batch_size_str
            );
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
