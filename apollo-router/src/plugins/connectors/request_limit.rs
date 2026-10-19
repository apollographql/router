//! Limits on Connectors requests

use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use apollo_federation::connectors::Connector;
use apollo_federation::connectors::SourceName;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::errors::Error;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use futures::future::BoxFuture;
use futures::future::Either;
use parking_lot::Mutex;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::plugins::connectors::handle_responses::process_response;
use crate::services::connector::request_service::Request;
use crate::services::connector::request_service::Response;
use crate::services::router::body::RouterBody;

/// Key to access request limits for a connector
#[derive(Eq, Hash, PartialEq)]
pub(crate) enum RequestLimitKey {
    /// A key to access the request limit for a connector referencing a source directive
    SourceName(SourceName),

    /// A key to access the request limit for a connector without a corresponding source directive
    ConnectorLabel(String),
}

impl Display for RequestLimitKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestLimitKey::SourceName(source_name) => {
                write!(f, "connector source {source_name}")
            }
            RequestLimitKey::ConnectorLabel(connector_label) => {
                write!(f, "connector {connector_label}")
            }
        }
    }
}

impl From<&Connector> for RequestLimitKey {
    fn from(value: &Connector) -> Self {
        value
            .id
            .source_name
            .as_ref()
            .map(|source_name| RequestLimitKey::SourceName(source_name.clone()))
            .unwrap_or(RequestLimitKey::ConnectorLabel(value.label.0.clone()))
    }
}

/// Tracks a request limit for a connector
pub(crate) struct RequestLimit {
    max_requests: usize,
    total_requests: AtomicUsize,
}

impl RequestLimit {
    pub(crate) fn new(max_requests: usize) -> Self {
        Self {
            max_requests,
            total_requests: AtomicUsize::new(0),
        }
    }

    pub(crate) fn allow(&self) -> bool {
        self.total_requests.fetch_add(1, Ordering::Relaxed) < self.max_requests
    }
}

/// Tracks the request limits for an operation
pub(crate) struct RequestLimits {
    default_max_requests: Option<usize>,
    limits: Mutex<HashMap<RequestLimitKey, Arc<RequestLimit>>>,
}

impl RequestLimits {
    pub(crate) fn new(default_max_requests: Option<usize>) -> Self {
        Self {
            default_max_requests,
            limits: Mutex::new(HashMap::new()),
        }
    }

    #[allow(clippy::unwrap_used)] // Unwrap checked by invariant
    pub(crate) fn get(
        &self,
        key: RequestLimitKey,
        limit: Option<usize>,
    ) -> Option<Arc<RequestLimit>> {
        if limit.is_none() && self.default_max_requests.is_none() {
            return None;
        }
        Some(
            self.limits
                .lock()
                .entry(key)
                .or_insert_with(|| {
                    Arc::new(RequestLimit::new(
                        limit.or(self.default_max_requests).unwrap(),
                    ))
                }) // unwrap ok, invariant checked above
                .clone(),
        )
    }

    pub(crate) fn log(&self) {
        self.limits.lock().iter().for_each(|(key, limit)| {
            let total = limit.total_requests.load(Ordering::Relaxed);
            if total > limit.max_requests {
                tracing::warn!(
                    "Request limit exceeded for {}: max: {}, attempted: {}",
                    key,
                    limit.max_requests,
                    total,
                );
            }
        });
    }
}

/// Rejects a connector request once its operation has used up the connector's `max_requests`,
/// without sending it.
///
/// Placed with traffic shaping's admission, above every plugin hook: a request over the limit is
/// the router declining to send it, so nothing beneath this layer sees it.
#[derive(Clone, Copy, Default)]
pub(crate) struct RequestLimitLayer;

impl<S> Layer<S> for RequestLimitLayer {
    type Service = RequestLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestLimitService { inner }
    }
}

/// Service type for [`RequestLimitLayer`].
#[derive(Clone)]
pub(crate) struct RequestLimitService<S> {
    inner: S,
}

impl<S> Service<Request> for RequestLimitService<S>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    type Response = Response;
    type Error = BoxError;
    type Future = Either<S::Future, BoxFuture<'static, Result<Response, BoxError>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // Mapping-only connectors make no request, so they are never limited.
        let limit = match request.transport_request {
            TransportRequest::Http(_) => request.context.extensions().with_lock(|lock| {
                lock.get::<Arc<RequestLimits>>().and_then(|limits| {
                    limits.get(
                        request.connector.as_ref().into(),
                        request.connector.max_requests,
                    )
                })
            }),
            TransportRequest::MappingOnly => None,
        };

        if limit.is_none_or(|limit| limit.allow()) {
            return Either::Left(self.inner.call(request));
        }

        // Recorded for connector debugging like any other request, with no request sent.
        let debug = request
            .context
            .extensions()
            .with_lock(|lock| lock.get::<Arc<Mutex<ConnectorContext>>>().cloned());

        Either::Right(Box::pin(async move {
            Ok(process_response::<RouterBody>(
                Err(Error::RequestLimitExceeded),
                request.key,
                request.connector,
                &request.context,
                (None, Default::default()),
                debug.as_ref(),
                request.supergraph_request,
                request.operation,
            )
            .await)
        }))
    }
}
