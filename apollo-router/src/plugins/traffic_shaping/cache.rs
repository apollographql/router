use std::collections::HashMap;
use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;
use tracing::Level;

use crate::cache::storage::CacheStorage;
use crate::error::FetchError;
use crate::services::subgraph;

#[derive(Clone)]
pub(crate) struct SubgraphCacheLayer {
    storage: CacheStorage<String, Value>,
    name: String,
}

impl SubgraphCacheLayer {
    pub(crate) fn new_with_storage(name: String, storage: CacheStorage<String, Value>) -> Self {
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
    storage: CacheStorage<String, Value>,
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
    cache: CacheStorage<String, Value>,
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
    let reps = body
        .variables
        .get_mut("representations")
        .and_then(|value| value.as_array_mut())
        .expect("we already checked that representations exist");

    let mut new_reps: Vec<Value> = Vec::new();
    let mut result: Vec<(String, String, Option<Value>)> = Vec::new();
    let mut cache_hit: HashMap<String, (usize, usize)> = HashMap::new();

    for mut representation in reps.drain(..) {
        let opt_type = representation
            .as_object_mut()
            .and_then(|o| o.remove("__typename"))
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "missing __typename in representation".to_string(),
            })?;

        let typename = opt_type.as_str().unwrap_or("-").to_string();

        let key = format!(
            "subgraph.{}|{}|{}|{}",
            name,
            &typename,
            serde_json::to_string(&representation).unwrap(),
            body.query.as_deref().unwrap_or("-")
        );

        let res = cache.get(&key).await;
        if res.is_none() {
            cache_hit.entry(typename.clone()).or_default().1 += 1;

            representation
                .as_object_mut()
                .map(|o| o.insert("__typename", opt_type));
            new_reps.push(representation);
        } else {
            cache_hit.entry(typename.clone()).or_default().0 += 1;
        }
        result.push((key, typename, res));
    }

    for (ty, (hit, miss)) in cache_hit {
        tracing::event!(
            Level::INFO,
            entity_type = ty.as_str(),
            cache_hit = hit,
            cache_miss = miss
        );
    }

    body.variables.insert("representations", new_reps.into());

    let mut response = service.oneshot(request).await?;

    let mut data = response.response.body_mut().data.take();

    if let Some(mut entities) = data
        .as_mut()
        .and_then(|v| v.as_object_mut())
        .and_then(|o| o.remove("_entities"))
    {
        let mut new_entities = Vec::new();
        let mut entities_it = entities
            .as_array_mut()
            .ok_or_else(|| FetchError::MalformedResponse {
                reason: "expected an array of entities".to_string(),
            })?
            .drain(..);

        let mut inserted: HashMap<String, usize> = HashMap::new();
        // insert requested entities and cached entities in the same order as
        // they were requested
        for (key, typename, entity) in result.drain(..) {
            match entity {
                Some(v) => new_entities.push(v),
                None => {
                    let value =
                        entities_it
                            .next()
                            .ok_or_else(|| FetchError::MalformedResponse {
                                reason: "invalid number of entities".to_string(),
                            })?;
                    *inserted.entry(typename).or_default() += 1;
                    cache.insert(key, value.clone()).await;

                    new_entities.push(value);
                }
            }
        }

        for (ty, nb) in inserted {
            tracing::event!(Level::INFO, entity_type = ty.as_str(), cache_insert = nb,);
        }

        //FIXME: check that entities_it is now empty
        data.as_mut()
            .and_then(|v| v.as_object_mut())
            .map(|o| o.insert("_entities", new_entities.into()));
    }

    response.response.body_mut().data = data;
    Ok(response)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Key {
    #[serde(rename = "type")]
    opt_type: Option<Value>,
    id: Value,
}
