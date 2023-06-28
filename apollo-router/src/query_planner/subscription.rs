use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Instant;

use futures::channel::mpsc;
use futures::channel::mpsc::SendError;
use futures::future;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tokio::sync::broadcast;
use tower::ServiceExt;
use tracing::field;
use tracing::Span;
use tracing_futures::Instrument;

use super::execution::ExecutionParameters;
use super::fetch::Variables;
use super::rewrites;
use super::rewrites::DataRewrite;
use super::OperationKind;
use super::PlanNode;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::http_ext;
use crate::json_ext::Path;
use crate::notification::HandleStream;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use crate::plugins::telemetry::GRAPHQL_OPERATION_NAME_CONTEXT_KEY;
use crate::plugins::telemetry::LOGGING_DISPLAY_BODY;
use crate::query_planner::SUBSCRIBE_SPAN_NAME;
use crate::services::SubgraphRequest;

pub(crate) const SUBSCRIPTION_EVENT_SPAN_NAME: &str = "subscription_event";
pub(crate) static OPENED_SUBSCRIPTIONS: AtomicUsize = AtomicUsize::new(0);
pub(crate) struct SubscriptionHandle {
    pub(crate) closed_signal: broadcast::Receiver<()>,
}

impl Clone for SubscriptionHandle {
    fn clone(&self) -> Self {
        Self {
            closed_signal: self.closed_signal.resubscribe(),
        }
    }
}

