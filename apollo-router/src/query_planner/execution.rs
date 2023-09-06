use std::collections::HashMap;
use std::sync::Arc;

use futures::future::join_all;
use futures::prelude::*;
use tokio::sync::broadcast::Sender;
use tokio_stream::wrappers::BroadcastStream;
use tracing::Instrument;

use super::log;
use super::subscription::SubscriptionHandle;
use super::DeferredNode;
use super::PlanNode;
use super::QueryPlan;
use crate::error::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::plugins::subscription::SubscriptionConfig;
use crate::query_planner::FlattenNode;
use crate::query_planner::Primary;
use crate::query_planner::CONDITION_ELSE_SPAN_NAME;
use crate::query_planner::CONDITION_IF_SPAN_NAME;
use crate::query_planner::CONDITION_SPAN_NAME;
use crate::query_planner::DEFER_DEFERRED_SPAN_NAME;
use crate::query_planner::DEFER_PRIMARY_SPAN_NAME;
use crate::query_planner::DEFER_SPAN_NAME;
use crate::query_planner::FETCH_SPAN_NAME;
use crate::query_planner::FLATTEN_SPAN_NAME;
use crate::query_planner::PARALLEL_SPAN_NAME;
use crate::query_planner::SEQUENCE_SPAN_NAME;
use crate::query_planner::SUBSCRIBE_SPAN_NAME;
use crate::services::SubgraphServiceFactory;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Context;

impl QueryPlan {
    #[allow(clippy::too_many_arguments)]
    /// Execute the plan and return a [`Response`].
    pub(crate) async fn execute<'a>(
        &self,
        context: &'a Context,
        service_factory: &'a Arc<SubgraphServiceFactory>,
        supergraph_request: &'a Arc<http::Request<Request>>,
        schema: &'a Arc<Schema>,
        sender: futures::channel::mpsc::Sender<Response>,
        subscription_handle: Option<SubscriptionHandle>,
        subscription_config: &'a Option<SubscriptionConfig>,
        initial_value: Option<Value>,
    ) -> Response {
        let root = Path::empty();

        log::trace_query_plan(&self.root);
        let deferred_fetches = HashMap::new();

        let (value, errors) = self
            .root
            .execute_recursively(
                &ExecutionParameters {
                    context,
                    service_factory,
                    schema,
                    supergraph_request,
                    deferred_fetches: &deferred_fetches,
                    query: &self.query,
                    root_node: &self.root,
                    subscription_handle: &subscription_handle,
                    subscription_config,
                },
                &root,
                &initial_value.unwrap_or_default(),
                sender,
            )
            .await;
        if !deferred_fetches.is_empty() {
            tracing::info!(monotonic_counter.apollo.router.operations.defer = 1u64);
        }

        Response::builder().data(value).errors(errors).build()
    }

    pub fn contains_mutations(&self) -> bool {
        self.root.contains_mutations()
    }

    pub fn subgraph_fetches(&self) -> usize {
        self.root.subgraph_fetches()
    }
}

// holds the query plan executon arguments that do not change between calls
pub(crate) struct ExecutionParameters<'a> {
    pub(crate) context: &'a Context,
    pub(crate) service_factory: &'a Arc<SubgraphServiceFactory>,
    pub(crate) schema: &'a Arc<Schema>,
    pub(crate) supergraph_request: &'a Arc<http::Request<Request>>,
    pub(crate) deferred_fetches: &'a HashMap<String, Sender<(Value, Vec<Error>)>>,
    pub(crate) query: &'a Arc<Query>,
    pub(crate) root_node: &'a PlanNode,
    pub(crate) subscription_handle: &'a Option<SubscriptionHandle>,
    pub(crate) subscription_config: &'a Option<SubscriptionConfig>,
}

