use std::collections::HashMap;
use std::ops::ControlFlow;
use std::time::Duration;

use http::header;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::Level;
use url::Url;

use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::cache::redis::RedisValue;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::Object;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::services::subgraph;
use crate::spec::TYPENAME;
use crate::Context;

const ENTITIES: &str = "_entities";
pub(crate) const REPRESENTATIONS: &str = "representations";

register_plugin!("apollo", "entity_cache", EntityCache);

struct EntityCache {
    storage: RedisCacheStorage,
    //service_name: String,
}

/// Configuration for exposing errors that originate from subgraphs
#[derive(Clone, Debug, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct Config {
    urls: Vec<Url>,
    ttl: Option<Duration>,
    timeout: Option<Duration>,
}

#[async_trait::async_trait]
impl Plugin for EntityCache {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        let storage =
            RedisCacheStorage::new(init.config.urls, init.config.ttl, init.config.timeout).await?;

        Ok(Self { storage })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let cache = self.storage.clone();
        let cache2 = self.storage.clone();
        let name = name.to_string();
        ServiceBuilder::new()
            .oneshot_checkpoint_async(move |request: subgraph::Request| {
                let name = name.clone();
                let cache = cache.clone();

                async move {
                    if !request
                        .subgraph_request
                        .body()
                        .variables
                        .contains_key(REPRESENTATIONS)
                    {
                        Ok(ControlFlow::Continue(request))
                    } else {
                        cache_call(name, cache, request).await
                    }
                }
            })
            .map_future(move |response| {
                let cache = cache2.clone();
                async move { cache_store_from_response(cache, response.await?).await }
            })
            .service(service)
            .boxed()
    }
}

