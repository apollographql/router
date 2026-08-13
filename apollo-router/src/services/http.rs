#![allow(dead_code)]
use std::sync::Arc;

use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use super::Plugins;
use super::router::body::RouterBody;
use crate::Configuration;
use crate::Context;
use crate::batching::JoinBatchRequestsLayer;
use crate::layers::InternalServiceBuilderExt as _;
use crate::plugins::limits::response_size_limit::SubgraphResponseSizeLimitLayer;

pub(crate) mod connection_timing;
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
    pub(crate) configuration: Arc<Configuration>,
}

impl HttpClientServiceFactory {
    pub(crate) fn new(
        service: HttpClientService,
        plugins: Arc<Plugins>,
        configuration: Arc<Configuration>,
    ) -> Self {
        HttpClientServiceFactory {
            service,
            plugins,
            configuration,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_config(
        service_name: &str,
        configuration: &crate::Configuration,
        client_config: crate::configuration::shared::Client,
    ) -> Self {
        use indexmap::IndexMap;

        let service = HttpClientService::from_config_for_subgraph(
            service_name,
            configuration,
            &rustls::RootCertStore::empty(),
            client_config,
        )
        .unwrap();

        HttpClientServiceFactory {
            service,
            plugins: Arc::new(IndexMap::default()),
            configuration: Arc::new(configuration.clone()),
        }
    }

    pub(crate) fn create(&self, name: &str) -> BoxCloneService {
        ServiceBuilder::new()
            .option_layer(
                self.configuration
                    .batching
                    .batch_include(name)
                    .then(|| JoinBatchRequestsLayer::new(name)),
            )
            .layer(SubgraphResponseSizeLimitLayer::new(name))
            .rust_plugins(self.plugins.clone(), |plugin, service| {
                plugin.http_client_service(name, service)
            })
            .service(self.service.clone())
            .boxed_clone()
    }

    #[cfg(test)]
    pub(crate) fn for_test(name: &str) -> BoxCloneService {
        Self::from_config(
            name,
            &crate::Configuration::default(),
            crate::configuration::shared::Client::default(),
        )
        .create(name)
    }
}

/// The kind of remote service an [`HttpClientService`] is configured to talk to.
///
/// Used by [`service::HttpClientService`] to derive the service name and by
/// [`connection_timing::ConnectionTimingConnector`] to select the OTel attributes emitted on the
/// `apollo.router.connection.acquire.duration` histogram.
#[derive(Clone)]
enum ServiceTarget {
    /// A coprocessor: emits `coprocessor = true`.
    Coprocessor,
    /// A subgraph: emits `subgraph.name = name`.
    Subgraph { name: Arc<str> },
    /// A connector source: emits `connector.source.name = name`.
    Connector { name: Arc<str> },
}
