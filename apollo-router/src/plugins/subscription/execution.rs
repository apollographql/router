//! Tests for this functionality are still mostly in the `crate::services::supergraph::tests` module.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::Ordering;
use std::time::Instant;

use futures::FutureExt;
use futures::Stream;
use futures::future::BoxFuture;
use futures::stream::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendError;
use tokio_stream::wrappers::ReceiverStream;
use tower::Layer;
use tower::Service;
use tower::ServiceExt as _;
use tracing::Span;
use tracing::field;
use tracing_futures::Instrument;

use crate::Configuration;
use crate::Context;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::OPERATION_NAME;
use crate::graphql;
use crate::graphql::Response;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::subscription::Notify;
use crate::plugins::subscription::SUBSCRIPTION_ERROR_EXTENSION_KEY;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use crate::query_planner::subscription::OPENED_SUBSCRIPTIONS;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;
use crate::query_planner::subscription::SubscriptionHandle;
use crate::services::ExecutionRequest;
use crate::services::execution;
use crate::services::execution::QueryPlan;
use crate::services::subgraph::BoxGqlStream;
use crate::spec::Schema;

pub(crate) const SUBSCRIPTION_CONFIG_RELOAD_EXTENSION_CODE: &str = "SUBSCRIPTION_CONFIG_RELOAD";
pub(crate) const SUBSCRIPTION_SCHEMA_RELOAD_EXTENSION_CODE: &str = "SUBSCRIPTION_SCHEMA_RELOAD";
const SUBSCRIPTION_JWT_EXPIRED_EXTENSION_CODE: &str = "SUBSCRIPTION_JWT_EXPIRED";
const SUBSCRIPTION_EXECUTION_ERROR_EXTENSION_CODE: &str = "SUBSCRIPTION_EXECUTION_ERROR";
pub(crate) const SUBSCRIPTION_MAX_LIFETIME_EXTENSION_CODE: &str =
    "SUBSCRIPTION_MAX_LIFETIME_EXCEEDED";

/// The execution side of the subscriptions implementation starts up a side-channel task used to
/// handle messages received from the subgraph that we subscribed to.
pub(crate) struct SubscriptionExecutionLayer {
    notify: Notify<String, graphql::Response>,
}

impl SubscriptionExecutionLayer {
    pub(crate) fn new(notify: Notify<String, graphql::Response>) -> Self {
        Self { notify }
    }
}