impl PlanNode {
    pub(super) fn execute_recursively<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        current_dir: &'a Path,
        parent_value: &'a Value,
        sender: futures::channel::mpsc::Sender<Response>,
    ) -> future::BoxFuture<(Value, Vec<Error>)> {
        Box::pin(async move {
            tracing::trace!("executing plan:\n{:#?}", self);
            let mut value;
            let mut errors;

            match self {
                PlanNode::Sequence { nodes } => {
                    value = parent_value.clone();
                    errors = Vec::new();
                    async {
                        for node in nodes {
                            let (v, err) = node
                                .execute_recursively(
                                    parameters,
                                    current_dir,
                                    &value,
                                    sender.clone(),
                                )
                                .in_current_span()
                                .await;
                            value.deep_merge(v);
                            errors.extend(err.into_iter());
                        }
                    }
                    .instrument(tracing::info_span!(
                        SEQUENCE_SPAN_NAME,
                        "otel.kind" = "INTERNAL"
                    ))
                    .await
                }
                PlanNode::Parallel { nodes } => {
                    value = Value::default();
                    errors = Vec::new();
                    async {
                        let mut stream: stream::FuturesUnordered<_> = nodes
                            .iter()
                            .map(|plan| {
                                plan.execute_recursively(
                                    parameters,
                                    current_dir,
                                    parent_value,
                                    sender.clone(),
                                )
                                .in_current_span()
                            })
                            .collect();

                        while let Some((v, err)) = stream.next().in_current_span().await {
                            value.deep_merge(v);
                            errors.extend(err.into_iter());
                        }
                    }
                    .instrument(tracing::info_span!(
                        PARALLEL_SPAN_NAME,
                        "otel.kind" = "INTERNAL"
                    ))
                    .await
                }
                PlanNode::Flatten(FlattenNode { path, node }) => {
                    // Note that the span must be `info` as we need to pick this up in apollo tracing
                    let current_dir = current_dir.join(path);
                    let (v, err) = node
                        .execute_recursively(
                            parameters,
                            // this is the only command that actually changes the "current dir"
                            &current_dir,
                            parent_value,
                            sender,
                        )
                        .instrument(tracing::info_span!(
                            FLATTEN_SPAN_NAME,
                            "graphql.path" = %current_dir,
                            "otel.kind" = "INTERNAL"
                        ))
                        .await;

                    value = v;
                    errors = err;
                }
                PlanNode::Subscription { primary, .. } => {
                    if parameters.subscription_handle.is_some() {
                        let fetch_time_offset =
                            parameters.context.created_at.elapsed().as_nanos() as i64;
                        errors = primary
                            .execute_recursively(parameters, current_dir, parent_value, sender)
                            .instrument(tracing::info_span!(
                                SUBSCRIBE_SPAN_NAME,
                                "otel.kind" = "INTERNAL",
                                "apollo.subgraph.name" = primary.service_name.as_str(),
                                "apollo_private.sent_time_offset" = fetch_time_offset
                            ))
                            .await;
                    } else {
                        tracing::error!("No subscription handle provided for a subscription");
                        errors = vec![Error::builder()
                            .message("no subscription handle provided for a subscription")
                            .extension_code("NO_SUBSCRIPTION_HANDLE")
                            .build()];
                    };

                    value = Value::default();
                }
                PlanNode::Fetch(fetch_node) => {
                    let fetch_time_offset =
                        parameters.context.created_at.elapsed().as_nanos() as i64;
                    match fetch_node
                        .fetch_node(parameters, parent_value, current_dir)
                        .instrument(tracing::info_span!(
                            FETCH_SPAN_NAME,
                            "otel.kind" = "INTERNAL",
                            "apollo.subgraph.name" = fetch_node.service_name.as_str(),
                            "apollo_private.sent_time_offset" = fetch_time_offset
                        ))
                        .await
                    {
                        Ok((v, e)) => {
                            value = v;
                            errors = e;
                        }
                        Err(err) => {
                            failfast_error!("Fetch error: {}", err);
                            errors = vec![err.to_graphql_error(Some(current_dir.to_owned()))];
                            value = Value::default();
                        }
                    }
                }
                PlanNode::Defer {
                    primary:
                        Primary {
                            path: _primary_path,
                            node,
                            ..
                        },
                    deferred,
                } => {
                    value = parent_value.clone();
                    errors = Vec::new();
                    async {
                        let mut deferred_fetches: HashMap<String, Sender<(Value, Vec<Error>)>> =
                            HashMap::new();
                        let mut futures = Vec::new();

                        let (primary_sender, _) =
                            tokio::sync::broadcast::channel::<(Value, Vec<Error>)>(1);

                        for deferred_node in deferred {
                            let fut = deferred_node
                                .execute(
                                    parameters,
                                    parent_value,
                                    sender.clone(),
                                    &primary_sender,
                                    &mut deferred_fetches,
                                )
                                .in_current_span();

                            futures.push(fut);
                        }

                        tokio::task::spawn(async move {
                            join_all(futures).await;
                        });

                        if let Some(node) = node {
                            let (v, err) = node
                                .execute_recursively(
                                    &ExecutionParameters {
                                        context: parameters.context,
                                        service_factory: parameters.service_factory,
                                        schema: parameters.schema,
                                        supergraph_request: parameters.supergraph_request,
                                        deferred_fetches: &deferred_fetches,
                                        query: parameters.query,
                                        root_node: parameters.root_node,
                                        subscription_handle: parameters.subscription_handle,
                                        subscription_config: parameters.subscription_config,
                                    },
                                    current_dir,
                                    &value,
                                    sender,
                                )
                                .instrument(tracing::info_span!(
                                    DEFER_PRIMARY_SPAN_NAME,
                                    "otel.kind" = "INTERNAL"
                                ))
                                .await;
                            value.deep_merge(v);
                            errors.extend(err.into_iter());

                            let _ = primary_sender.send((value.clone(), errors.clone()));
                        } else {
                            let _ = primary_sender.send((value.clone(), errors.clone()));
                        }
                    }
                    .instrument(tracing::info_span!(
                        DEFER_SPAN_NAME,
                        "otel.kind" = "INTERNAL"
                    ))
                    .await
                }
                PlanNode::Condition {
                    condition,
                    if_clause,
                    else_clause,
                } => {
                    value = Value::default();
                    errors = Vec::new();

                    async {
                        let v = parameters
                            .query
                            .variable_value(
                                parameters
                                    .supergraph_request
                                    .body()
                                    .operation_name
                                    .as_deref(),
                                condition.as_str(),
                                &parameters.supergraph_request.body().variables,
                            )
                            .unwrap_or(&Value::Bool(true)); // the defer if clause is mandatory, and defaults to true

                        if let &Value::Bool(true) = v {
                            //FIXME: should we show an error if the if_node was not present?
                            if let Some(node) = if_clause {
                                let (v, err) = node
                                    .execute_recursively(
                                        parameters,
                                        current_dir,
                                        parent_value,
                                        sender.clone(),
                                    )
                                    .instrument(tracing::info_span!(
                                        CONDITION_IF_SPAN_NAME,
                                        "otel.kind" = "INTERNAL"
                                    ))
                                    .await;
                                value.deep_merge(v);
                                errors.extend(err.into_iter());
                            }
                        } else if let Some(node) = else_clause {
                            let (v, err) = node
                                .execute_recursively(
                                    parameters,
                                    current_dir,
                                    parent_value,
                                    sender.clone(),
                                )
                                .instrument(tracing::info_span!(
                                    CONDITION_ELSE_SPAN_NAME,
                                    "otel.kind" = "INTERNAL"
                                ))
                                .await;
                            value.deep_merge(v);
                            errors.extend(err.into_iter());
                        }
                    }
                    .instrument(tracing::info_span!(
                        CONDITION_SPAN_NAME,
                        "graphql.condition" = condition,
                        "otel.kind" = "INTERNAL"
                    ))
                    .await
                }
            }

            (value, errors)
        })
    }
}

