use std::sync::Arc;

use hyper::Body;
use tower::{BoxError, ServiceExt};
use tower_service::Service;

use crate::Context;

use super::Plugins;

pub(crate) mod service;
//#[cfg(test)]
//mod tests;

pub(crate) use service::HttpService;

pub(crate) type BoxService = tower::util::BoxService<HttpRequest, HttpResponse, BoxError>;
pub(crate) type BoxCloneService = tower::util::BoxCloneService<HttpRequest, HttpResponse, BoxError>;
pub(crate) type ServiceResult = Result<HttpResponse, BoxError>;

#[non_exhaustive]
pub(crate) struct HttpRequest {
    pub(crate) http_request: http::Request<Body>,
    pub(crate) context: Context,
}

#[non_exhaustive]
pub(crate) struct HttpResponse {
    pub(crate) http_response: http::Response<Body>,
    pub(crate) context: Context,
}

#[derive(Clone)]
pub(crate) struct HttpServiceFactory {
    pub(crate) service: Arc<dyn MakeHttpService>,
    pub(crate) plugins: Arc<Plugins>,
}

impl HttpServiceFactory {
    pub(crate) fn new(service: Arc<dyn MakeHttpService>, plugins: Arc<Plugins>) -> Self {
        HttpServiceFactory { service, plugins }
    }

    #[cfg(test)]
    pub(crate) fn from_config(
        service: impl Into<String>,
        configuration: &crate::Configuration,
        http2: crate::plugins::traffic_shaping::Http2Config,
    ) -> Self {
        use indexmap::IndexMap;

        let service = HttpService::from_config(service, configuration, &None, http2).unwrap();

        HttpServiceFactory {
            service: Arc::new(service),
            plugins: Arc::new(IndexMap::new()),
        }
    }

    pub(crate) fn create(&self, name: &str) -> BoxService {
        let service = self.service.make();
        self.plugins
            .iter()
            .rev()
            .fold(service, |acc, (_, e)| e.http_service(name, acc))
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
