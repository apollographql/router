use std::{
    future::{ready, Ready},
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};
use serde_json_bytes::Value;
use tower::{Layer, Service, ServiceExt};

use crate::{cache::storage::CacheStorage, services::subgraph};

#[derive(Clone)]
pub(crate) struct SubgraphCacheLayer {
    storage: CacheStorage<String, Value>,
    name: String,
}

impl SubgraphCacheLayer {
    pub(crate) async fn new(name: String) -> Self {
        SubgraphCacheLayer {
            storage: CacheStorage::new(1024).await,
            name,
        }
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
    S: Service<subgraph::Request, Response = subgraph::Response> + Clone + Send + 'static,
    S::Error: Into<tower::BoxError> + std::fmt::Debug,
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
    /*Either<
        BoxFuture<'static, Result<S::Response, <S as Service<subgraph::Request>>::Error>>,
        Oneshot<S, subgraph::Request>,
    >*/
    //type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: subgraph::Request) -> Self::Future {
        /*let response = self.inner.call(request);

        ResponseFuture::new(
            response,
            self.sleep
                .take()
                .expect("poll_ready must been called before"),
        )*/

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
        Box::pin(async move {
            let body = request.subgraph_request.body_mut();
            let reps = body
                .variables
                .get_mut("representations")
                .and_then(|value| value.as_array_mut());

            let mut reps = reps.unwrap();

            let mut new_reps: Vec<Value> = Vec::new();
            let mut result: Vec<(String, Option<Value>)> = Vec::new();
            for mut representation in reps.drain(..) {
                let opt_type: Option<Value> = representation
                    .as_object_mut()
                    .and_then(|o| o.remove("__typename"));
                //and_then(|v| serde_json::from_value(v).ok());

                //FIXME: add the query
                let key = format!(
                    "subgraph.{}|{}|{}|{}",
                    name,
                    opt_type.as_ref().and_then(|v| v.as_str()).unwrap_or("-"),
                    serde_json::to_string(&representation).unwrap(),
                    body.query.as_deref().unwrap_or("-")
                );

                let res = cache.get(&key).await;
                if res.is_none() {
                    println!("cache miss for {key}");
                    representation
                        .as_object_mut()
                        .map(|o| o.insert("__typename", opt_type.unwrap()));
                    new_reps.push(representation);
                } else {
                    println!("cache hit for {key}");
                }
                result.push((key, res));
            }

            body.variables.insert("representations", new_reps.into());

            /*FIXME: cache results
            match service.oneshot(request).await {

            }*/
            let mut response = service.oneshot(request).await.unwrap();

            let mut data = response.response.body_mut().data.take();

            if let Some(mut entities) = data
                .as_mut()
                .and_then(|v| v.as_object_mut())
                .and_then(|o| o.remove("_entities"))
            {
                let mut new_entities = Vec::new();
                let mut entities_it = entities.as_array_mut().unwrap().drain(..);

                // insert requested entities and cached entities in the same order as
                // they were requested
                for (key, entity) in result.drain(..) {
                    match entity {
                        Some(v) => new_entities.push(v),
                        None => {
                            let value = entities_it.next().unwrap();
                            println!(
                                "cache insert for {key}: {}",
                                serde_json::to_string(&value).unwrap()
                            );
                            cache.insert(key, value.clone()).await;

                            new_entities.push(value);
                        }
                    }
                }

                //FIXME: check that entities_it is now empty
                data.as_mut()
                    .and_then(|v| v.as_object_mut())
                    .map(|o| o.insert("_entities", new_entities.into()));
            }

            response.response.body_mut().data = data;
            Ok(response)
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Key {
    #[serde(rename = "type")]
    opt_type: Option<Value>,
    id: Value,
}
