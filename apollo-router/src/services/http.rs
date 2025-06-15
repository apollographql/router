#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use futures::future::BoxFuture;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::buffer::Buffer;
use tower_service::Service;

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
pub(crate) type ServiceResult = Result<HttpResponse, BoxError>;
type MemoizedService = Arc<Buffer<HttpRequest, BoxFuture<'static, Result<HttpResponse, BoxError>>>>;
type ServiceCache = Arc<RwLock<HashMap<String, MemoizedService>>>;

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

#[derive(Clone)]
pub(crate) struct HttpClientServiceFactory {
    pub(crate) service: HttpClientService,
    pub(crate) plugins: Arc<Plugins>,
    memoized: ServiceCache,
}

impl HttpClientServiceFactory {
    pub(crate) fn new(service: HttpClientService, plugins: Arc<Plugins>) -> Self {
        HttpClientServiceFactory {
            service,
            plugins,
            memoized: Arc::new(Default::default()),
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
            memoized: Arc::new(Default::default()),
        }
    }

    pub(crate) fn create(&self, name: &str) -> BoxService {
        // Check if we already have a memoized service for this name
        {
            let cache = self.memoized.read().unwrap();
            if let Some(service) = cache.get(name) {
                return (**service).clone().boxed();
            }
        }

        // Create the service if not cached
        let service = self.service.clone();
        let service = self
            .plugins
            .iter()
            .rev()
            .fold(service.boxed(), |acc, (_, e)| {
                e.http_client_service(name, acc)
            });
        let buffered_clone_service = ServiceBuilder::new().buffered().service(service);

        // Store in cache
        let arc_service = Arc::new(buffered_clone_service.clone());
        {
            let mut cache = self.memoized.write().unwrap();
            cache.insert(name.to_string(), arc_service);
        }

        buffered_clone_service.boxed()
    }
}

pub(crate) trait MakeHttpService: Send + Sync + 'static {
    fn make(&self) -> BoxService;
}

impl<S> MakeHttpService for S
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <S as Service<HttpRequest>>::Future: Send,
{
    fn make(&self) -> BoxService {
        self.clone().boxed()
    }
}
