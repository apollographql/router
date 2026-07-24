//! Tower layers wrapping old non-tower layer-like things into real tower layers.
//!
//! Long-term, we should move the actual implementation code into a service structure, but that is
//! more work especially to translate the tests.

use std::sync::Arc;

use futures::future::BoxFuture;
use http::StatusCode;
use tower::BoxError;
use tower::Service;

use crate::apollo_studio_interop::UsageReporting;
use crate::compute_job::MaybeBackPressureError;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::error::Error as RouterError;
use crate::graphql::ErrorExtension;
use crate::graphql::IntoGraphQLErrors;
use crate::query_planner::OperationKind;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::query_parsing;
use crate::services::supergraph;
use crate::spec::SpecError;

/// Parses the GraphQL in the supergraph request.
///
/// # Context
/// This stores values in the request context:
/// - [`ParsedDocument`]
/// - "operation_name" and "operation_kind"
/// - [`Arc`]`<`[`UsageReporting`]`>` if there was an error; normally, this would be populated
///   by the caching query planner, but we do not reach that code if we fail early here.
pub(crate) struct ParseQueryLayer {
    query_parsing_service: query_parsing::BoxCloneService,
    redact_query_validation_errors: bool,
}

impl ParseQueryLayer {
    pub(crate) fn new(
        query_parsing_service: query_parsing::BoxCloneService,
        redact_query_validation_errors: bool,
    ) -> Self {
        Self {
            query_parsing_service,
            redact_query_validation_errors,
        }
    }
}

