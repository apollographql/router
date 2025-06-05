use futures::FutureExt as _;
use futures::StreamExt as _;
use futures::future::BoxFuture;
use tower::Service;

use crate::graphql;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::supergraph;
use crate::spec::operation_limits::OperationLimits;

/// Layer that enforces operation limits and rejects GraphQL requests that exceed the limits.
///
/// `ParsedDocument` must be available in the context. Otherwise, no limits are enforced.
pub(crate) struct EnforceOperationLimitsLayer {
    config: crate::plugins::limits::Config,
}

impl EnforceOperationLimitsLayer {
    pub(crate) fn new(config: &crate::plugins::limits::Config) -> Self {
        Self {
            config: config.clone(),
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
/// `ParsedDocument` must be available in the context. Otherwise, no limits are enforced.
#[derive(Clone)]
pub(crate) struct EnforceOperationLimits<S> {
    inner: S,
    config: crate::plugins::limits::Config,
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

        if let Some(document) = document {
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
        } else {
            panic!("No document?");
        }

        self.inner.call(req).boxed()
    }
}
