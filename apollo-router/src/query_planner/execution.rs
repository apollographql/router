use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::join_all;
use futures::prelude::*;
use indexmap::IndexSet;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_stream::wrappers::BroadcastStream;
use tower::ServiceExt;
use tracing::Instrument;

use super::DeferredNode;
use super::PlanNode;
use super::QueryPlan;
use super::log;
use super::selection::type_condition_matches;
use super::subscription::SubscriptionHandle;
use crate::Context;
use crate::axum_factory::CanceledRequest;
use crate::error::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::plugins::subscription::SubscriptionConfig;
use crate::query_planner::CONDITION_ELSE_SPAN_NAME;
use crate::query_planner::CONDITION_IF_SPAN_NAME;
use crate::query_planner::CONDITION_SPAN_NAME;
use crate::query_planner::DEFER_DEFERRED_SPAN_NAME;
use crate::query_planner::DEFER_PRIMARY_SPAN_NAME;
use crate::query_planner::DEFER_SPAN_NAME;
use crate::query_planner::FLATTEN_SPAN_NAME;
use crate::query_planner::FlattenNode;
use crate::query_planner::PARALLEL_SPAN_NAME;
use crate::query_planner::Primary;
use crate::query_planner::SEQUENCE_SPAN_NAME;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::SubgraphSchemas;
use crate::query_planner::fetch::Variables;
use crate::services::FetchRequest;
use crate::services::fetch;
use crate::services::fetch::ErrorMapping;
use crate::services::fetch::SubscriptionRequest;
use crate::services::fetch_service::FetchServiceFactory;
use crate::services::new_service::ServiceFactory;
use crate::spec::Fragment;
use crate::spec::Fragments;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::Selection;
use crate::spec::TYPENAME;