impl DeferredNode {
    fn execute<'a>(
        &self,
        parameters: &'a ExecutionParameters<'a>,
        parent_value: &Value,
        sender: futures::channel::mpsc::Sender<Response>,
        primary_sender: &Sender<(Value, Vec<Error>)>,
        deferred_fetches: &mut HashMap<String, Sender<(Value, Vec<Error>)>>,
    ) -> impl Future<Output = ()> {
        let mut deferred_receivers = Vec::new();

        for d in self.depends.iter() {
            match deferred_fetches.get(&d.id) {
                None => {
                    let (sender, receiver) = tokio::sync::broadcast::channel(1);
                    deferred_fetches.insert(d.id.clone(), sender.clone());
                    deferred_receivers.push(BroadcastStream::new(receiver).into_future());
                }
                Some(sender) => {
                    let receiver = sender.subscribe();
                    deferred_receivers.push(BroadcastStream::new(receiver).into_future());
                }
            }
        }

        // if a deferred node has no depends (ie not waiting for data from fetches) then it has to
        // wait until the primary response is entirely created.
        //
        // If the depends list is not empty, the inner node can start working on the fetched data, then
        // it is merged into the primary response before applying the subselection
        let is_depends_empty = self.depends.is_empty();

        let mut stream: stream::FuturesUnordered<_> = deferred_receivers.into_iter().collect();
        //FIXME/ is there a solution without cloning the entire node? Maybe it could be moved instead?
        let deferred_inner = self.node.clone();
        let deferred_path = self.query_path.clone();
        let label = self.label.clone();
        let mut tx = sender;
        let sc = parameters.schema.clone();
        let orig = parameters.supergraph_request.clone();
        let sf = parameters.service_factory.clone();
        let root_node = parameters.root_node.clone();
        let ctx = parameters.context.clone();
        let query = parameters.query.clone();
        let subscription_handle = parameters.subscription_handle.clone();
        let subscription_config = parameters.subscription_config.clone();
        let mut primary_receiver = primary_sender.subscribe();
        let mut value = parent_value.clone();
        let depends_json = serde_json::to_string(&self.depends).unwrap_or_default();
        async move {
            let mut errors = Vec::new();

            if is_depends_empty {
                let (primary_value, primary_errors) =
                    primary_receiver.recv().await.unwrap_or_default();
                value.deep_merge(primary_value);
                errors.extend(primary_errors)
            } else {
                while let Some((v, _remaining)) = stream.next().await {
                    // a Err(RecvError) means either that the fetch was not performed and the
                    // sender was dropped, possibly because there was no need to do it,
                    // or because it is lagging, but here we only send one message so it
                    // will not happen
                    if let Some(Ok((deferred_value, err))) = v {
                        value.deep_merge(deferred_value);
                        errors.extend(err.into_iter())
                    }
                }
            }

            let deferred_fetches = HashMap::new();

            if let Some(node) = deferred_inner {
                let (mut v, err) = node
                    .execute_recursively(
                        &ExecutionParameters {
                            context: &ctx,
                            service_factory: &sf,
                            schema: &sc,
                            supergraph_request: &orig,
                            deferred_fetches: &deferred_fetches,
                            query: &query,
                            root_node: &root_node,
                            subscription_handle: &subscription_handle,
                            subscription_config: &subscription_config,
                        },
                        &Path::default(),
                        &value,
                        tx.clone(),
                    )
                    .instrument(tracing::info_span!(
                        DEFER_DEFERRED_SPAN_NAME,
                        "graphql.label" = label,
                        "graphql.depends" = depends_json,
                        "graphql.path" = deferred_path.to_string(),
                        "otel.kind" = "INTERNAL"
                    ))
                    .await;

                if !is_depends_empty {
                    let (primary_value, primary_errors) =
                        primary_receiver.recv().await.unwrap_or_default();
                    v.deep_merge(primary_value);
                    errors.extend(primary_errors)
                }

                if let Err(e) = tx
                    .send(
                        Response::builder()
                            .data(v)
                            .errors(err)
                            .and_path(Some(deferred_path.clone()))
                            .and_label(label)
                            .build(),
                    )
                    .await
                {
                    tracing::error!(
                        "error sending deferred response at path {}: {:?}",
                        deferred_path,
                        e
                    );
                };
                tx.disconnect();
            } else {
                let (primary_value, primary_errors) =
                    primary_receiver.recv().await.unwrap_or_default();
                value.deep_merge(primary_value);
                errors.extend(primary_errors);

                if let Err(e) = tx
                    .send(
                        Response::builder()
                            .data(value)
                            .errors(errors)
                            .and_path(Some(deferred_path.clone()))
                            .and_label(label)
                            .build(),
                    )
                    .await
                {
                    tracing::error!(
                        "error sending deferred response at path {}: {:?}",
                        deferred_path,
                        e
                    );
                }
                tx.disconnect();
            };
        }
    }
}
