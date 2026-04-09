#![allow(dead_code)]
use std::sync::Arc;

use tower::BoxError;
use tower::ServiceExt;

use super::Plugins;
use super::router::body::RouterBody;
use crate::Context;

pub(crate) mod service;
#[cfg(test)]
mod tests;

pub(crate) use service::HttpClientService;

pub(crate) type BoxCloneService = tower::util::BoxCloneService<HttpRequest, HttpResponse, BoxError>;
pub(crate) type ServiceResult = Result<HttpResponse, BoxError>;

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
}

impl HttpClientServiceFactory {
    pub(crate) fn new(service: HttpClientService, plugins: Arc<Plugins>) -> Self {
        HttpClientServiceFactory { service, plugins }
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
        }
    }

    pub(crate) fn create(&self, name: &str) -> BoxCloneService {
        let service = self.service.clone();
        self.plugins
            .iter()
            .rev()
            .fold(service.boxed_clone(), |acc, (_, e)| {
                e.http_client_service(name, acc)
            })
    }
}
