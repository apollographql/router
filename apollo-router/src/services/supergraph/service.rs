//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use futures::TryFutureExt;
use futures::future::BoxFuture;
use futures::future::ready;
use futures::stream::StreamExt;
use futures::stream::once;
use http::StatusCode;
use indexmap::IndexMap;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use tower::BoxError;
use tower::ServiceExt;
use tower::load_shed::error::Overloaded;
use tower_service::Service;
use tracing_futures::Instrument;

use crate::Context;
use crate::compute_job::ComputeBackPressureError;
use crate::configuration::mode::Mode;
use crate::error::CacheResolverError;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::introspection;
use crate::introspection::IntrospectionService;
use crate::plugin::DynPlugin;
use crate::plugins::connectors::query_plans::store_connectors;
use crate::plugins::connectors::query_plans::store_connectors_labels;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::supergraph::events::SupergraphEventResponse;
use crate::plugins::telemetry::consts::QUERY_PLANNING_SPAN_NAME;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::QueryPlannerResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::execution;
use crate::services::query_parsing::ParsedDocument;
use crate::services::query_planner;
use crate::services::router::ClientRequestAccepts;
use crate::spec::Schema;

pub(crate) const FIRST_EVENT_CONTEXT_KEY: &str = "apollo::supergraph::first_event";

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecycle.
#[derive(Clone)]
pub(crate) struct SupergraphService {
    query_planner_service: query_planner::CacheBoxCloneService,
    execution_service: execution::BoxCloneService,
    introspection_service: IntrospectionService,
    schema: Arc<Schema>,
    strict_variable_validation: Mode,
}

#[buildstructor::buildstructor]
impl SupergraphService {
    #[builder]
    pub(crate) fn new(
        query_planner_service: query_planner::CacheBoxCloneService,
        execution_service: execution::BoxCloneService,
        introspection_service: IntrospectionService,
        schema: Arc<Schema>,
        strict_variable_validation: Mode,
    ) -> Self {
        SupergraphService {
            query_planner_service,
            execution_service,
            introspection_service,
            schema,
            strict_variable_validation,
        }
    }
}

impl Service<SupergraphRequest> for SupergraphService {
    type Response = SupergraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.query_planner_service.poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other.map_err(|err| err.into()),
        }
        self.execution_service.poll_ready(cx)
    }

    fn call(&mut self, req: SupergraphRequest) -> Self::Future {
        if let Some(connectors) = &self.schema.connectors {
            store_connectors_labels(&req.context, connectors.labels_by_service_name.clone());
            store_connectors(&req.context, connectors.by_service_name.clone());
        }

        // Consume our cloned services and allow ownership to be transferred to the async block.
        let query_planner_service = self.query_planner_service.clone();
        let query_planner_service =
            std::mem::replace(&mut self.query_planner_service, query_planner_service);

        let execution_service = self.execution_service.clone();
        let execution_service = std::mem::replace(&mut self.execution_service, execution_service);

        // We won't do a readiness dance here because we didn't ready the service: we don't know if
        // we'll need it.
        let introspection_service = self.introspection_service.clone();

        let schema = self.schema.clone();

        let context_cloned = req.context.clone();
        let fut = service_call(
            query_planner_service,
            execution_service,
            introspection_service,
            schema,
            req,
            self.strict_variable_validation,
        )
        .or_else(|error: BoxError| async move {
            let errors = vec![
                crate::error::Error::builder()
                    .message(error.to_string())
                    .extension_code("INTERNAL_SERVER_ERROR")
                    .build(),
            ];

            Ok(SupergraphResponse::infallible_builder()
                .errors(errors)
                .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                .context(context_cloned)
                .build())
        });

        Box::pin(fut)
    }
}