async fn cache_call(
    name: String,
    cache: RedisCacheStorage,
    mut request: subgraph::Request,
) -> Result<ControlFlow<subgraph::Response, subgraph::Request>, BoxError> {
    let body = request.subgraph_request.body_mut();

    let keys = extract_cache_keys(&name, body, &request.context)?;

    let cache_result = cache
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
    let (new_representations, cache_result) =
        filter_representations(&name, representations, keys, cache_result)?;

    if !new_representations.is_empty() {
        body.variables
            .insert(REPRESENTATIONS, new_representations.into());

        request.context.private_entries.lock().insert(cache_result);

        Ok(ControlFlow::Continue(request))
    } else {
        let entities = cache_result
            .into_iter()
            .filter_map(|res| res.cache_entry)
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

async fn cache_store_from_response(
    cache: RedisCacheStorage,
    mut response: subgraph::Response,
) -> Result<subgraph::Response, BoxError> {
    if let Some(mut result_from_cache) = {
        let mut entries = response.context.private_entries.lock();
        let res = entries.remove::<Vec<IntermediateResult>>();
        drop(entries);
        res
    } {
        let mut data = response.response.body_mut().data.take();

        if let Some(mut entities) = data
            .as_mut()
            .and_then(|v| v.as_object_mut())
            .and_then(|o| o.remove(ENTITIES))
        {
            // TODO: compute TTL with cacheControl directive on the subgraph
            let ttl = None;
            let new_entities = insert_entities_in_result(
                entities
                    .as_array_mut()
                    .ok_or_else(|| FetchError::MalformedResponse {
                        reason: "expected an array of entities".to_string(),
                    })?,
                &cache,
                ttl,
                &mut result_from_cache,
            )
            .await?;

            data.as_mut()
                .and_then(|v| v.as_object_mut())
                .map(|o| o.insert(ENTITIES, new_entities.into()));
            response.response.body_mut().data = data;
        }
    }

    Ok(response)
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

pub(crate) fn hash_query(body: &graphql::Request) -> String {
    let mut digest = Sha256::new();
    digest.update(body.query.as_deref().unwrap_or("-").as_bytes());
    digest.update(&[0u8; 1][..]);
    digest.update(body.operation_name.as_deref().unwrap_or("-").as_bytes());
    digest.update(&[0u8; 1][..]);

    hex::encode(digest.finalize().as_slice())
}

pub(crate) fn hash_additional_data(body: &mut graphql::Request, context: &Context) -> String {
    let mut digest = Sha256::new();

    let repr_key = ByteString::from(REPRESENTATIONS);
    // Removing the representations variable because it's already part of the cache key
    let representations = body.variables.remove(&repr_key);
    digest.update(&serde_json::to_vec(&body.variables).unwrap());
    if let Some(representations) = representations {
        body.variables.insert(repr_key, representations);
    }

    let cache_key = context
        .private_entries
        .lock()
        .get::<CacheKeyMetadata>()
        .cloned()
        .unwrap_or_default();
    digest.update(&serde_json::to_vec(&cache_key).unwrap());

    hex::encode(digest.finalize().as_slice())
}

// build a list of keys to get from the cache in one query
fn extract_cache_keys(
    //representations: &mut Vec<Value>,
    subgraph_name: &str,
    body: &mut graphql::Request,
    context: &Context,
    //query_hash: &str,
) -> Result<Vec<String>, BoxError> {
    // hash the query and operation name
    let query_hash = hash_query(body);
    // hash more data like variables and authorization status
    let additional_data_hash = hash_additional_data(body, context);

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
        let key = format!(
            "subgraph.{}|{}|{}|{}|{}",
            subgraph_name, &typename, hashed_entity_key, query_hash, additional_data_hash
        );

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
    cache_entry: Option<Value>,
}

// build a new list of representations without the ones we got from the cache
fn filter_representations(
    subgraph_name: &str,
    representations: &mut Vec<Value>,
    keys: Vec<String>,
    mut cache_result: Vec<Option<Value>>,
) -> Result<(Vec<Value>, Vec<IntermediateResult>), BoxError> {
    let mut new_representations: Vec<Value> = Vec::new();
    let mut result = Vec::new();
    let mut cache_hit: HashMap<String, (usize, usize)> = HashMap::new();

    for ((mut representation, key), cache_entry) in representations
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

        if cache_entry.is_none() {
            cache_hit.entry(typename.clone()).or_default().1 += 1;

            representation
                .as_object_mut()
                .map(|o| o.insert(TYPENAME, opt_type));
            new_representations.push(representation);
        } else {
            cache_hit.entry(typename.clone()).or_default().0 += 1;
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
            Level::INFO,
            entity_type = ty.as_str(),
            cache_hit = hit,
            cache_miss = miss
        );
    }

    Ok((new_representations, result))
}

// fill in the entities for the response
async fn insert_entities_in_result(
    entities: &mut Vec<Value>,
    cache: &RedisCacheStorage,
    ttl: Option<Duration>,
    result: &mut Vec<IntermediateResult>,
) -> Result<Vec<Value>, BoxError> {
    let mut new_entities = Vec::new();

    let mut inserted_types: HashMap<String, usize> = HashMap::new();
    let mut to_insert: Vec<_> = Vec::new();
    let mut entities_it = entities.drain(..);

    // insert requested entities and cached entities in the same order as
    // they were requested
    for IntermediateResult {
        key,
        typename,
        cache_entry,
    } in result.drain(..)
    {
        match cache_entry {
            Some(v) => new_entities.push(v),
            None => {
                let value = entities_it
                    .next()
                    .ok_or_else(|| FetchError::MalformedResponse {
                        reason: "invalid number of entities".to_string(),
                    })?;
                *inserted_types.entry(typename).or_default() += 1;
                to_insert.push((RedisKey(key), RedisValue(value.clone())));

                new_entities.push(value);
            }
        }
    }

    if !to_insert.is_empty() {
        cache.insert_multiple(&to_insert, ttl).await;
    }

    for (ty, nb) in inserted_types {
        tracing::event!(Level::INFO, entity_type = ty.as_str(), cache_insert = nb,);
    }

    Ok(new_entities)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Key {
    #[serde(rename = "type")]
    opt_type: Option<Value>,
    id: Value,
}
