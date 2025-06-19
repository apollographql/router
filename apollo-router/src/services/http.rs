#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use super::Plugins;
use super::router::body::RouterBody;
use crate::Context;

pub(crate) mod service;
#[cfg(test)]
mod tests;

pub(crate) use service::HttpClientService;

use crate::layers::ServiceBuilderExt;

pub(crate) type BoxService = tower::util::BoxService<HttpRequest, HttpResponse, BoxError>;
pub(crate) type BoxCloneService = tower::util::BoxCloneService<HttpRequest, HttpResponse, BoxError>;
pub(crate) type BoxCloneSyncService =
    tower::util::BoxCloneSyncService<HttpRequest, HttpResponse, BoxError>;
pub(crate) type ServiceResult = Result<HttpResponse, BoxError>;

// Use ArcSwap to avoid locking on the cache.
// Updates are very infrequent, so we take advantage of that to:
//  - Return a clone of the service if the key exists (fast path)
//  - If the key doesn't exist: (slow path)
//    - Perform the slow creation of the new service
//    - Use the `rcu` method to update the cache
// `rcu` will repeatedly execute until the update succeeds at which point our new cache will be
// available for all readers.
type ServiceCache = ArcSwap<HashMap<String, BoxCloneSyncService>>;

#[non_exhaustive]
pub(crate) struct HttpRequest {
    pub(crate) http_request: http::Request<RouterBody>,
    pub(crate) context: Context,
}

#[non_exhaustive]
pub(crate) struct HttpResponse {
    pub(crate) http_response: http::Response<RouterBody>,
    pub(crate) context: Context,
}

pub(crate) struct HttpClientServiceFactory {
    pub(crate) service: HttpClientService,
    pub(crate) plugins: Arc<Plugins>,
    cache: ServiceCache,
}

// We can't clone ArcSwap, but we can give each factory its own copy
impl Clone for HttpClientServiceFactory {
    fn clone(&self) -> Self {
        let cache = ArcSwap::new(self.cache.load_full());
        Self {
            service: self.service.clone(),
            plugins: self.plugins.clone(),
            cache,
        }
    }
}

impl HttpClientServiceFactory {
    pub(crate) fn new(service: HttpClientService, plugins: Arc<Plugins>) -> Self {
        HttpClientServiceFactory {
            service,
            plugins,
            cache: Default::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_config(
        service: impl Into<String>,
        configuration: &crate::Configuration,
        client_config: crate::configuration::shared::Client,
    ) -> Self {
        use indexmap::IndexMap;

        let service = HttpClientService::from_config_for_subgraph(
            service,
            configuration,
            &rustls::RootCertStore::empty(),
            client_config,
        )
        .unwrap();

        HttpClientServiceFactory {
            service,
            plugins: Arc::new(IndexMap::default()),
            cache: Default::default(),
        }
    }

    pub(crate) fn create(&self, name: &str) -> BoxCloneSyncService {
        // Check if we already have a memoized service for this name
        if let Some(service) = self.cache.load().get(name) {
            service.clone()
        } else {
            // Create the service if not cached
            let service = self
                .plugins
                .iter()
                .rev()
                .fold(self.service.clone().boxed(), |acc, (_, e)| {
                    e.http_client_service(name, acc)
                });
            let buffered_clone_service = ServiceBuilder::new().buffered().service(service);

            let boxed_clone_sync_service = BoxCloneSyncService::new(buffered_clone_service);

            self.cache.rcu(|cache| {
                let mut cache = HashMap::clone(cache);
                cache.insert(name.to_string(), boxed_clone_sync_service.clone());
                cache
            });

            boxed_clone_sync_service
        }
    }

    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.cache.load().len()
    }

    #[cfg(test)]
    pub(crate) fn has_cached_service(&self, name: &str) -> bool {
        self.cache.load().contains_key(name)
    }
}