async fn service_call(
    planning: query_planner::CacheBoxCloneService,
    mut execution_service: execution::BoxCloneService,
    mut introspection_service: IntrospectionService,
    schema: Arc<Schema>,
    req: SupergraphRequest,
    strict_variable_validation: Mode,
) -> Result<SupergraphResponse, BoxError> {
    let context = req.context;
    let body = req.supergraph_request.body();
    let variables = body.variables.clone();

    if let Some(document) = context
        .extensions()
        .with_lock(|extensions| extensions.get::<ParsedDocument>().cloned())
        && introspection::is_introspection_query(&document)
    {
        // Introspection queries are currently short-circuited: we don't support query planning
        // them, and we don't support mixed introspection/non-introspection queries.
        // It's unfortunate that we are _executing_ these queries here rather than in the execution
        // service, but it's basically the only way we can do it right now.
        let result = introspection_service
            // This has a load shed layer on it, so it will definitely be ready.
            .ready()
            .await?
            .call(introspection::IntrospectionRequest {
                schema,
                document,
                variables,
            })
            .await;

        return match result {
            Ok(response) => Ok(SupergraphResponse::new_from_graphql_response(
                response, context,
            )),
            Err(error) => {
                // There are two types of backpressure errors currently: one from tower and one from
                // the compute job pool. Handle both of them the same way.
                let backpressure = error
                    .downcast_ref::<Overloaded>()
                    .map(|_| &ComputeBackPressureError)
                    .or_else(|| error.downcast_ref::<ComputeBackPressureError>());

                if let Some(backpressure) = backpressure {
                    Ok(SupergraphResponse::error_builder()
                        .status_code(StatusCode::SERVICE_UNAVAILABLE)
                        .context(context)
                        .error(backpressure.to_graphql_error())
                        .build()
                        .unwrap())
                } else {
                    Err(error)
                }
            }
        };
    }

    let QueryPlannerResponse { content, errors } = match plan_query(
        planning,
        body.operation_name.clone(),
        context.clone(),
        // We cannot assume that the query is present as it may have been modified by coprocessors or plugins.
        // There is a deeper issue here in that query analysis is doing a bunch of stuff that it should not and
        // places the results in context. Therefore plugins that have modified the query won't actually take effect.
        // However, this can't be resolved before looking at the pipeline again.
        req.supergraph_request
            .body()
            .query
            .clone()
            .unwrap_or_default(),
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => {
            let status = match &err {
                CacheResolverError::Backpressure(_) => StatusCode::SERVICE_UNAVAILABLE,
                CacheResolverError::RetrievalError(_) => StatusCode::BAD_REQUEST,
            };
            match err.into_graphql_errors() {
                Ok(gql_errors) => {
                    return Ok(SupergraphResponse::infallible_builder()
                        .context(context)
                        .errors(gql_errors)
                        .status_code(status) // If it's a graphql error we return a status code 400
                        .build());
                }
                Err(err) => return Err(err.into()),
            }
        }
    };

    if !errors.is_empty() {
        return Ok(SupergraphResponse::infallible_builder()
            .context(context)
            .errors(errors)
            .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
            .build());
    }

    match content {
        Some(plan) => {
            let is_deferred = plan.is_deferred(&variables);
            let is_subscription = plan.is_subscription();

            let ClientRequestAccepts {
                multipart_defer: accepts_multipart_defer,
                multipart_subscription: accepts_multipart_subscription,
                ..
            } = context
                .extensions()
                .with_lock(|lock| lock.get().cloned())
                .unwrap_or_default();
            if (is_deferred && !accepts_multipart_defer)
                || (is_subscription && !accepts_multipart_subscription)
            {
                let (error_message, error_code) = if is_deferred {
                    (
                        String::from(
                            "the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed;deferSpec=20220824'",
                        ),
                        "DEFER_BAD_HEADER",
                    )
                } else {
                    (
                        String::from(
                            "the router received a query with a subscription but the client does not accept multipart/mixed HTTP responses. To enable subscription support, add the HTTP header 'Accept: multipart/mixed;subscriptionSpec=1.0'",
                        ),
                        "SUBSCRIPTION_BAD_HEADER",
                    )
                };
                let mut response = SupergraphResponse::new_from_graphql_response(
                    graphql::Response::builder()
                        .errors(vec![
                            crate::error::Error::builder()
                                .message(error_message)
                                .extension_code(error_code)
                                .build(),
                        ])
                        .build(),
                    context,
                );
                *response.response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                Ok(response)
            } else if let Some(err) = plan
                .query
                .validate_variables(body, &schema, strict_variable_validation)
                .err()
            {
                let mut res = SupergraphResponse::new_from_graphql_response(err, context);
                *res.response.status_mut() = StatusCode::BAD_REQUEST;
                Ok(res)
            } else {
                let execution_response = execution_service
                    .call(
                        ExecutionRequest::internal_builder()
                            .supergraph_request(req.supergraph_request)
                            .query_plan(plan.clone())
                            .context(context)
                            .build()
                            .await,
                    )
                    .await?;

                let ExecutionResponse { response, context } = execution_response;

                let (parts, response_stream) = response.into_parts();

                let supergraph_response_event = context
                    .extensions()
                    .with_lock(|lock| lock.get::<SupergraphEventResponse>().cloned());
                let mut first_event = true;
                let mut inserted = false;
                let ctx = context.clone();
                let response_stream = response_stream.inspect(move |_| {
                    if first_event {
                        // Populate FIRST_EVENT_CONTEXT_KEY so downstream telemetry selectors
                        // (SupergraphSelector::IsPrimaryResponse) can distinguish the primary
                        // response chunk from deferred/subscription chunks.
                        ctx.insert_json_value(
                            FIRST_EVENT_CONTEXT_KEY,
                            serde_json_bytes::Value::Bool(true),
                        );
                        first_event = false;
                    } else if !inserted {
                        ctx.insert_json_value(
                            FIRST_EVENT_CONTEXT_KEY,
                            serde_json_bytes::Value::Bool(false),
                        );
                        inserted = true;
                    }
                });

                // make sure to resolve the first part of the stream - that way we know context
                // variables (`FIRST_EVENT_CONTEXT_KEY`, `CONTAINS_GRAPHQL_ERROR`) have been set
                let (first, remaining) = StreamExt::into_future(response_stream).await;
                let response_stream = once(ready(first.unwrap_or_default()))
                    .chain(remaining)
                    .boxed();

                match supergraph_response_event {
                    Some(supergraph_response_event) => {
                        let mut attrs = Vec::with_capacity(4);
                        let header_string = crate::services::header_masking::masked_headers_for_log(
                            &context,
                            crate::services::header_masking::Direction::Response,
                            None,
                            &parts.headers,
                        );
                        attrs.push(KeyValue::new(
                            Key::from_static_str("http.response.headers"),
                            opentelemetry::Value::String(header_string.into()),
                        ));
                        attrs.push(KeyValue::new(
                            Key::from_static_str("http.response.status"),
                            opentelemetry::Value::String(format!("{}", parts.status).into()),
                        ));
                        attrs.push(KeyValue::new(
                            Key::from_static_str("http.response.version"),
                            opentelemetry::Value::String(format!("{:?}", parts.version).into()),
                        ));
                        let ctx = context.clone();
                        let response_stream = Box::pin(response_stream.inspect(move |resp| {
                            if !supergraph_response_event
                                .condition
                                .evaluate_event_response(resp, &ctx)
                            {
                                return;
                            }
                            attrs.push(KeyValue::new(
                                Key::from_static_str("http.response.body"),
                                opentelemetry::Value::String(
                                    serde_json::to_string(resp).unwrap_or_default().into(),
                                ),
                            ));
                            log_event(
                                supergraph_response_event.level,
                                "supergraph.response",
                                attrs.clone(),
                                "",
                            );
                        }));

                        Ok(SupergraphResponse {
                            context,
                            response: http::Response::from_parts(parts, response_stream.boxed()),
                        })
                    }
                    None => Ok(SupergraphResponse {
                        context,
                        response: http::Response::from_parts(parts, response_stream.boxed()),
                    }),
                }
            }
        }
        // This should never happen because if we have an empty query plan we should have error in errors vec
        None => Err(BoxError::from("cannot compute a query plan")),
    }
}

async fn plan_query(
    mut planning: query_planner::CacheBoxCloneService,
    operation_name: Option<String>,
    context: Context,
    query_str: String,
) -> Result<QueryPlannerResponse, CacheResolverError> {
    let qpr = planning
        .call(
            query_planner::CachingRequest::builder()
                .query(query_str)
                .and_operation_name(operation_name)
                .context(context.clone())
                .build(),
        )
        .instrument(tracing::info_span!(
            QUERY_PLANNING_SPAN_NAME,
            "otel.kind" = "INTERNAL"
        ))
        .await?;

    Ok(qpr)
}