impl QueryPlan {
    #[allow(clippy::too_many_arguments)]
    /// Execute the plan and return a [`Response`].
    pub(crate) async fn execute<'a>(
        &self,
        context: &'a Context,
        service_factory: &'a Arc<FetchServiceFactory>,
        // The original supergraph request is used to populate variable values and for plugin
        // features like propagating headers or subgraph telemetry based on supergraph request
        // values.
        supergraph_request: &'a Arc<http::Request<Request>>,
        schema: &'a Arc<Schema>,
        subgraph_schemas: &'a Arc<SubgraphSchemas>,
        // Sender for additional responses past the first one (@defer, @stream, subscriptions)
        sender: mpsc::Sender<Response>,
        subscription_handle: Option<SubscriptionHandle>,
        subscription_config: &'a Option<SubscriptionConfig>,
        // Query plan execution builds up a JSON result value, use this as the initial data.
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
                    subgraph_schemas,
                },
                &root,
                &initial_value.unwrap_or_default(),
                sender,
            )
            .await;
        if !deferred_fetches.is_empty() {
            u64_counter!(
                "apollo.router.operations.defer",
                "Number of requests that request deferred data",
                1
            );
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
    pub(crate) service_factory: &'a Arc<FetchServiceFactory>,
    pub(crate) schema: &'a Arc<Schema>,
    pub(crate) subgraph_schemas: &'a Arc<SubgraphSchemas>,
    pub(crate) supergraph_request: &'a Arc<http::Request<Request>>,
    pub(crate) deferred_fetches: &'a HashMap<String, broadcast::Sender<(Value, Vec<Error>)>>,
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
        sender: mpsc::Sender<Response>,
    ) -> future::BoxFuture<'a, (Value, Vec<Error>)> {
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
                            value.type_aware_deep_merge(v, parameters.schema);
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
                            value.type_aware_deep_merge(v, parameters.schema);
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
                    let current_dir = current_dir.join(path.remove_empty_key_root());
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
                PlanNode::Subscription {
                    primary: subscription_node,
                    ..
                } => {
                    if parameters.subscription_handle.is_none() {
                        tracing::error!("No subscription handle provided for a subscription");
                        value = Value::default();
                        errors = vec![
                            Error::builder()
                                .message("no subscription handle provided for a subscription")
                                .extension_code("NO_SUBSCRIPTION_HANDLE")
                                .build(),
                        ];
                    } else {
                        // Note: subscriptions currently pass empty requires (&[]), so
                        // _unsatisfied_paths will always be empty.
                        let (opt_variables, _unsatisfied_paths) = Variables::new(
                            &[],
                            &subscription_node.variable_usages,
                            parent_value,
                            current_dir,
                            parameters.supergraph_request,
                            parameters.schema,
                            &subscription_node.input_rewrites,
                            &None,
                        );
                        match opt_variables {
                            Some(variables) => {
                                let service = parameters.service_factory.create();
                                let request = fetch::Request::Subscription(
                                    SubscriptionRequest::builder()
                                        .context(parameters.context.clone())
                                        .subscription_node(subscription_node.clone())
                                        .supergraph_request(parameters.supergraph_request.clone())
                                        .variables(variables)
                                        .current_dir(current_dir.clone())
                                        .sender(sender)
                                        .and_subscription_handle(
                                            parameters.subscription_handle.clone(),
                                        )
                                        .and_subscription_config(
                                            parameters.subscription_config.clone(),
                                        )
                                        .build(),
                                );
                                (value, errors) =
                                    match service.oneshot(request).await.map_to_graphql_error(
                                        subscription_node.service_name.to_string(),
                                        current_dir,
                                    ) {
                                        Ok(r) => r,
                                        Err(e) => (Value::default(), vec![e]),
                                    };
                            }
                            None => {
                                value = Value::Object(Object::default());
                                errors = Vec::new();
                            }
                        };
                    }
                }
                PlanNode::Fetch(fetch_node) => {
                    // The client closed the connection, we are still executing the request pipeline,
                    // but we won't send unused trafic to subgraph
                    if parameters
                        .context
                        .extensions()
                        .with_lock(|lock| lock.get::<CanceledRequest>().is_some())
                    {
                        value = Value::Object(Object::default());
                        errors = Vec::new();
                    } else {
                        let (opt_variables, unsatisfied_paths) = Variables::new(
                            &fetch_node.requires,
                            &fetch_node.variable_usages,
                            parent_value,
                            current_dir,
                            parameters.supergraph_request,
                            parameters.schema.as_ref(),
                            &fetch_node.input_rewrites,
                            &fetch_node.context_rewrites,
                        );

                        // Generate errors for any entities whose required fields were missing.
                        let mut skipped_errors = Vec::new();
                        for unsatisfied_path in &unsatisfied_paths {
                            skipped_errors.extend(errors_for_skipped_fetch(
                                parameters,
                                unsatisfied_path,
                                parent_value,
                            ));
                        }

                        match opt_variables {
                            Some(variables) => {
                                let paths = variables.inverted_paths.clone();
                                let service = parameters.service_factory.create();
                                let request = fetch::Request::Fetch(
                                    FetchRequest::builder()
                                        .context(parameters.context.clone())
                                        .fetch_node(fetch_node.clone())
                                        .supergraph_request(parameters.supergraph_request.clone())
                                        .variables(variables)
                                        .current_dir(current_dir.clone())
                                        .build(),
                                );
                                let raw_errors;
                                (value, raw_errors) =
                                    match service.oneshot(request).await.map_to_graphql_error(
                                        fetch_node.service_name.to_string(),
                                        current_dir,
                                    ) {
                                        Ok(r) => r,
                                        Err(e) => (Value::default(), vec![e]),
                                    };

                                // When a subgraph returns an unexpected response (ie not a body with
                                // at least one of errors or data), the errors surfaced by the router
                                // include an @ in the path. This indicates the error should be applied
                                // to all elements in the array.
                                errors = Vec::default();
                                for err in raw_errors {
                                    if let Some(err_path) = err.path.as_ref()
                                        && err_path
                                            .iter()
                                            .any(|elem| matches!(elem, PathElement::Flatten(_)))
                                    {
                                        for path in paths.iter().flatten() {
                                            if err_path.equal_if_flattened(path) {
                                                let mut err = err.clone();
                                                err.path = Some(path.clone());
                                                errors.push(err);
                                            }
                                        }

                                        continue;
                                    }

                                    errors.push(err);
                                }
                                errors.extend(skipped_errors);

                                FetchNode::deferred_fetches(
                                    current_dir,
                                    &fetch_node.id,
                                    parameters.deferred_fetches,
                                    &value,
                                    &errors,
                                );
                            }
                            None => {
                                value = Value::Object(Object::default());
                                errors = skipped_errors;
                            }
                        };
                    }
                }
                PlanNode::Defer {
                    primary: Primary { node, .. },
                    deferred,
                } => {
                    value = parent_value.clone();
                    errors = Vec::new();
                    async {
                        let mut deferred_fetches: HashMap<
                            String,
                            broadcast::Sender<(Value, Vec<Error>)>,
                        > = HashMap::new();
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
                                        subgraph_schemas: parameters.subgraph_schemas,
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
                            value.type_aware_deep_merge(v, parameters.schema);
                            errors.extend(err.into_iter());

                            let _ = primary_sender.send((value.clone(), errors.clone()));
                        } else {
                            let _ = primary_sender.send((value.clone(), errors.clone()));
                            // primary response should be an empty object
                            value.deep_merge(Value::Object(Default::default()));
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
                                value.type_aware_deep_merge(v, parameters.schema);
                                errors.extend(err.into_iter());
                            } else if current_dir.is_empty() {
                                // If the condition is on the root selection set and it's the only one
                                // For queries like {get @skip(if: true) {id name}}
                                value.deep_merge(Value::Object(Default::default()));
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
                            value.type_aware_deep_merge(v, parameters.schema);
                            errors.extend(err.into_iter());
                        } else if current_dir.is_empty() {
                            // If the condition is on the root selection set and it's the only one
                            // For queries like {get @include(if: false) {id name}}
                            value.deep_merge(Value::Object(Default::default()));
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
        sender: mpsc::Sender<Response>,
        primary_sender: &broadcast::Sender<(Value, Vec<Error>)>,
        deferred_fetches: &mut HashMap<String, broadcast::Sender<(Value, Vec<Error>)>>,
    ) -> impl Future<Output = ()> + use<> {
        let mut deferred_receivers = Vec::new();

        for d in self.depends.iter() {
            match deferred_fetches.get(&d.id) {
                None => {
                    let (sender, receiver) = tokio::sync::broadcast::channel(1);
                    deferred_fetches.insert(d.id.clone(), sender.clone());
                    deferred_receivers.push(StreamExt::into_future(BroadcastStream::new(receiver)));
                }
                Some(sender) => {
                    let receiver = sender.subscribe();
                    deferred_receivers.push(StreamExt::into_future(BroadcastStream::new(receiver)));
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
        let label = self.label.as_ref().map(|l| l.to_string());
        let tx = sender;
        let sc = parameters.schema.clone();
        let subgraph_schemas = parameters.subgraph_schemas.clone();
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
                value.type_aware_deep_merge(primary_value, &sc);
                errors.extend(primary_errors)
            } else {
                while let Some((v, _remaining)) = stream.next().await {
                    // a Err(RecvError) means either that the fetch was not performed and the
                    // sender was dropped, possibly because there was no need to do it,
                    // or because it is lagging, but here we only send one message so it
                    // will not happen
                    if let Some(Ok((deferred_value, err))) = v {
                        value.type_aware_deep_merge(deferred_value, &sc);
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
                            subgraph_schemas: &subgraph_schemas,
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
                    v.type_aware_deep_merge(primary_value, &sc);
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
                drop(tx);
            } else {
                let (primary_value, primary_errors) =
                    primary_receiver.recv().await.unwrap_or_default();
                value.type_aware_deep_merge(primary_value, &sc);
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
                drop(tx);
            };
        }
    }
}

/// Generate errors for fields that could not be fetched because the entity
/// fetch was skipped (a non-nullable `@requires` input field was missing).
///
/// Uses the supergraph query's selection set to find which fields are expected
/// at `current_dir`, then checks which of those are absent from `parent_value`.
/// Each missing field gets an error with its path.
/// Navigate a JSON value by following key segments in a path, returning
/// the nested value (or `Value::Null` if any segment is missing or
/// non-object).
fn errors_for_skipped_fetch(
    parameters: &ExecutionParameters<'_>,
    current_dir: &Path,
    parent_value: &Value,
) -> Vec<Error> {
    // Navigate parent_value to the object at current_dir
    let entity_data = get_value_at_path(parent_value, current_dir);

    let variables = &parameters.supergraph_request.body().variables;
    let expected_fields = collect_field_names_at_path(
        &parameters.query.operation.selection_set,
        &current_dir.0,
        parent_value,
        parameters.schema,
        variables,
        &parameters.query.fragments,
    );

    expected_fields
        .into_iter()
        .filter(|field_name| {
            // Only generate errors for fields NOT already present in parent_value
            match entity_data {
                Value::Object(obj) => !obj.contains_key(*field_name),
                _ => true,
            }
        })
        .map(|field_name| {
            let mut path = current_dir.clone();
            path.push(PathElement::Key(field_name.to_string(), None));
            Error::builder()
                .message("Could not fetch field")
                .path(path)
                .extension_code("UNSATISFIED_FETCH_CONDITION")
                .build()
        })
        .collect()
}

fn get_value_at_path<'a>(value: &'a Value, path: &Path) -> &'a Value {
    let mut current = value;
    for segment in &path.0 {
        match segment {
            PathElement::Key(k, _) => match current {
                Value::Object(obj) => {
                    current = match obj.get(k.as_str()) {
                        Some(v) => v,
                        None => &Value::Null,
                    };
                }
                _ => {
                    return &Value::Null;
                }
            },
            PathElement::Index(i) => match current {
                Value::Array(arr) => {
                    current = match arr.get(*i) {
                        Some(v) => v,
                        None => &Value::Null,
                    };
                }
                _ => {
                    return &Value::Null;
                }
            },
            _ => {
                return &Value::Null;
            }
        }
    }
    current
}

/// Extract `__typename` from a JSON value, returning `None` if not present.
fn typename_of(value: &Value) -> Option<&str> {
    match value {
        Value::Object(obj) => obj.get(TYPENAME).and_then(|v| v.as_str()),
        _ => None,
    }
}

/// Collect field names from a query selection set at the given path, handling inline fragments,
/// named fragment spreads, and @skip/@include conditions.
///
/// When `remaining_dir` is non-empty, navigates through selections (including fragments) to reach
/// the target depth. Once there, collects unique field names (excluding `__typename`), recursing
/// into fragments whose `type_condition` matches the runtime type at each level (read from
/// `current_value`'s `__typename`).
fn collect_field_names_at_path<'sel>(
    selection_set: &'sel [Selection],
    remaining_dir: &[PathElement],
    current_value: &Value,
    schema: &Schema,
    variables: &Object,
    fragments: &'sel Fragments,
) -> IndexSet<&'sel str> {
    let current_type = typename_of(current_value);
    if let Some((segment, rest)) = remaining_dir.split_first() {
        match segment {
            PathElement::Key(k, _) => {
                let key = k.as_str();
                // Look up the child value for the next navigation level
                let child_value = match current_value {
                    Value::Object(obj) => obj.get(key).unwrap_or(&Value::Null),
                    _ => &Value::Null,
                };
                let mut field_names = IndexSet::new();
                collect_field_names_at_path_inner(
                    selection_set,
                    key,
                    rest,
                    child_value,
                    current_type,
                    schema,
                    variables,
                    fragments,
                    &mut field_names,
                );
                field_names
            }
            PathElement::Index(i) => {
                // Array index doesn't correspond to a selection set field — navigate
                // into the array element value and continue with the same selection set.
                let child_value = match current_value {
                    Value::Array(arr) => arr.get(*i).unwrap_or(&Value::Null),
                    _ => &Value::Null,
                };
                collect_field_names_at_path(
                    selection_set,
                    rest,
                    child_value,
                    schema,
                    variables,
                    fragments,
                )
            }
            _ => IndexSet::default(),
        }
    } else {
        let mut field_names = IndexSet::new();
        collect_fields_from_selections(
            selection_set,
            current_type,
            schema,
            variables,
            fragments,
            &mut field_names,
        );
        field_names
    }
}

