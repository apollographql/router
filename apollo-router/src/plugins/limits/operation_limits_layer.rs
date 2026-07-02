use std::sync::Arc;

use futures::FutureExt as _;
use futures::StreamExt as _;
use futures::future::BoxFuture;
use tower::Service;

use crate::graphql;
use crate::plugins::limits::RouterLimitsConfig;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::supergraph;
use crate::spec::operation_limits::OperationLimits;

/// Layer that enforces operation limits and rejects GraphQL requests that exceed the limits.
///
/// `ParsedDocument` must be available in the context. Otherwise, no limits are enforced.
pub(crate) struct EnforceOperationLimitsLayer {
    config: Arc<RouterLimitsConfig>,
}

impl EnforceOperationLimitsLayer {
    /// Create an operation limit enforcement layer based on router limits configuration.
    pub(crate) fn new(config: &RouterLimitsConfig) -> Self {
        Self {
            config: Arc::new(config.clone()),
        }
    }
}

impl<S> tower::Layer<S> for EnforceOperationLimitsLayer {
    type Service = EnforceOperationLimits<S>;

    fn layer(&self, inner: S) -> Self::Service {
        EnforceOperationLimits {
            inner,
            config: self.config.clone(),
        }
    }
}

/// Service that enforces operation limits.
///
/// # Context
/// This layer requires the following context values to be available on the request:
/// - [`ParsedDocument`] - The layer **panics** if this is not available.
///
/// This layer populates the following context values on the request:
/// - [`OperationLimits`] - This can then be used to report telemetry.
#[derive(Clone)]
pub(crate) struct EnforceOperationLimits<S> {
    inner: S,
    config: Arc<RouterLimitsConfig>,
}

impl<S> Service<supergraph::Request> for EnforceOperationLimits<S>
where
    S: Service<supergraph::Request, Response = supergraph::Response>,
    S::Error: From<http::Error> + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: supergraph::Request) -> Self::Future {
        let document = req
            .context
            .extensions()
            .with_lock(|lock| lock.get::<ParsedDocument>().cloned());
        let operation_name = req.supergraph_request.body().operation_name.as_deref();

        let Some(document) = document else {
            panic!("No document?");
        };
        let mut query_metrics = OperationLimits::default();
        let result = crate::spec::operation_limits::check(
            &mut query_metrics,
            &self.config,
            &document.executable,
            operation_name,
        );

        // Stash the measurements in context so they can be used for telemetry.
        req.context.extensions().with_lock(|lock| {
            let _ = lock.insert(query_metrics);
        });

        if let Err(OperationLimits {
            depth,
            height,
            root_fields,
            aliases,
        }) = result
        {
            let mut errors = Vec::new();
            let mut build = |exceeded, code, message| {
                if exceeded {
                    errors.push(
                        graphql::Error::builder()
                            .message(message)
                            .extension_code(code)
                            .build(),
                    )
                }
            };
            build(
                depth,
                "MAX_DEPTH_LIMIT",
                "Maximum depth limit exceeded in this operation",
            );
            build(
                height,
                "MAX_HEIGHT_LIMIT",
                "Maximum height (field count) limit exceeded in this operation",
            );
            build(
                root_fields,
                "MAX_ROOT_FIELDS_LIMIT",
                "Maximum root fields limit exceeded in this operation",
            );
            build(
                aliases,
                "MAX_ALIASES_LIMIT",
                "Maximum aliases limit exceeded in this operation",
            );
            let graphql_response = graphql::Response::builder().errors(errors).build();
            let response = http::Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .body(futures::stream::once(std::future::ready(graphql_response)).boxed())
                .map_err(Self::Error::from)
                .map(|http_response| supergraph::Response {
                    response: http_response,
                    context: req.context,
                });

            return std::future::ready(response).boxed();
        }

        self.inner.call(req).boxed()
    }
}
