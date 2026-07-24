//! Tower layers wrapping old non-tower layer-like things into real tower layers.
//!
//! Long-term, we should move the actual implementation code into a service structure, but that is
//! more work especially to translate the tests.

use std::sync::Arc;

use futures::future::BoxFuture;
use http::StatusCode;
use tower::BoxError;
use tower::Service;

use crate::Context;
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
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: supergraph::Request) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let mut query_parsing_service = self.query_parsing_service.clone();
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
                .call(query_parsing::Request {
                    query,
                    operation_name: operation_name.clone(),
                })
                .await
            {
                Ok(doc) => {
                    let context = Context::new();

                    context
                        .insert(OPERATION_NAME, doc.operation.name.clone())
                        .expect("cannot insert operation name into context; this is a bug");
                    let operation_kind = OperationKind::from(doc.operation.operation_type);
                    context
                        .insert(OPERATION_KIND, operation_kind)
                        .expect("cannot insert operation kind in the context; this is a bug");

                    req.context.extend(&context);
                    req.context
                        .extensions()
                        .with_lock(|lock| lock.insert::<ParsedDocument>(doc));

                    inner
                        .call(supergraph::Request {
                            supergraph_request: req.supergraph_request,
                            context: req.context,
                        })
                        .await
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
