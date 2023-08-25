use std::collections::HashMap;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;
use tracing::Level;

use crate::cache::redis::RedisCacheStorage;
use crate::cache::redis::RedisKey;
use crate::cache::redis::RedisValue;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::Object;
use crate::services::subgraph;
use crate::spec::TYPENAME;

#[derive(Clone)]
pub(crate) struct SubgraphCacheLayer {
    storage: RedisCacheStorage,
    name: String,
}

impl SubgraphCacheLayer {
    pub(crate) fn new_with_storage(
        name: String,
        mut storage: RedisCacheStorage,
        ttl: Duration,
    ) -> Self {
        storage.set_ttl(Some(ttl));
        SubgraphCacheLayer { storage, name }
    }
}

impl<S: Clone> Layer<S> for SubgraphCacheLayer {
    type Service = SubgraphCache<S>;

    fn layer(&self, service: S) -> Self::Service {
        SubgraphCache {
            name: self.name.clone(),
            storage: self.storage.clone(),
            service,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SubgraphCache<S: Clone> {
    storage: RedisCacheStorage,
    name: String,
    service: S,
}

impl<S> Service<subgraph::Request> for SubgraphCache<S>
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as Service<subgraph::Request>>::Future: std::marker::Send,
{
    type Response = <S as Service<subgraph::Request>>::Response;
    type Error = <S as Service<subgraph::Request>>::Error;
    type Future = BoxFuture<
        'static,
        Result<
            <S as Service<subgraph::Request>>::Response,
            <S as Service<subgraph::Request>>::Error,
        >,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: subgraph::Request) -> Self::Future {
        let service = self.service.clone();

        if !request
            .subgraph_request
            .body_mut()
            .variables
            .contains_key("representations")
        {
            return service.oneshot(request).boxed();
        }

        let cache = self.storage.clone();
        let name = self.name.clone();
        Box::pin(cache_call(service, name, cache, request))
    }
}

async fn cache_call<S>(
    service: S,
    name: String,
    cache: RedisCacheStorage,
    mut request: subgraph::Request,
) -> Result<<S as Service<subgraph::Request>>::Response, <S as Service<subgraph::Request>>::Error>
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Error: Into<tower::BoxError> + std::fmt::Debug,
    <S as Service<subgraph::Request>>::Future: std::marker::Send,
{
    let body = request.subgraph_request.body_mut();
    let query_hash = hash_request(body);

    let representations = body
        .variables
        .get_mut("representations")
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");

    let keys = extract_cache_keys(representations, &name, &query_hash)?;
    let cache_result = cache
        .get_multiple(keys.iter().map(|k| RedisKey(k.clone())).collect::<Vec<_>>())
        .await
        .map(|res| res.into_iter().map(|r| r.map(|v| v.0)).collect())
        .unwrap_or_else(|| std::iter::repeat(None).take(keys.len()).collect());

    let (new_representations, mut result) =
        filter_representations(representations, keys, cache_result)?;

    if !new_representations.is_empty() {
        body.variables
            .insert("representations", new_representations.into());

        let mut response = service.oneshot(request).await?;

        let mut data = response.response.body_mut().data.take();

        if let Some(mut entities) = data
            .as_mut()
            .and_then(|v| v.as_object_mut())
            .and_then(|o| o.remove("_entities"))
        {
            let new_entities = insert_entities_in_result(
                entities
                    .as_array_mut()
                    .ok_or_else(|| FetchError::MalformedResponse {
                        reason: "expected an array of entities".to_string(),
                    })?,
                &cache,
                &mut result,
            )
            .await?;

            data.as_mut()
                .and_then(|v| v.as_object_mut())
                .map(|o| o.insert("_entities", new_entities.into()));
            response.response.body_mut().data = data;
        }

        Ok(response)
    } else {
        let entities = insert_entities_in_result(&mut Vec::new(), &cache, &mut result).await?;
        let mut data = Object::default();
        data.insert("_entities", entities.into());

        Ok(subgraph::Response::builder()
            .data(data)
            .extensions(Object::new())
            .context(request.context)
            .build())
    }
}

fn hash_request(body: &graphql::Request) -> String {
    let mut digest = Sha256::new();
    digest.update(body.query.as_deref().unwrap_or("-").as_bytes());
    digest.update(&[0u8; 1][..]);
    digest.update(body.operation_name.as_deref().unwrap_or("-").as_bytes());
    digest.update(&[0u8; 1][..]);
    digest.update(&serde_json::to_vec(&body.variables).unwrap());

    hex::encode(digest.finalize().as_slice())
}

// build a list of keys to get from the cache in one query
fn extract_cache_keys(
    representations: &mut Vec<Value>,
    subgraph_name: &str,
    query_hash: &str,
) -> Result<Vec<String>, BoxError> {
    let mut res = Vec::new();
    for representation in representations {
        let opt_type = representation
            .as_object_mut()
            .and_then(|o| o.remove(TYPENAME))
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "missing __typename in representation".to_string(),
            })?;

        let typename = opt_type.as_str().unwrap_or("-").to_string();

        let key = format!(
            "subgraph.{}|{}|{}|{}",
            subgraph_name,
            &typename,
            serde_json::to_string(&representation).unwrap(),
            query_hash
        );

        representation
            .as_object_mut()
            .map(|o| o.insert("__typename", opt_type));
        res.push(key);
    }
    Ok(res)
}

struct IntermediateResult {
    key: String,
    typename: String,
    cache_entry: Option<Value>,
}

// build a new list of representations without the ones we got from the cache
fn filter_representations(
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
            .and_then(|o| o.remove("__typename"))
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "missing __typename in representation".to_string(),
            })?;

        let typename = opt_type.as_str().unwrap_or("-").to_string();

        if cache_entry.is_none() {
            cache_hit.entry(typename.clone()).or_default().1 += 1;

            representation
                .as_object_mut()
                .map(|o| o.insert("__typename", opt_type));
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
        tracing::event!(
            Level::INFO,
            entity_type = ty.as_str(),
            cache_hit = hit,
            cache_miss = miss
        );
    }

    Ok((new_representations, result))
}

async fn insert_entities_in_result(
    entities: &mut Vec<Value>,
    cache: &RedisCacheStorage,
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
        cache.insert_multiple(&to_insert).await;
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
