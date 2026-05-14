use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::join_all;
use futures::prelude::*;
use parking_lot::Mutex;
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
        let skipped_entity_paths: Mutex<Vec<Path>> = Mutex::new(Vec::new());

        let (value, mut errors) = self
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
                    skipped_entity_paths: &skipped_entity_paths,
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

        // End-of-execution pass: walk the user's operation against the final
        // merged data and emit `UNSATISFIED_FETCH_CONDITION` for each missing
        // leaf that sits under a recorded skipped entity path.
        let skipped_paths = std::mem::take(&mut *skipped_entity_paths.lock());
        if !skipped_paths.is_empty() {
            collect_unsatisfied_fetch_errors(
                &self.query.operation.selection_set,
                &self.query.fragments,
                schema.as_ref(),
                &supergraph_request.body().variables,
                &value,
                &Path::empty(),
                &skipped_paths,
                &mut errors,
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
    /// Entity-level paths whose `_entities` fetch was skipped because a
    /// required input was unsatisfied. Accumulated across the entire
    /// execution; consumed at end-of-execution to generate
    /// `UNSATISFIED_FETCH_CONDITION` errors against the final merged data.
    pub(crate) skipped_entity_paths: &'a Mutex<Vec<Path>>,
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
                            errors.extend(err);
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
                            errors.extend(err);
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
                        debug_assert!(
                            _unsatisfied_paths.is_empty(),
                            "subscriptions pass empty `requires` — unsatisfied_paths should always be empty",
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

                        // Record skipped entity paths; error generation is deferred to
                        // end-of-execution where the final merged data is available.
                        if !unsatisfied_paths.is_empty() {
                            parameters
                                .skipped_entity_paths
                                .lock()
                                .extend(unsatisfied_paths);
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
                                errors = Vec::new();
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
                                        skipped_entity_paths: parameters.skipped_entity_paths,
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
                            errors.extend(err);

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
                                errors.extend(err);
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
                            errors.extend(err);
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
                        errors.extend(err)
                    }
                }
            }

            let deferred_fetches = HashMap::new();
            let deferred_skipped_paths: Mutex<Vec<Path>> = Mutex::new(Vec::new());

            if let Some(node) = deferred_inner {
                let (mut v, mut err) = node
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
                            skipped_entity_paths: &deferred_skipped_paths,
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

                // End-of-chunk pass for this deferred response.
                let skipped_paths = std::mem::take(&mut *deferred_skipped_paths.lock());
                if !skipped_paths.is_empty() {
                    collect_unsatisfied_fetch_errors(
                        &query.operation.selection_set,
                        &query.fragments,
                        &sc,
                        &orig.body().variables,
                        &v,
                        &Path::empty(),
                        &skipped_paths,
                        &mut err,
                    );
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

/// End-of-execution walker: traverse the user's selection set against the final
/// merged response data and emit `UNSATISFIED_FETCH_CONDITION` for each missing
/// field whose path sits under an entry in `skipped_paths`.
///
/// `skipped_paths` is the accumulator filled by `PlanNode::Fetch` execution:
/// one path per entity whose `_entities` fetch was skipped because a required
/// input was unsatisfied (paths are entity-root, not per-leaf).
///
/// A path is considered "under a skipped path" if some prefix equals an entry
/// in `skipped_paths`. A field is considered "missing" if either it's absent
/// from the value object or it's present-but-null. Missing fields that aren't
/// under any skipped path are left alone — they're either explained by an
/// existing subgraph error, or fall outside this PR's scope.
#[allow(clippy::too_many_arguments)]
fn collect_unsatisfied_fetch_errors(
    selection_set: &[Selection],
    fragments: &Fragments,
    schema: &Schema,
    variables: &Object,
    value: &Value,
    current_path: &Path,
    skipped_paths: &[Path],
    output: &mut Vec<Error>,
) {
    let current_type = value
        .as_object()
        .and_then(|o| o.get(TYPENAME))
        .and_then(|v| v.as_str());

    for selection in selection_set {
        match selection {
            Selection::Field {
                name,
                alias,
                selection_set: sub,
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if name.as_str() == TYPENAME {
                    continue;
                }
                let response_key = alias.as_ref().unwrap_or(name).as_str();
                let field_value = value.as_object().and_then(|o| o.get(response_key));

                // Treat absent OR null as "missing".
                let missing = match field_value {
                    None => true,
                    Some(v) => v.is_null(),
                };

                let mut field_path = current_path.clone();
                field_path.push(PathElement::Key(response_key.to_string(), None));

                if missing {
                    if is_under_skipped_path(&field_path, skipped_paths) {
                        output.push(
                            Error::builder()
                                .message("Could not fetch field")
                                .path(field_path)
                                .extension_code("UNSATISFIED_FETCH_CONDITION")
                                .build(),
                        );
                    }
                    continue;
                }

                // Composite: recurse into the field's value if it has a sub-selection.
                if let Some(sub_sel) = sub
                    && let Some(field_value) = field_value
                {
                    descend_into_value(
                        sub_sel,
                        fragments,
                        schema,
                        variables,
                        field_value,
                        &field_path,
                        skipped_paths,
                        output,
                    );
                }
            }
            Selection::InlineFragment {
                type_condition,
                selection_set: sub,
                include_skip,
                known_type,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                let effective_type = known_type.as_deref().or(current_type);
                if type_condition_matches(schema, effective_type, type_condition) {
                    collect_unsatisfied_fetch_errors(
                        sub,
                        fragments,
                        schema,
                        variables,
                        value,
                        current_path,
                        skipped_paths,
                        output,
                    );
                }
            }
            Selection::FragmentSpread {
                name,
                known_type,
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                let Some(fragment) = fragments.get(name) else {
                    continue;
                };
                let effective_type = known_type.as_deref().or(current_type);
                if type_condition_matches(schema, effective_type, &fragment.type_condition) {
                    collect_unsatisfied_fetch_errors(
                        &fragment.selection_set,
                        fragments,
                        schema,
                        variables,
                        value,
                        current_path,
                        skipped_paths,
                        output,
                    );
                }
            }
        }
    }
}

/// Recurse into a field's value; if it's an array, iterate per-index with
/// `PathElement::Index` segments. Otherwise descend into the value as an object.
#[allow(clippy::too_many_arguments)]
fn descend_into_value(
    selection_set: &[Selection],
    fragments: &Fragments,
    schema: &Schema,
    variables: &Object,
    value: &Value,
    current_path: &Path,
    skipped_paths: &[Path],
    output: &mut Vec<Error>,
) {
    if let Some(arr) = value.as_array() {
        for (i, item) in arr.iter().enumerate() {
            let mut item_path = current_path.clone();
            item_path.push(PathElement::Index(i));
            descend_into_value(
                selection_set,
                fragments,
                schema,
                variables,
                item,
                &item_path,
                skipped_paths,
                output,
            );
        }
    } else {
        collect_unsatisfied_fetch_errors(
            selection_set,
            fragments,
            schema,
            variables,
            value,
            current_path,
            skipped_paths,
            output,
        );
    }
}

/// True iff `path` has some prefix in `skipped`. Equality counts (a skipped
/// entity path is "under itself"), so leaves of the skipped entity directly
/// match.
fn is_under_skipped_path(path: &Path, skipped: &[Path]) -> bool {
    skipped
        .iter()
        .any(|sp| sp.len() <= path.len() && sp.iter().zip(path.iter()).all(|(a, b)| a == b))
}