/// Navigate into a selection set looking for fields matching `key`, digging into inline fragments
/// and named fragment spreads. `child_value` is the JSON value at the child level. `current_type`
/// is the runtime type at this level for fragment type-condition checks. Collects field names
/// from all matching fields across all matching fragments (does not short-circuit on the first
/// match, since multiple fragments may contain the same key and contribute different sub-fields).
#[allow(clippy::too_many_arguments)]
fn collect_field_names_at_path_inner<'sel>(
    selection_set: &'sel [Selection],
    key: &str,
    remaining_dir: &[PathElement],
    child_value: &Value,
    current_type: Option<&str>,
    schema: &Schema,
    variables: &Object,
    fragments: &'sel Fragments,
    field_names: &mut IndexSet<&'sel str>,
) {
    for sel in selection_set {
        match sel {
            Selection::Field {
                name,
                alias,
                selection_set: Some(inner),
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                let field_name = alias.as_ref().unwrap_or(name);
                if field_name.as_str() == key {
                    field_names.extend(collect_field_names_at_path(
                        inner,
                        remaining_dir,
                        child_value,
                        schema,
                        variables,
                        fragments,
                    ));
                }
            }
            Selection::InlineFragment {
                type_condition,
                selection_set,
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if type_condition_matches(schema, current_type, type_condition) {
                    collect_field_names_at_path_inner(
                        selection_set,
                        key,
                        remaining_dir,
                        child_value,
                        current_type,
                        schema,
                        variables,
                        fragments,
                        field_names,
                    );
                }
            }
            Selection::FragmentSpread {
                name, include_skip, ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if let Some(Fragment {
                    type_condition,
                    selection_set,
                }) = fragments.get(name)
                    && type_condition_matches(schema, current_type, type_condition)
                {
                    collect_field_names_at_path_inner(
                        selection_set,
                        key,
                        remaining_dir,
                        child_value,
                        current_type,
                        schema,
                        variables,
                        fragments,
                        field_names,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Recursively collect field names from a selection set, expanding inline fragments and named
/// fragment spreads whose type_condition matches `entity_type`.
fn collect_fields_from_selections<'sel>(
    selection_set: &'sel [Selection],
    entity_type: Option<&str>,
    schema: &Schema,
    variables: &Object,
    fragments: &'sel Fragments,
    field_names: &mut IndexSet<&'sel str>,
) {
    for sel in selection_set {
        match sel {
            Selection::Field {
                name,
                alias,
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if name.as_str() == TYPENAME {
                    continue;
                }
                field_names.insert(alias.as_ref().unwrap_or(name).as_str());
            }
            Selection::InlineFragment {
                type_condition,
                selection_set,
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if type_condition_matches(schema, entity_type, type_condition) {
                    collect_fields_from_selections(
                        selection_set,
                        entity_type,
                        schema,
                        variables,
                        fragments,
                        field_names,
                    );
                }
            }
            Selection::FragmentSpread {
                name, include_skip, ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if let Some(Fragment {
                    type_condition,
                    selection_set,
                }) = fragments.get(name)
                    && type_condition_matches(schema, entity_type, type_condition)
                {
                    collect_fields_from_selections(
                        selection_set,
                        entity_type,
                        schema,
                        variables,
                        fragments,
                        field_names,
                    );
                }
            }
        }
    }
}