impl<S> Layer<S> for SubscriptionExecutionLayer {
    type Service = SubscriptionExecutionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SubscriptionExecutionService {
            inner,
            notify: self.notify.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SubscriptionExecutionService<S> {
    inner: S,
    notify: Notify<String, graphql::Response>,
}

impl<S> Service<ExecutionRequest> for SubscriptionExecutionService<S>
where
    S: Service<ExecutionRequest, Response = execution::Response, Error = tower::BoxError>
        + Clone
        + Send
        + 'static,
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

    fn call(&mut self, mut req: ExecutionRequest) -> Self::Future {
        if req.query_plan.is_subscription() {
            let context = req.context.clone();
            let (subs_tx, subs_rx) = mpsc::channel(1);
            let query_plan = req.query_plan.clone();
            let execution_service_cloned = self.inner.clone();
            let supergraph_http_request = req.supergraph_request.clone();

            // Subscribe to config and schema changes immediately, so we don't
            // miss out on any events happening in-between here and when we
            // actually start responding to changes (which happens asynchronously
            // inside the subscription task).
            let configuration_updated_rx = Box::pin(self.notify.subscribe_configuration());
            let schema_updated_rx = Box::pin(self.notify.subscribe_schema());

            tokio::spawn(async move {
                subscription_task(
                    execution_service_cloned,
                    context,
                    query_plan,
                    subs_rx,
                    supergraph_http_request,
                    configuration_updated_rx,
                    schema_updated_rx,
                )
                .await;
            });
            req.subscription_tx = subs_tx.into();
        }

        Box::pin(self.inner.call(req))
    }
}

// WARN: you might be tempted to change this to pub(crate) because its fields are pub(crate) and
// there's no actual use in having it marked pub _apart_ from it being used in the
// execution::Request builder and, more importantly, the fake_builder we get from BuildStructor.
// Customers use that in unit tests for their plugins, and they'll fail to compile if we mark this
// pub(crate)
pub struct SubscriptionTaskParams {
    pub(crate) client_sender: tokio::sync::mpsc::Sender<Response>,
    pub(crate) subscription_handle: SubscriptionHandle,
    pub(crate) subscription_config: SubscriptionConfig,
    pub(crate) stream_rx: ReceiverStream<BoxGqlStream>,
}

fn subscription_fatal_error(message: impl Into<String>, extension_code: &str) -> Response {
    Response::builder()
        .subscribed(false)
        .extension(SUBSCRIPTION_ERROR_EXTENSION_KEY, true)
        .error(
            graphql::Error::builder()
                .message(message)
                .extension_code(extension_code)
                .build(),
        )
        .build()
}

/// The execution-side end of the subscriptions side-channel, which propagates messages from the
/// subgraph to the client.
///
/// This end receives the messages from the subgraph, executes query plans to resolve federated
/// data in those messages, and sends the response back on a channel that is part of the eventual
/// response.
async fn subscription_task(
    execution_service: impl Service<
        ExecutionRequest,
        Response = execution::Response,
        Error = tower::BoxError,
    > + Clone,
    context: Context,
    query_plan: Arc<QueryPlan>,
    mut rx: mpsc::Receiver<SubscriptionTaskParams>,
    supergraph_http_request: http::Request<graphql::Request>,
    mut configuration_updated_rx: Pin<Box<dyn Stream<Item = Weak<Configuration>> + Send>>,
    mut schema_updated_rx: Pin<Box<dyn Stream<Item = Arc<Schema>> + Send>>,
) {
    let sub_params = match rx.recv().await {
        Some(sub_params) => sub_params,
        None => {
            return;
        }
    };
    let subscription_config = sub_params.subscription_config;
    let subscription_handle = sub_params.subscription_handle;
    let mut receiver = sub_params.stream_rx;
    let sender = sub_params.client_sender;

    // Get the rest of the query_plan to execute for subscription events. If `rest` is `None`, we
    // bubble that up to the whole query plan instead of having an empty node: this lets us avoid
    // doing excessive calls into the execution service when no fetches are required.
    let query_plan = match query_plan.root.as_deref() {
        Some(crate::query_planner::PlanNode::Subscription { rest, .. }) => rest.clone().map(|r| {
            Arc::new(QueryPlan {
                usage_reporting: query_plan.usage_reporting.clone(),
                root: Some(Arc::new(*r)),
                formatted_query_plan: query_plan.formatted_query_plan.clone(),
                query: query_plan.query.clone(),
                estimated_size: Default::default(),
                batch_operations: Default::default(),
            })
        }),
        _ => {
            let _ = sender
                .send(subscription_fatal_error(
                    "cannot execute the subscription event",
                    SUBSCRIPTION_EXECUTION_ERROR_EXTENSION_CODE,
                ))
                .await;
            return;
        }
    };

    let limit_is_set = subscription_config.max_opened_subscriptions.is_some();
    let mut subscription_handle = subscription_handle.clone();
    let operation_signature = context
        .extensions()
        .with_lock(|lock| {
            lock.get::<Arc<UsageReporting>>()
                .map(|usage_reporting| usage_reporting.get_stats_report_key().clone())
        })
        .unwrap_or_default();

    let operation_name = context
        .get::<_, String>(OPERATION_NAME)
        .ok()
        .flatten()
        .unwrap_or_default();

    let mut receiver = match receiver.next().await {
        Some(receiver) => receiver,
        None => {
            tracing::trace!("receiver channel closed");
            return;
        }
    };

    if limit_is_set {
        OPENED_SUBSCRIPTIONS.fetch_add(1, Ordering::Relaxed);
    }

    // Note: `configuration_updated_rx` and `schema_updated_rx` are now
    // subscribed in `SubscriptionExecutionService::call` (before the spawn)
    // and passed in as parameters. See the comment in `call` for the race
    // this fix addresses.

    let mut timeout = if context
        .get_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS)
        .is_some()
    {
        let expires_in = crate::plugins::authentication::jwks::jwt_expires_in(&context);
        tokio::time::sleep(expires_in).boxed()
    } else {
        futures::future::pending().boxed()
    };

