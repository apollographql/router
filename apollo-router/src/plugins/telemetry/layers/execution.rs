use std::sync::Arc;

use ::tracing::info_span;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::layers::ServiceBuilderExt;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::apollo::OperationSubType;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::consts::EXECUTION_SPAN_NAME;
use crate::plugins::telemetry::error_counter::count_execution_errors;
use crate::query_planner::OperationKind;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::execution;

/// Layer type for [Telemetry::instrument_execution_layer].
#[derive(Clone)]
pub(crate) struct InstrumentExecutionLayer {
    config: Arc<config::Conf>,
}

impl InstrumentExecutionLayer {
    fn new(config: Arc<config::Conf>) -> Self {
        Self { config }
    }
}

impl<S> tower::Layer<S> for InstrumentExecutionLayer
where
    S: tower::Service<ExecutionRequest, Response = ExecutionResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = execution::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        let config = self.config.clone();

        ServiceBuilder::new()
            .instrument(move |req: &ExecutionRequest| {
                let operation_kind = req.query_plan.query.operation.kind();

                match operation_kind {
                    OperationKind::Subscription => info_span!(
                        EXECUTION_SPAN_NAME,
                        "otel.kind" = "INTERNAL",
                        "graphql.operation.type" = operation_kind.as_apollo_operation_type(),
                        "apollo_private.operation.subtype" =
                            OperationSubType::SubscriptionRequest.as_str(),
                    ),
                    _ => info_span!(
                        EXECUTION_SPAN_NAME,
                        "otel.kind" = "INTERNAL",
                        "graphql.operation.type" = operation_kind.as_apollo_operation_type(),
                    ),
                }
            })
            .and_then(move |resp: ExecutionResponse| {
                let config = config.clone();
                async move {
                    let resp = count_execution_errors(resp, &config.apollo.errors).await;
                    Ok::<_, BoxError>(resp)
                }
            })
            .service(inner)
            .boxed_clone()
    }
}

impl Telemetry {
    /// Returns a layer that instruments query plan execution with a span and error metrics.
    pub(crate) fn instrument_execution_layer(&self) -> InstrumentExecutionLayer {
        InstrumentExecutionLayer::new(self.config.clone())
    }
}
