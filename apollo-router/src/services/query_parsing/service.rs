use std::sync::Arc;

use futures::future::BoxFuture;
use tracing::Instrument as _;

use crate::Configuration;
use crate::compute_job;
use crate::compute_job::MaybeBackPressureError;
use crate::plugins::telemetry::consts::QUERY_PARSING_SPAN_NAME;
use crate::services::query_parsing::ParsedDocument;
use crate::services::query_parsing::Request;
use crate::services::query_parsing::ServiceError;
use crate::spec::Query;
use crate::spec::Schema;

/// Parses and validates GraphQL queries on the compute job pool.
#[derive(Clone)]
pub(crate) struct QueryParsingService {
    schema: Arc<Schema>,
    configuration: Arc<Configuration>,
}

impl QueryParsingService {
    pub(crate) fn new(schema: Arc<Schema>, configuration: Arc<Configuration>) -> Self {
        Self {
            schema,
            configuration,
        }
    }
}

impl tower::Service<Request> for QueryParsingService {
    type Response = ParsedDocument;
    type Error = ServiceError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let schema = self.schema.clone();
        let conf = self.configuration.clone();

        Box::pin(async move {
            // Must be created *outside* of the compute_job or the span is not connected to the parent
            let span = tracing::info_span!(QUERY_PARSING_SPAN_NAME, "otel.kind" = "INTERNAL");
            let compute_job_future = span.in_scope(|| {
                compute_job::execute(req.compute_job_type, move |_| {
                    Query::parse_document(
                        &req.query,
                        req.operation_name.as_deref(),
                        schema.as_ref(),
                        conf.as_ref(),
                    )
                })
            });

            compute_job_future
                .map_err(MaybeBackPressureError::TemporaryError)?
                .instrument(span)
                .await
                .map_err(MaybeBackPressureError::PermanentError)
        })
    }
}
