use std::sync::Arc;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use tower::Service;

use crate::graphql;
use crate::plugins::limits::RouterLimitsConfig;
use crate::plugins::limits::operation_limits;
use crate::plugins::limits::operation_limits::OperationLimits;
use crate::services::query_parsing::ParsedDocument;
use crate::services::supergraph;

/// Layer that enforces operation limits and rejects GraphQL requests that exceed the limits.
///
/// # Context
/// This layer requires the following context values to be available on the request:
/// - [`ParsedDocument`] - An error is returned if the document is missing.
///
/// This layer populates the following context values on the request:
/// - [`OperationLimits`] - This can then be used to report telemetry.
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
#[derive(Clone)]
pub(crate) struct EnforceOperationLimits<S> {
    inner: S,
    config: Arc<RouterLimitsConfig>,
}

impl<S> Service<supergraph::Request> for EnforceOperationLimits<S>
where
    S: Service<supergraph::Request, Response = supergraph::Response> + Clone + Send + 'static,
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
        let config = self.config.clone();
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            let Some(document) = req
                .context
                .extensions()
                .with_lock(|lock| lock.get::<ParsedDocument>().cloned())
            else {
                // We shouldn't ever reach here unless the pipeline was set up
                // improperly (i.e. programmer error), but do something better than
                // panicking just in case.
                return Ok(supergraph::Response::error_builder()
                    .status_code(http::StatusCode::INTERNAL_SERVER_ERROR)
                    .context(req.context)
                    .error(
                        graphql::Error::builder()
                            .message("Cannot find executable document")
                            .extension_code("MISSING_EXECUTABLE_DOCUMENT")
                            .build(),
                    )
                    .build()
                    .expect("body is valid"));
            };

            let mut query_metrics = OperationLimits::default();

            let max = OperationLimits {
                depth: config.max_depth,
                height: config.max_height,
                root_fields: config.max_root_fields,
                aliases: config.max_aliases,
            };
            let result = operation_limits::check(
                &mut query_metrics,
                max,
                &document.executable,
                document.operation.name.as_deref(),
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
                && !config.warn_only
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

                return http::Response::builder()
                    .status(http::StatusCode::BAD_REQUEST)
                    .body(futures::stream::once(std::future::ready(graphql_response)).boxed())
                    .map_err(Self::Error::from)
                    .map(|http_response| supergraph::Response {
                        response: http_response,
                        context: req.context,
                    });
            }

            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Context;
    use crate::services::supergraph;
    use crate::spec::Query;
    use crate::spec::Schema;
    use crate::test_harness::tracing_test;

    /// Build a supergraph request for a query.
    fn make_request(schema: &Schema, query: &str) -> supergraph::Request {
        // In the future, we can hopefully just use a query parsing tower layer here...
        let doc = Query::parse_document(query, None, schema, &Default::default()).unwrap();
        let ctx = Context::new();
        ctx.extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(doc));
        supergraph::Request::fake_builder()
            .query(query)
            .context(ctx)
            .build()
            .unwrap()
    }

    fn error_codes(response: &graphql::Response) -> Vec<&str> {
        response
            .errors
            .iter()
            .filter_map(|e| e.extensions.get("code")?.as_str())
            .collect()
    }

    #[tokio::test]
    async fn test_under_limits() {
        let schema = Schema::parse(
            include_str!("../../testdata/supergraph.graphql"),
            &Default::default(),
        )
        .unwrap();
        let config = RouterLimitsConfig {
            max_root_fields: Some(1),
            max_aliases: Some(2),
            max_depth: Some(3),
            max_height: Some(4),
            ..Default::default()
        };

        let (mock, mut handle) =
            tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_response(supergraph::Response::fake_builder().build().unwrap());
        });

        let service = ServiceBuilder::new()
            .layer(EnforceOperationLimitsLayer::new(&config))
            .service(mock);

        let mut response = service
            .oneshot(make_request(&schema, "{ me { id } }"))
            .await
            .unwrap();

        let body = response.next_response().await.unwrap();
        assert!(body.errors.is_empty());

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn test_max_root_fields() {
        let schema = Schema::parse(
            include_str!("../../testdata/supergraph.graphql"),
            &Default::default(),
        )
        .unwrap();
        let config = RouterLimitsConfig {
            max_root_fields: Some(1),
            ..Default::default()
        };

        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

        let service = ServiceBuilder::new()
            .layer(EnforceOperationLimitsLayer::new(&config))
            .service(mock);

        let query = "{ me { id } topProducts { name } }";
        let mut response = service.oneshot(make_request(&schema, query)).await.unwrap();
        let body = response.next_response().await.unwrap();
        assert_eq!(error_codes(&body), &["MAX_ROOT_FIELDS_LIMIT"]);

        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_max_aliases() {
        let schema = Schema::parse(
            include_str!("../../testdata/supergraph.graphql"),
            &Default::default(),
        )
        .unwrap();
        let config = RouterLimitsConfig {
            max_aliases: Some(2),
            ..Default::default()
        };

        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

        let service = ServiceBuilder::new()
            .layer(EnforceOperationLimitsLayer::new(&config))
            .service(mock);

        let query =
            "{ topProducts { productName: name productReviews: reviews { reviewBody: body } } }";
        let mut response = service.oneshot(make_request(&schema, query)).await.unwrap();
        let body = response.next_response().await.unwrap();
        assert_eq!(error_codes(&body), &["MAX_ALIASES_LIMIT"]);

        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_max_depth() {
        let schema = Schema::parse(
            include_str!("../../testdata/supergraph.graphql"),
            &Default::default(),
        )
        .unwrap();
        let config = RouterLimitsConfig {
            max_depth: Some(3),
            ..Default::default()
        };

        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

        let service = ServiceBuilder::new()
            .layer(EnforceOperationLimitsLayer::new(&config))
            .service(mock);

        let query = "{ topProducts { reviews { author { name } } } }";
        let mut response = service.oneshot(make_request(&schema, query)).await.unwrap();
        let body = response.next_response().await.unwrap();
        assert_eq!(error_codes(&body), &["MAX_DEPTH_LIMIT"]);

        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_multiple_violations() {
        let schema = Schema::parse(
            include_str!("../../testdata/supergraph.graphql"),
            &Default::default(),
        )
        .unwrap();
        let config = RouterLimitsConfig {
            max_root_fields: Some(1),
            max_aliases: Some(2),
            max_depth: Some(3),
            max_height: Some(4),
            ..Default::default()
        };

        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

        let service = ServiceBuilder::new()
            .layer(EnforceOperationLimitsLayer::new(&config))
            .service(mock);

        let query = "{
            topProducts {
                productName: name
                productReviews: reviews {
                    reviewAuthor: author {
                        name
                    }
                }
            }
        }";
        let mut response = service.oneshot(make_request(&schema, query)).await.unwrap();
        let body = response.next_response().await.unwrap();
        let mut codes = error_codes(&body);
        codes.sort();
        assert_eq!(
            codes,
            ["MAX_ALIASES_LIMIT", "MAX_DEPTH_LIMIT", "MAX_HEIGHT_LIMIT"]
        );

        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_warn_only() {
        let _guard = tracing_test::dispatcher_guard();

        let schema = Schema::parse(
            include_str!("../../testdata/supergraph.graphql"),
            &Default::default(),
        )
        .unwrap();
        let config = RouterLimitsConfig {
            max_root_fields: Some(1),
            max_depth: Some(2),
            warn_only: true,
            ..Default::default()
        };

        let (mock, mut handle) =
            tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_response(supergraph::Response::fake_builder().build().unwrap());
        });

        let service = ServiceBuilder::new()
            .layer(EnforceOperationLimitsLayer::new(&config))
            .service(mock);

        let query = "{ me { id } topProducts { reviews { body } } }";
        let mut response = service.oneshot(make_request(&schema, query)).await.unwrap();
        let body = response.next_response().await.unwrap();
        assert!(body.errors.is_empty());
        assert!(
            tracing_test::logs_contain("request exceeded complexity limits"),
            "expected a warning to be logged"
        );

        crate::plugin::test::await_mock_driver(driver).await;
    }
}
