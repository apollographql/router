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

/// Marks a response whose headers arrived but whose body did not: the connection failed while
/// the body was being read.
///
/// The subgraph and connector services keep the status the headers carried, so without this a
/// `200` whose body was cut off is indistinguishable from one that was answered in full. They
/// insert it into the response's `http` extensions, where the circuit breaker reads it to count
/// the transport failure against the target. A body the router cut short itself, at a
/// configured response size limit, is not marked: that is the router's decision, not a failure
/// of the target.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IncompleteResponseBody;

/// Marks a subgraph response the router produced without calling the subgraph: a coprocessor or
/// plugin broke the request, or the response cache answered it.
///
/// Such a response says nothing about the subgraph's health, so the circuit breaker records no
/// outcome for it. It lives in the response's `http` extensions, like [`IncompleteResponseBody`].
/// A connector response carries the same fact as its `answered_by_router` field.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AnsweredByRouter;

/// Marks a subgraph response to a fetch that failed because the client's file upload, which the
/// router was streaming to the subgraph, failed part way through.
///
/// The failure is the client's, not the subgraph's, so the circuit breaker records no outcome for
/// it, whatever status the response carries.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UploadStreamFailed;

/// The error an HTTP fetch fails with when the client's file upload it was streaming failed part
/// way through. It reads exactly like the error it wraps, and only tells the subgraph service to
/// mark its response with [`UploadStreamFailed`].
#[derive(Debug)]
pub(crate) struct UploadStreamError(pub(crate) BoxError);

impl std::fmt::Display for UploadStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for UploadStreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
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
    crate::pipeline::build_http_client_service(
        name,
        inputs,
        Arc::new(indexmap::IndexMap::default()),
    )
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