impl SubscriptionHandle {
    pub(crate) fn new(closed_signal: broadcast::Receiver<()>) -> Self {
        Self { closed_signal }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubscriptionNode {
    /// The name of the service or subgraph that the subscription is querying.
    pub(crate) service_name: String,

    /// The variables that are used for the subgraph subscription.
    pub(crate) variable_usages: Vec<String>,

    /// The GraphQL subquery that is used for the subscription.
    pub(crate) operation: String,

    /// The GraphQL subquery operation name.
    pub(crate) operation_name: Option<String>,

    /// The GraphQL operation kind that is used for the fetch.
    pub(crate) operation_kind: OperationKind,

    // Optionally describes a number of "rewrites" that query plan executors should apply to the data that is sent as input of this subscription.
    pub(crate) input_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that received from a subscription (and before it is applied to the current in-memory results).
    pub(crate) output_rewrites: Option<Vec<rewrites::DataRewrite>>,
}

impl SubscriptionNode {
    pub(crate) fn execute_recursively<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        current_dir: &'a Path,
        parent_value: &'a Value,
        mut sender: futures::channel::mpsc::Sender<Response>,
        rest: &'a Option<Box<PlanNode>>,
    ) -> future::BoxFuture<Vec<Error>> {
        if parameters.subscription_handle.is_none() {
            tracing::error!("No subscription handle provided for a subscription");
            return Box::pin(async {
                vec![Error::builder()
                    .message("no subscription handle provided for a subscription")
                    .extension_code("NO_SUBSCRIPTION_HANDLE")
                    .build()]
            });
        };
        if let Some(max_opened_subscriptions) = parameters
            .subscription_config
            .as_ref()
            .and_then(|s| s.max_opened_subscriptions)
        {
            if OPENED_SUBSCRIPTIONS.load(Ordering::Relaxed) >= max_opened_subscriptions {
                return Box::pin(async {
                    vec![Error::builder()
                        .message("can't open new subscription, limit reached")
                        .extension_code("SUBSCRIPTION_MAX_LIMIT")
                        .build()]
                });
            }
        }
        let subscription_handle = parameters
            .subscription_handle
            .as_ref()
            .expect("checked above; qed");
        let mode = match parameters.subscription_config.as_ref().map(|c| &c.mode) {
            Some(mode) => mode.get_subgraph_config(&self.service_name),
            None => {
                return Box::pin(async {
                    vec![Error::builder()
                        .message("subscription support is not enabled")
                        .extension_code("SUBSCRIPTION_DISABLED")
                        .build()]
                });
            }
        };
        let output_rewrites = self.output_rewrites.clone();
        let service_name = self.service_name.clone();

        Box::pin(async move {
            let cloned_qp = parameters.root_node.clone();
            let current_dir_cloned = current_dir.clone();
            let context = parameters.context.clone();
            let service_factory = parameters.service_factory.clone();
            let schema = parameters.schema.clone();
            let supergraph_request = parameters.supergraph_request.clone();
            let deferred_fetches = parameters.deferred_fetches.clone();
            let query = parameters.query.clone();
            let subscription_handle = subscription_handle.clone();
            let subscription_config = parameters.subscription_config.clone();
            let rest = rest.clone();

            match mode {
                Some(_) => {
                    let (tx_handle, mut rx_handle) =
                        mpsc::channel::<HandleStream<String, graphql::Response>>(1);

                    let _subscription_task = tokio::task::spawn(async move {
                        let sub_handle = match rx_handle.next().await {
                            Some(ws) => ws,
                            None => {
                                tracing::debug!("cannot get the graphql subscription stream");
                                let _ = sender.send(graphql::Response::builder().error(graphql::Error::builder().message("cannot get the subscription stream from subgraph").extension_code("SUBSCRIPTION_STREAM_GET").build()).build()).await;
                                return;
                            }
                        };

                        let parameters = ExecutionParameters {
                            context: &context,
                            service_factory: &service_factory,
                            schema: &schema,
                            supergraph_request: &supergraph_request,
                            deferred_fetches: &deferred_fetches,
                            query: &query,
                            root_node: &cloned_qp,
                            subscription_handle: &Some(subscription_handle),
                            subscription_config: &subscription_config,
                        };

                        Self::task(
                            sub_handle,
                            &parameters,
                            rest,
                            output_rewrites,
                            &current_dir_cloned,
                            sender.clone(),
                            service_name,
                        )
                        .await;
                    });

                    let fetch_time_offset =
                        parameters.context.created_at.elapsed().as_nanos() as i64;
                    match self
                        .subgraph_call(parameters, current_dir, parent_value, tx_handle)
                        .instrument(tracing::info_span!(
                            SUBSCRIBE_SPAN_NAME,
                            "otel.kind" = "INTERNAL",
                            "apollo.subgraph.name" = self.service_name.as_str(),
                            "apollo_private.sent_time_offset" = fetch_time_offset
                        ))
                        .await
                    {
                        Ok(e) => e,
                        Err(err) => {
                            failfast_error!("subgraph call fetch error: {}", err);
                            vec![err.to_graphql_error(Some(current_dir.to_owned()))]
                        }
                    }
                }
                None => {
                    vec![Error::builder()
                        .message(format!(
                            "subscription mode is not configured for subgraph {:?}",
                            self.service_name
                        ))
                        .extension_code("INVALID_SUBSCRIPTION_MODE")
                        .build()]
                }
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn task<'a>(
        mut receiver: impl Stream<Item = graphql::Response> + Unpin,
        parameters: &'a ExecutionParameters<'a>,
        rest: Option<Box<PlanNode>>,
        output_rewrites: Option<Vec<DataRewrite>>,
        current_dir: &'a Path,
        mut sender: futures::channel::mpsc::Sender<Response>,
        service_name: String,
    ) {
        let limit_is_set = parameters
            .subscription_config
            .as_ref()
            .and_then(|s| s.max_opened_subscriptions)
            .is_some();

        if limit_is_set {
            OPENED_SUBSCRIPTIONS.fetch_add(1, Ordering::Relaxed);
        }
        let mut subscription_handle = parameters
            .subscription_handle
            .clone()
            .expect("it has already been checked before; qed");

        let operation_signature = if let Some(usage_reporting) = parameters
            .context
            .private_entries
            .lock()
            .get::<UsageReporting>()
        {
            usage_reporting.stats_report_key.clone()
        } else {
            String::new()
        };

        let operation_name = parameters
            .context
            .get::<_, String>(GRAPHQL_OPERATION_NAME_CONTEXT_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        let display_body = parameters.context.contains_key(LOGGING_DISPLAY_BODY);

        loop {
            tokio::select! {
                _ = subscription_handle.closed_signal.recv() => {
                    break;
                }
                message = receiver.next() => {
                    match message {
                        Some(mut val) => {
                            if display_body {
                                tracing::info!(http.request.body = ?val, apollo.subgraph.name = %service_name, "Subscription event body from subgraph {service_name:?}");
                            }
                            val.created_at = Some(Instant::now());
                            if let Some(data) = &mut val.data {
                                rewrites::apply_rewrites(parameters.schema, data, &output_rewrites);
                            }


                            if let Err(err) =
                                Self::dispatch_value(val, parameters, &rest, current_dir, sender.clone())
                                    .instrument(tracing::info_span!(SUBSCRIPTION_EVENT_SPAN_NAME,
                                        graphql.document = parameters.query.string,
                                        graphql.operation.name = %operation_name,
                                        otel.kind = "INTERNAL",
                                        apollo_private.operation_signature = %operation_signature,
                                        apollo_private.duration_ns = field::Empty,)
                                    )
                                    .await
                            {
                                if !err.is_disconnected() {
                                    tracing::error!("cannot send the subscription to the client: {err:?}");
                                }
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        if let Err(err) = sender.close().await {
            tracing::trace!("cannot close the sender {err:?}");
        }

        tracing::trace!("Leaving the task for subscription");
        if limit_is_set {
            OPENED_SUBSCRIPTIONS.fetch_sub(1, Ordering::Relaxed);
        }
    }

    async fn dispatch_value<'a>(
        mut val: graphql::Response,
        parameters: &'a ExecutionParameters<'a>,
        rest: &Option<Box<PlanNode>>,
        current_dir: &'a Path,
        mut sender: futures::channel::mpsc::Sender<Response>,
    ) -> Result<(), SendError> {
        let start = Instant::now();
        let span = Span::current();

        match rest {
            Some(rest) => {
                let created_at = val.created_at.take();
                let (value, mut errors) = rest
                    .execute_recursively(
                        parameters,
                        current_dir,
                        &val.data.unwrap_or_default(),
                        sender.clone(),
                    )
                    .in_current_span()
                    .await;

                errors.append(&mut val.errors);

                sender
                    .send(
                        Response::builder()
                            .data(value)
                            .and_subscribed(val.subscribed)
                            .errors(errors)
                            .extensions(val.extensions)
                            .and_path(val.path)
                            .and_created_at(created_at)
                            .build(),
                    )
                    .await?;
            }
            None => {
                sender.send(val).await?;
            }
        }
        span.record(
            APOLLO_PRIVATE_DURATION_NS,
            start.elapsed().as_nanos() as i64,
        );

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn subgraph_call<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        current_dir: &'a Path,
        data: &Value,
        tx_gql: mpsc::Sender<HandleStream<String, graphql::Response>>,
    ) -> Result<Vec<Error>, FetchError> {
        let SubscriptionNode {
            operation,
            operation_name,
            service_name,
            ..
        } = self;

        let Variables { variables, .. } = match Variables::new(
            &[],
            self.variable_usages.as_ref(),
            data,
            current_dir,
            // Needs the original request here
            parameters.supergraph_request,
            parameters.schema,
            &self.input_rewrites,
        )
        .await
        {
            Some(variables) => variables,
            None => {
                return Ok(Vec::new());
            }
        };

        let subgraph_request = SubgraphRequest::builder()
            .supergraph_request(parameters.supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(
                        parameters
                            .schema
                            .subgraph_url(service_name)
                            .unwrap_or_else(|| {
                                panic!(
                                    "schema uri for subgraph '{service_name}' should already have been checked"
                                )
                            })
                            .clone(),
                    )
                    .body(
                        Request::builder()
                            .query(operation)
                            .and_operation_name(operation_name.clone())
                            .variables(variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .operation_kind(OperationKind::Subscription)
            .context(parameters.context.clone())
            .subscription_stream(tx_gql)
            .and_connection_closed_signal(parameters.subscription_handle.as_ref().map(|s| s.closed_signal.resubscribe()))
            .build();

        let service = parameters
            .service_factory
            .create(service_name)
            .expect("we already checked that the service exists during planning; qed");

        let (_parts, response) = service
            .oneshot(subgraph_request)
            .instrument(tracing::trace_span!("subscription_call"))
            .await
            // TODO this is a problem since it restores details about failed service
            // when errors have been redacted in the include_subgraph_errors module.
            // Unfortunately, not easy to fix here, because at this point we don't
            // know if we should be redacting errors for this subgraph...
            .map_err(|e| FetchError::SubrequestHttpError {
                service: service_name.to_string(),
                reason: e.to_string(),
                status_code: None,
            })?
            .response
            .into_parts();

        Ok(response.errors)
    }
}
