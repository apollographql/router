#![allow(dead_code)]
use std::sync::Arc;

use tower::BoxError;

use super::router::body::RouterBody;
use crate::Context;

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

/// Test-only wrapper around the `build_http_client_service` pipeline function: a
/// subgraph client for `name` built from default configuration with no plugins.
#[cfg(test)]
pub(crate) fn test_http_client_service(name: &str) -> BoxCloneService {
    let inputs = service::HttpClientInputs::for_subgraph(
        name,
        &crate::Configuration::default(),
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
        &mut service::DnsResolverCache::default(),
    )
    .unwrap();
    crate::pipeline::build_http_client_service(name, inputs, Arc::new(indexmap::IndexMap::default()))
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
