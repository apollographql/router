#![allow(dead_code)]
use std::sync::Arc;

use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use super::Plugins;
use super::router::body::RouterBody;
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

/// Assembles the HTTP client service stack for one target, outermost first: the
/// batching and response-size-limit layers, each plugin's `http_client_service` hook,
/// and the [`HttpClientService`] built from `inputs`.
///
/// `name` is the subgraph name — for a connector source's client, the name of the
/// subgraph that owns the source — and is what the plugin hook receives.
pub(crate) fn build_http_client_service(
    name: &str,
    inputs: service::HttpClientInputs,
    plugins: Arc<Plugins>,
) -> BoxCloneService {
    ServiceBuilder::new()
        .layer(JoinBatchRequestsLayer::new(name))
        .layer(SubgraphResponseSizeLimitLayer::new(name))
        .rust_plugins(plugins, |plugin, service| {
            plugin.http_client_service(name, service)
        })
        .service(HttpClientService::new(inputs))
        .boxed_clone()
}

/// Test-only [`build_http_client_service`] wrapper: a subgraph client for `name` built
/// from default configuration with no plugins.
#[cfg(test)]
pub(crate) fn test_http_client_service(name: &str) -> BoxCloneService {
    let inputs = service::HttpClientInputs::for_subgraph(
        name,
        &crate::Configuration::default(),
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
    )
    .unwrap();
    build_http_client_service(name, inputs, Arc::new(indexmap::IndexMap::default()))
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