    let mut max_lifetime_timeout = if let Some(max_lifetime) = subscription_config.max_lifetime {
        tokio::time::sleep(*max_lifetime).boxed()
    } else {
        futures::future::pending().boxed()
    };

    loop {
        tokio::select! {
            // Every branch that ends the subscription is checked before the event branch. When the
            // subgraph always has another event ready, the event branch would otherwise win every
            // pass and the subscription would never see a close, an expiry or a reload. Those
            // branches end the loop when they fire, so they cannot starve events.
            biased;
            _ = subscription_handle.closed_signal.recv() => {
                break;
            }
            _ = &mut timeout => {
                let _ = sender.send(subscription_fatal_error("subscription closed because the JWT has expired", SUBSCRIPTION_JWT_EXPIRED_EXTENSION_CODE)).await;
                break;
            },
            _ = &mut max_lifetime_timeout => {
                let _ = sender.send(subscription_fatal_error("subscription closed because the maximum lifetime has been reached", SUBSCRIPTION_MAX_LIFETIME_EXTENSION_CODE)).await;
                break;
            },
            Some(_new_schema) = schema_updated_rx.next() => {
                let _ = sender
                    .send(subscription_fatal_error("subscription has been closed due to a schema reload", SUBSCRIPTION_SCHEMA_RELOAD_EXTENSION_CODE))
                    .await;

                break;
            }
            Some(_new_configuration) = configuration_updated_rx.next() => {
                let _ = sender
                    .send(subscription_fatal_error("subscription has been closed due to a configuration reload", SUBSCRIPTION_CONFIG_RELOAD_EXTENSION_CODE))
                    .await;

                break;
            }
            message = receiver.next() => {
                match message {
                    Some(mut val) => {
                        val.created_at = Some(Instant::now());
                        let res = dispatch_subscription_event(
                            &supergraph_http_request,
                            execution_service.clone(),
                            query_plan.as_ref(),
                            context.clone(),
                            val,
                            sender.clone(),
                        )
                            .instrument(tracing::info_span!(SUBSCRIPTION_EVENT_SPAN_NAME,
                                graphql.operation.name = %operation_name,
                                otel.kind = "INTERNAL",
                                apollo_private.operation_signature = %operation_signature,
                                apollo_private.duration_ns = field::Empty,)
                            )
                            .await;
                        if let Err(err) = res {
                            tracing::error!("cannot send the subscription to the client: {err:?}");
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }
    drop(sender);
    tracing::trace!("Leaving the task for subscription");
    if limit_is_set {
        OPENED_SUBSCRIPTIONS.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn dispatch_subscription_event(
    supergraph_http_request: &http::Request<graphql::Request>,
    execution_service: impl Service<
        ExecutionRequest,
        Response = execution::Response,
        Error = tower::BoxError,
    > + Clone,
    query_plan: Option<&Arc<QueryPlan>>,
    context: Context,
    mut val: graphql::Response,
    sender: mpsc::Sender<Response>,
) -> Result<(), SendError<Box<Response>>> {
    let start = Instant::now();
    let span = Span::current();
    let res = match query_plan {
        Some(query_plan) => {
            let cloned_supergraph_req = supergraph_http_request.clone();
            let execution_request = ExecutionRequest::internal_builder()
                .supergraph_request(cloned_supergraph_req)
                .query_plan(query_plan.clone())
                .context(context)
                .source_stream_value(val.data.take().unwrap_or_default())
                .build()
                .await;

            let execution_response = execution_service.oneshot(execution_request).await;
            let next_response = match execution_response {
                Ok(mut execution_response) => execution_response.next_response().await,
                Err(err) => {
                    tracing::error!("cannot execute the subscription event: {err:?}");
                    let _ = sender
                        .send(subscription_fatal_error(
                            "cannot execute the subscription event",
                            SUBSCRIPTION_EXECUTION_ERROR_EXTENSION_CODE,
                        ))
                        .await;
                    return Ok(());
                }
            };

            if let Some(mut next_response) = next_response {
                next_response.created_at = val.created_at;
                next_response.subscribed = val.subscribed;
                val.errors.append(&mut next_response.errors);
                next_response.errors = val.errors;

                sender
                    .send(next_response)
                    .await
                    .map_err(|SendError(unboxed)| SendError(Box::new(unboxed)))
            } else {
                Ok(())
            }
        }
        None => sender
            .send(val)
            .await
            .map_err(|SendError(unboxed)| SendError(Box::new(unboxed))),
    };
    span.record(
        APOLLO_PRIVATE_DURATION_NS,
        start.elapsed().as_nanos() as i64,
    );

    res
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use apollo_federation::query_plan::serializable_document::SerializableDocument;
    use serde_json_bytes::json;
    use tokio::sync::broadcast;
    use tokio::task::JoinHandle;
    use tower_test::mock;

    use super::*;
    use crate::query_planner::OperationKind;
    use crate::query_planner::PlanNode;
    use crate::query_planner::subscription::SubscriptionNode;

    /// How many events a test reads while waiting for a close before deciding the task never
    /// noticed the signal. With an event always ready, a starved task would otherwise run forever.
    const MAX_EVENTS_BEFORE_CLOSE: u64 = 100;

    type ExecutionHandle = mock::Handle<ExecutionRequest, execution::Response>;

    /// Drives `subscription_task` with a subgraph stream that always has another event ready.
    ///
    /// The client channel holds a single response, so each event's dispatch waits until the test
    /// reads the previous one, like a slow consumer.
    struct Harness {
        client_rx: mpsc::Receiver<Response>,
        configuration_tx: mpsc::Sender<Weak<Configuration>>,
        schema_tx: mpsc::Sender<Arc<Schema>>,
        _closed_tx: broadcast::Sender<()>,
        task: JoinHandle<()>,
    }

    impl Harness {
        /// A subscription whose plan has no fetches after it, so each event goes straight to the
        /// client, as with passthrough subscriptions.
        fn start(context: Context, subscription_config: SubscriptionConfig) -> Self {
            let execution_service = tower::service_fn(|_: ExecutionRequest| async {
                Err::<execution::Response, tower::BoxError>(
                    "a plan without fetches after the subscription does not execute".into(),
                )
            });
            Self::spawn(context, subscription_config, execution_service, None)
        }

        /// A subscription whose plan has fetches after it, so each event is executed by the
        /// returned mock execution service, which waits until the test answers each call. This is
        /// how a slow execution coprocessor holds up an event.
        fn start_with_execution() -> (Self, ExecutionHandle) {
            let (execution_service, execution) = mock::pair();
            // The mock never runs the plan, so any node will do.
            let rest = PlanNode::Sequence { nodes: Vec::new() };
            let harness = Self::spawn(
                Context::new(),
                SubscriptionConfig::default(),
                execution_service,
                Some(Box::new(rest)),
            );
            (harness, execution)
        }

        fn spawn<S>(
            context: Context,
            subscription_config: SubscriptionConfig,
            execution_service: S,
            rest: Option<Box<PlanNode>>,
        ) -> Self
        where
            S: Service<ExecutionRequest, Response = execution::Response, Error = tower::BoxError>
                + Clone
                + Send
                + 'static,
            S::Future: Send,
        {
            let query_plan = Arc::new(
                QueryPlan::fake_builder()
                    .root(PlanNode::Subscription {
                        primary: SubscriptionNode {
                            service_name: Arc::from("subgraph"),
                            variable_usages: Vec::new(),
                            operation: SerializableDocument::from_string(
                                "subscription { onEvent { n } }",
                            ),
                            operation_name: None,
                            operation_kind: OperationKind::Subscription,
                            input_rewrites: None,
                            output_rewrites: None,
                        },
                        rest,
                    })
                    .build(),
            );
            let supergraph_request = http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap();

            let events: BoxGqlStream = Box::pin(
                futures::stream::iter(1u64..)
                    .map(|n| graphql::Response::builder().data(json!({ "n": n })).build()),
            );
            let (stream_tx, stream_rx) = mpsc::channel(1);
            stream_tx.try_send(events).unwrap();

            let (client_tx, client_rx) = mpsc::channel(1);
            let (closed_tx, closed_rx) = broadcast::channel(1);
            let (params_tx, params_rx) = mpsc::channel(1);
            params_tx
                .try_send(SubscriptionTaskParams {
                    client_sender: client_tx,
                    subscription_handle: SubscriptionHandle::new(closed_rx, None),
                    subscription_config,
                    stream_rx: ReceiverStream::new(stream_rx),
                })
                .unwrap_or_else(|_| panic!("the params channel has room"));

            let (configuration_tx, configuration_rx) = mpsc::channel(1);
            let (schema_tx, schema_rx) = mpsc::channel(1);

            let task = tokio::spawn(subscription_task(
                execution_service,
                context,
                query_plan,
                params_rx,
                supergraph_request,
                Box::pin(ReceiverStream::new(configuration_rx)),
                Box::pin(ReceiverStream::new(schema_rx)),
            ));

            Self {
                client_rx,
                configuration_tx,
                schema_tx,
                _closed_tx: closed_tx,
                task,
            }
        }

        /// Reads `count` events and checks they continue the sequence from `first`.
        async fn expect_events(&mut self, first: u64, count: u64) {
            for n in first..first + count {
                let response = self.client_rx.recv().await.expect("an event");
                assert!(response.errors.is_empty(), "{response:?}");
                assert_eq!(response.data, Some(json!({ "n": n })));
            }
        }

        /// Reads events until the subscription is closed, and returns the close's error code.
        async fn expect_close(&mut self) -> String {
            for _ in 0..MAX_EVENTS_BEFORE_CLOSE {
                let response = self
                    .client_rx
                    .recv()
                    .await
                    .expect("the subscription sends an error before it ends");
                if let Some(error) = response.errors.first() {
                    return error.extension_code().expect("an error code");
                }
            }
            panic!("received {MAX_EVENTS_BEFORE_CLOSE} events after the close signal and no close");
        }

        /// Checks that nothing follows the close and the task has finished.
        async fn expect_ended(mut self) {
            if let Some(response) = self.client_rx.recv().await {
                panic!("received a response after the subscription closed: {response:?}");
            }
            self.task.await.unwrap();
        }

        /// Reads the next response, which must close the subscription before another event
        /// reaches the execution service, and returns the close's error code.
        async fn expect_close_before_next_execution(
            &mut self,
            execution: &mut ExecutionHandle,
        ) -> String {
            tokio::select! {
                response = self.client_rx.recv() => {
                    let response = response.expect("the subscription sends an error before it ends");
                    response.errors.first().and_then(|error| error.extension_code()).unwrap_or_else(|| panic!("expected the subscription to close, received {response:?}"))
                }
                Some((request, _)) = execution.next_request() => {
                    panic!("an event reached the execution service after the close signal: {:?}", request.source_stream_value);
                }
            }
        }
    }

    /// Waits for event `n` to reach the execution service, and returns the handle that answers it.
    async fn expect_execution(
        execution: &mut ExecutionHandle,
        n: u64,
    ) -> mock::SendResponse<execution::Response> {
        let (request, response) = execution.next_request().await.expect("an execution call");
        assert_eq!(request.source_stream_value, Some(json!({ "n": n })));
        response
    }

    fn executed_event(n: u64) -> execution::Response {
        execution::Response::fake_builder()
            .data(json!({ "n": n }))
            .build()
            .unwrap()
    }

    fn test_schema() -> Arc<Schema> {
        Arc::new(
            Schema::parse(
                include_str!("../../testdata/minimal_supergraph.graphql"),
                &Default::default(),
            )
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn delivers_events_in_order() {
        let mut harness = Harness::start(Context::new(), SubscriptionConfig::default());

        harness.expect_events(1, 50).await;
    }

    #[tokio::test]
    async fn schema_reload_closes_subscription_while_events_keep_arriving() {
        let mut harness = Harness::start(Context::new(), SubscriptionConfig::default());
        harness.expect_events(1, 3).await;

        harness.schema_tx.send(test_schema()).await.unwrap();

        assert_eq!(
            harness.expect_close().await,
            SUBSCRIPTION_SCHEMA_RELOAD_EXTENSION_CODE
        );
        harness.expect_ended().await;
    }

    #[tokio::test]
    async fn schema_reload_while_an_event_is_executing() {
        let (mut harness, mut execution) = Harness::start_with_execution();
        let event = expect_execution(&mut execution, 1).await;

        harness.schema_tx.send(test_schema()).await.unwrap();
        event.send_response(executed_event(1));

        harness.expect_events(1, 1).await;
        assert_eq!(
            harness
                .expect_close_before_next_execution(&mut execution)
                .await,
            SUBSCRIPTION_SCHEMA_RELOAD_EXTENSION_CODE
        );
        assert!(execution.next_request().await.is_none());
        harness.expect_ended().await;
    }

    #[tokio::test]
    async fn configuration_reload_closes_subscription_while_events_keep_arriving() {
        let mut harness = Harness::start(Context::new(), SubscriptionConfig::default());
        harness.expect_events(1, 3).await;

        harness.configuration_tx.send(Weak::new()).await.unwrap();

        assert_eq!(
            harness.expect_close().await,
            SUBSCRIPTION_CONFIG_RELOAD_EXTENSION_CODE
        );
        harness.expect_ended().await;
    }

    #[tokio::test]
    async fn configuration_reload_while_an_event_is_executing() {
        let (mut harness, mut execution) = Harness::start_with_execution();
        let event = expect_execution(&mut execution, 1).await;

        harness.configuration_tx.send(Weak::new()).await.unwrap();
        event.send_response(executed_event(1));

        harness.expect_events(1, 1).await;
        assert_eq!(
            harness
                .expect_close_before_next_execution(&mut execution)
                .await,
            SUBSCRIPTION_CONFIG_RELOAD_EXTENSION_CODE
        );
        assert!(execution.next_request().await.is_none());
        harness.expect_ended().await;
    }

    #[tokio::test(start_paused = true)]
    async fn jwt_expiry_closes_subscription_while_events_keep_arriving() {
        let context = Context::new();
        let expires_at =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap() + Duration::from_secs(60);
        context.insert_json_value(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json!({ "exp": expires_at.as_secs() }),
        );
        let mut harness = Harness::start(context, SubscriptionConfig::default());
        harness.expect_events(1, 3).await;

        tokio::time::advance(Duration::from_secs(61)).await;

        assert_eq!(
            harness.expect_close().await,
            SUBSCRIPTION_JWT_EXPIRED_EXTENSION_CODE
        );
        harness.expect_ended().await;
    }

    #[tokio::test(start_paused = true)]
    async fn max_lifetime_closes_subscription_while_events_keep_arriving() {
        let subscription_config = SubscriptionConfig {
            max_lifetime: Some(Duration::from_secs(60).into()),
            ..Default::default()
        };
        let mut harness = Harness::start(Context::new(), subscription_config);
        harness.expect_events(1, 3).await;

        tokio::time::advance(Duration::from_secs(61)).await;

        assert_eq!(
            harness.expect_close().await,
            SUBSCRIPTION_MAX_LIFETIME_EXTENSION_CODE
        );
        harness.expect_ended().await;
    }
}