impl<S> tower::Layer<S> for ParseQueryLayer {
    type Service = ParseQueryService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ParseQueryService {
            inner,
            query_parsing_service: self.query_parsing_service.clone(),
            redact_query_validation_errors: self.redact_query_validation_errors,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ParseQueryService<S> {
    inner: S,
    query_parsing_service: query_parsing::BoxCloneService,
    redact_query_validation_errors: bool,
}

impl<S> Service<supergraph::Request> for ParseQueryService<S>
where
    S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = supergraph::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::ready!(self.query_parsing_service.poll_ready(cx)).map_err(|err| match err {
            MaybeBackPressureError::PermanentError(err) => Box::new(err) as BoxError,
            // Technically a temporary error is supposed to be a backpressure error,
            // but we should not get an error here if it truly is "just" backpressure.
            MaybeBackPressureError::TemporaryError(err) => Box::new(err) as BoxError,
        })?;
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: supergraph::Request) -> Self::Future {
        let query_parsing_service = self.query_parsing_service.clone();
        let mut query_parsing_service =
            std::mem::replace(&mut self.query_parsing_service, query_parsing_service);

        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        let redact_query_validation_errors = self.redact_query_validation_errors;

        Box::pin(async move {
            let query = req.supergraph_request.body().query.as_ref();
            if query.is_none() || query.unwrap().trim().is_empty() {
                let errors = vec![
                    RouterError::builder()
                        .message("Must provide query string.".to_string())
                        .extension_code("MISSING_QUERY_STRING")
                        .build(),
                ];
                return Ok(supergraph::Response::builder()
                    .errors(errors)
                    .status_code(StatusCode::BAD_REQUEST)
                    .context(req.context)
                    .build()
                    .expect("response is valid"));
            }

            let operation_name = req.supergraph_request.body().operation_name.clone();
            let query = req
                .supergraph_request
                .body()
                .query
                .clone()
                .expect("query presence was already checked");

            match query_parsing_service
                .call(query_parsing::Request::new(query, operation_name.clone()))
                .await
            {
                Ok(doc) => {
                    req.context
                        .insert(OPERATION_NAME, doc.operation.name.clone())
                        .expect("cannot insert operation name into context; this is a bug");
                    let operation_kind = OperationKind::from(doc.operation.operation_type);
                    req.context
                        .insert(OPERATION_KIND, operation_kind)
                        .expect("cannot insert operation kind in the context; this is a bug");

                    req.context
                        .extensions()
                        .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

                    inner.call(req).await
                }
                Err(MaybeBackPressureError::PermanentError(errors)) => {
                    let errors = if redact_query_validation_errors
                        && matches!(errors, SpecError::ValidationError(_))
                    {
                        SpecError::Redacted
                    } else {
                        errors
                    };

                    req.context.extensions().with_lock(|lock| {
                        lock.insert(Arc::new(UsageReporting::Error(
                            errors.get_error_key().to_string(),
                        )))
                    });
                    let errors = match errors.into_graphql_errors() {
                        Ok(v) => v,
                        Err(errors) => vec![
                            crate::graphql::Error::builder()
                                .message(errors.to_string())
                                .extension_code(errors.extension_code())
                                .build(),
                        ],
                    };
                    Ok(supergraph::Response::builder()
                        .errors(errors)
                        .status_code(StatusCode::BAD_REQUEST)
                        .context(req.context)
                        .build()
                        .expect("response is valid"))
                }
                Err(MaybeBackPressureError::TemporaryError(error)) => {
                    req.context.extensions().with_lock(|lock| {
                        let error_key =
                            SpecError::ValidationError(crate::error::ValidationErrors {
                                errors: vec![],
                            })
                            .get_error_key();
                        lock.insert(Arc::new(UsageReporting::Error(error_key.to_string())))
                    });
                    Ok(supergraph::Response::builder()
                        .error(error.to_graphql_error())
                        .status_code(StatusCode::SERVICE_UNAVAILABLE)
                        .context(req.context)
                        .build()
                        .expect("response is valid"))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::*;
    use crate::Configuration;
    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;

    const SCHEMA: &str = include_str!("../../../testing_schema.graphql");

    /// Wrap a tower-test mock so it can be used as a query parsing service.
    ///
    /// In this case a tower-test error indicates a compute job backpressure error, and an Err
    /// response indicates an error inside the inner service.
    #[derive(Clone)]
    struct MockQueryParsing(
        tower_test::mock::Mock<query_parsing::Request, Result<ParsedDocument, SpecError>>,
    );

    impl Service<query_parsing::Request> for MockQueryParsing {
        type Response = ParsedDocument;
        type Error = query_parsing::ServiceError;
        type Future = BoxFuture<'static, Result<ParsedDocument, query_parsing::ServiceError>>;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx).map_err(|_| {
                MaybeBackPressureError::TemporaryError(crate::compute_job::ComputeBackPressureError)
            })
        }

        fn call(&mut self, req: query_parsing::Request) -> Self::Future {
            let inner = self.0.clone();
            let mut inner = std::mem::replace(&mut self.0, inner);
            Box::pin(async move {
                match inner.call(req).await {
                    Ok(Ok(doc)) => Ok(doc),
                    Ok(Err(spec_error)) => Err(MaybeBackPressureError::PermanentError(spec_error)),
                    Err(_boxed) => Err(MaybeBackPressureError::TemporaryError(
                        crate::compute_job::ComputeBackPressureError,
                    )),
                }
            })
        }
    }

    fn mock_query_parsing() -> (
        query_parsing::BoxCloneService,
        tower_test::mock::Handle<query_parsing::Request, Result<ParsedDocument, SpecError>>,
    ) {
        let (mock, handle) = tower_test::mock::pair();
        (
            tower::util::BoxCloneService::new(MockQueryParsing(mock)),
            handle,
        )
    }

    #[tokio::test]
    async fn it_accepts_valid_query() {
        let (query_parsing_service, mut query_parsing_handle) = mock_query_parsing();
        let config = Configuration::default();
        let schema = crate::spec::Schema::parse(SCHEMA, &config).unwrap();
        let query_parsing_driver = tokio::spawn(async move {
            let (req, responder) = query_parsing_handle.next_request().await.unwrap();
            responder.send_response(crate::spec::Query::parse_document(
                &req.query, None, &schema, &config,
            ));
        });

        let (mock, mut handle) =
            tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
        let inner_driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();

            // The document, operation name and operation kind should already be in context by
            // the time the inner service is called.
            assert!(
                req.context
                    .extensions()
                    .with_lock(|lock| lock.contains_key::<ParsedDocument>())
            );
            assert!(
                req.context
                    .get::<_, Option<String>>(OPERATION_NAME)
                    .unwrap()
                    .is_some()
            );
            assert!(
                req.context
                    .get::<_, OperationKind>(OPERATION_KIND)
                    .unwrap()
                    .is_some()
            );

            responder.send_response(supergraph::Response::fake_builder().build().unwrap());
        });

        let mut service = ServiceBuilder::new()
            .layer(ParseQueryLayer::new(query_parsing_service, false))
            .service(mock);

        let response = service
            .ready()
            .await
            .unwrap()
            .call(
                supergraph::Request::fake_builder()
                    .query("query { me { id } }")
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(http::StatusCode::OK, response.response.status());

        crate::plugin::test::await_mock_driver(query_parsing_driver).await;
        crate::plugin::test::await_mock_driver(inner_driver).await;
    }

    #[tokio::test]
    async fn it_rejects_missing_query() {
        let (query_parsing_service, query_parsing_handle) = mock_query_parsing();
        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

        let mut service = ServiceBuilder::new()
            .layer(ParseQueryLayer::new(query_parsing_service, false))
            .service(mock);

        let mut response = service
            .ready()
            .await
            .unwrap()
            .call(supergraph::Request::fake_builder().build().unwrap())
            .await
            .unwrap();

        assert_eq!(StatusCode::BAD_REQUEST, response.response.status());
        let graphql_response = response.next_response().await.unwrap();
        assert!(graphql_response.contains_error_code("MISSING_QUERY_STRING"));

        // Neither service is actually reached.
        crate::plugin::test::assert_no_mock_calls(query_parsing_handle).await;
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn it_rejects_invalid_query() {
        let (query_parsing_service, mut query_parsing_handle) = mock_query_parsing();
        let config = Configuration::default();
        let schema = crate::spec::Schema::parse(SCHEMA, &config).unwrap();
        let query_parsing_driver = tokio::spawn(async move {
            let (req, responder) = query_parsing_handle.next_request().await.unwrap();
            responder.send_response(crate::spec::Query::parse_document(
                &req.query, None, &schema, &config,
            ));
        });

        let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

        let mut service = ServiceBuilder::new()
            .layer(ParseQueryLayer::new(query_parsing_service, false))
            .service(mock);

        let response = service
            .ready()
            .await
            .unwrap()
            .call(
                supergraph::Request::fake_builder()
                    .query("query Missing { doesNotExist }")
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(StatusCode::BAD_REQUEST, response.response.status());

        crate::plugin::test::await_mock_driver(query_parsing_driver).await;
        // The inner service is not reached.
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }
}
