use std::sync::Arc;

use futures::future::Either;
use http::StatusCode;

use crate::configuration::mode::Mode;
use crate::services::execution;
use crate::spec::Schema;

#[derive(Clone)]
pub(super) struct LegacyVariableValidationLayer {
    schema: Arc<Schema>,
    mode: Mode,
}

impl LegacyVariableValidationLayer {
    pub(super) fn new(schema: Arc<Schema>, mode: Mode) -> Self {
        Self { schema, mode }
    }
}

impl<S> tower::Layer<S> for LegacyVariableValidationLayer {
    type Service = LegacyVariableValidationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LegacyVariableValidationService::new(inner, self.schema.clone(), self.mode)
    }
}

#[derive(Clone)]
pub(super) struct LegacyVariableValidationService<S> {
    inner: S,
    schema: Arc<Schema>,
    mode: Mode,
}

impl<S> LegacyVariableValidationService<S> {
    pub(super) fn new(inner: S, schema: Arc<Schema>, mode: Mode) -> Self {
        Self {
            inner,
            schema,
            mode,
        }
    }
}

impl<S> tower::Service<execution::Request> for LegacyVariableValidationService<S>
where
    S: tower::Service<execution::Request, Response = execution::Response>,
    S::Future: Send + 'static,
{
    type Response = execution::Response;
    type Error = S::Error;
    type Future = Either<S::Future, std::future::Ready<Result<S::Response, S::Error>>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: execution::Request) -> Self::Future {
        let variables = &req.supergraph_request.body().variables;

        match req
            .query_plan
            .query
            .validate_variables(variables, &self.schema, self.mode)
        {
            Ok(_) => Either::Left(self.inner.call(req)),
            Err(errors) => {
                let response = execution::Response::error_builder()
                    .status_code(StatusCode::BAD_REQUEST)
                    .errors(errors)
                    .context(req.context)
                    .build()
                    .expect("does not fail");
                Either::Right(std::future::ready(Ok(response)))
            }
        }
    }
}
