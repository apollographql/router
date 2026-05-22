use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable;
use futures::StreamExt;
use futures::future::join_all;
use futures::prelude::*;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_stream::wrappers::BroadcastStream;
use tower::ServiceExt;
use tracing::Instrument;

use super::DeferredNode;
use super::PlanNode;
use super::QueryPlan;
use super::log;
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
use crate::spec::IncludeSkip;
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
        let skipped_entity_paths = Default::default();

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

        let skipped_entity_paths = skipped_entity_paths.lock().await;
        emit_unsatisfied_fetch_errors(
            &skipped_entity_paths,
            &self.query,
            schema.as_ref(),
            &supergraph_request.body().variables,
            &value,
            &mut errors,
        );

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
    pub(crate) skipped_entity_paths: &'a Mutex<Vec<(Path, Arc<ResponseKeyTree>)>>,
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

                        if !unsatisfied_paths.is_empty() {
                            record_skipped_entities(
                                unsatisfied_paths,
                                fetch_node,
                                parent_value,
                                parameters.schema.as_ref(),
                                &parameters.supergraph_request.body().variables,
                                parameters.skipped_entity_paths,
                            )
                            .await;
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
            let deferred_skipped_paths: Mutex<Vec<(Path, Arc<ResponseKeyTree>)>> =
                Mutex::new(Vec::new());

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

                let deferred_skipped_paths = deferred_skipped_paths.lock().await;
                emit_unsatisfied_fetch_errors(
                    &deferred_skipped_paths,
                    &query,
                    &sc,
                    &orig.body().variables,
                    &v,
                    &mut err,
                );

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

// ──────────────────────────────────────────────────────────────────────────
// Skipped-entity error generation
// ──────────────────────────────────────────────────────────────────────────

/// Tree of response keys produced by a subgraph fetch for one entity,
/// over-approximated: we descend through every selection (including lists and
/// undecidable type conditions) so that every possible leaf the skipped fetch
/// could have produced shows up in `children`. Leaves are never fetched twice
/// by another subgraph, so any missing leaf in the final response is a true
/// positive `UNSATISFIED_FETCH_CONDITION`. Intermediate composite fields may
/// be filled by another subgraph — those are not by themselves errors, only
/// their genuinely-missing leaves are.
#[derive(Default, Debug)]
pub(crate) struct ResponseKeyTree {
    children: HashMap<Name, ResponseKeyTree>,
}

impl ResponseKeyTree {
    fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

/// For each entity in `unsatisfied_paths`, build the `ResponseKeyTree` of
/// response keys the skipped fetch would have produced for that entity
/// (filtered by the entity's `__typename`) and append it to `accumulator`.
///
/// Trees are deduped per `__typename` so a batched `_entities` skip of N
/// entities of the same type costs one tree build and N `Arc::clone`s.
async fn record_skipped_entities(
    unsatisfied_paths: Vec<Path>,
    fetch_node: &FetchNode,
    parent_value: &Value,
    schema: &Schema,
    variables: &Object,
    accumulator: &Mutex<Vec<(Path, Arc<ResponseKeyTree>)>>,
) {
    let Ok(parsed) = fetch_node.operation.as_parsed() else {
        // shouldn't happen -> skip recording
        return;
    };
    let operation = parsed.operations.get(fetch_node.operation_name.as_deref());
    let Ok(operation) = operation else {
        // shouldn't happen -> skip recording
        return;
    };

    let mut accumulator = accumulator.lock().await;
    let mut tree_cache: HashMap<String, Arc<ResponseKeyTree>> = HashMap::new();
    for entity_path in unsatisfied_paths {
        let entity_data = get_value_at_path(parent_value, &entity_path);
        let entity_type = typename_of(entity_data).map(str::to_owned);
        let Some(entity_type) = entity_type else {
            // No `__typename` on the entity data - Drop this path silently rather than guessing.
            continue;
        };
        let tree = tree_cache.entry(entity_type.clone()).or_insert_with(|| {
            let tree = entity_response_key_tree(
                operation,
                &parsed.fragments,
                &entity_type,
                schema,
                variables,
            );
            Arc::new(tree)
        });
        if !tree.is_empty() {
            accumulator.push((entity_path, tree.clone()));
        }
    }
}

/// Build the over-approximated tree of response keys for a subgraph
/// `_entities` fetch, anchored at `entity_type` (the runtime `__typename`).
/// Iterates the operation's top-level `_entities` selection and accumulates
/// keys via `collect_response_key_tree`. Returns an empty tree when the
/// operation has no `_entities` field, or every fragment at that level
/// definitely does not match the entity type.
fn entity_response_key_tree(
    operation: &apollo_compiler::Node<executable::Operation>,
    fragments: &IndexMap<Name, apollo_compiler::Node<executable::Fragment>>,
    entity_type: &str,
    schema: &Schema,
    variables: &Object,
) -> ResponseKeyTree {
    let mut tree = ResponseKeyTree::default();
    for sel in &operation.selection_set.selections {
        if let executable::Selection::Field(f) = sel
            && f.name.as_str() == "_entities"
        {
            collect_response_key_tree(
                &f.selection_set,
                Some(entity_type),
                fragments,
                variables,
                schema,
                &mut tree,
            );
        }
    }
    tree
}

/// Static three-valued resolution of a fragment type condition against the current type. Returns
/// following values:
/// - `Some(true)` when the condition definitely matches the runtime type
/// - `Some(false)` when it definitely does not
/// - `None` when static analysis cannot decide (the current type is abstract, or we have no
///   current type because we descended into a field and lost type info).
fn static_type_condition_match(
    schema: &Schema,
    current_type: &str,
    type_condition: &str,
) -> Option<bool> {
    if current_type == type_condition {
        return Some(true);
    }
    let current = schema.supergraph_schema().types.get(current_type)?;
    let cond = schema.supergraph_schema().types.get(type_condition)?;

    use apollo_compiler::schema::ExtendedType::*;
    match current {
        Object(o) => match cond {
            Object(c) => Some(o.name == c.name),
            Interface(i) => Some(o.implements_interfaces.contains(&i.name)),
            Union(u) => Some(u.members.contains(&o.name)),
            _ => Some(false),
        },
        // Abstract current type: undecidable without runtime info.
        Interface(_) | Union(_) => None,
        _ => Some(false),
    }
}

/// Pick the more concrete of `current` and `condition` as the new current type.
fn walker_refine_type<'a>(schema: &Schema, current: &'a str, condition: &'a str) -> &'a str {
    use apollo_compiler::schema::ExtendedType::*;
    if matches!(
        schema.supergraph_schema().types.get(current),
        Some(Object(_))
    ) {
        return current;
    }
    if matches!(
        schema.supergraph_schema().types.get(condition),
        Some(Object(_))
    ) {
        return condition;
    }
    current
}

/// Pick the more concrete of two types as the current type. Prefers a concrete object type over an
/// abstract interface/union or an unknown type.
fn refine_runtime_type<'a>(
    schema: &Schema,
    runtime: Option<&'a str>,
    condition: &'a str,
) -> Option<&'a str> {
    use apollo_compiler::schema::ExtendedType::*;
    if matches!(
        runtime.and_then(|t| schema.supergraph_schema().types.get(t)),
        Some(Object(_))
    ) {
        return runtime;
    }
    if matches!(
        schema.supergraph_schema().types.get(condition),
        Some(Object(_))
    ) {
        return Some(condition);
    }
    runtime
}

/// Recursively populate `tree` from a subgraph selection set, over-approximating
/// the keys the skipped fetch would have produced.
///
/// "Over-approximate" means:
/// - Descend through every composite field's sub-selection (including list-typed
///   fields), so leaves nested arbitrarily deep land in `tree`.
/// - Descend through inline fragments and named fragment spreads even when
///   their type condition is statically undecidable (the current type is
///   abstract, or we descended into a field and lost runtime type info).
///   Definite no-match (`Some(false)`) is the only short-circuit.
///
/// We may collect leaves that wouldn't actually have been produced at runtime
/// (e.g. a fragment on a sibling concrete object that didn't end up matching
/// the runtime type), but that is fine: a leaf is only emitted as an error
/// when it's also missing from the final merged response. Since leaves are
/// never fetched twice across subgraphs, a missing leaf from this set is a
/// true positive — `UNSATISFIED_FETCH_CONDITION` for the skipped fetch.
fn collect_response_key_tree(
    selection_set: &executable::SelectionSet,
    runtime_type: Option<&str>,
    fragments: &IndexMap<Name, apollo_compiler::Node<executable::Fragment>>,
    variables: &Object,
    schema: &Schema,
    tree: &mut ResponseKeyTree,
) {
    let current_type = runtime_type.unwrap_or(selection_set.ty.as_str());
    for sel in &selection_set.selections {
        match sel {
            executable::Selection::Field(f) => {
                if IncludeSkip::parse(&f.directives).should_skip(variables) {
                    continue;
                }
                let key = f.response_key();
                if key.as_str() == TYPENAME {
                    continue;
                }
                let entry = tree.children.entry(key.clone()).or_default();
                if !f.selection_set.selections.is_empty() {
                    collect_response_key_tree(
                        &f.selection_set,
                        None,
                        fragments,
                        variables,
                        schema,
                        entry,
                    );
                }
            }
            executable::Selection::InlineFragment(frag) => {
                if IncludeSkip::parse(&frag.directives).should_skip(variables) {
                    continue;
                }
                let matches = match frag.type_condition.as_ref() {
                    None => Some(true),
                    Some(cond) => static_type_condition_match(schema, current_type, cond.as_str()),
                };
                if matches == Some(false) {
                    continue;
                }
                let refined = match frag.type_condition.as_ref() {
                    Some(cond) => refine_runtime_type(schema, runtime_type, cond.as_str()),
                    None => runtime_type,
                };
                collect_response_key_tree(
                    &frag.selection_set,
                    refined,
                    fragments,
                    variables,
                    schema,
                    tree,
                );
            }
            executable::Selection::FragmentSpread(spread) => {
                if IncludeSkip::parse(&spread.directives).should_skip(variables) {
                    continue;
                }
                let Some(fragment) = fragments.get(&spread.fragment_name) else {
                    continue;
                };
                let cond = fragment.type_condition().as_str();
                if static_type_condition_match(schema, current_type, cond) == Some(false) {
                    continue;
                }
                let refined = refine_runtime_type(schema, runtime_type, cond);
                collect_response_key_tree(
                    &fragment.selection_set,
                    refined,
                    fragments,
                    variables,
                    schema,
                    tree,
                );
            }
        }
    }
}

/// Walk a JSON value to the location named by `path`, returning `&Value::Null`
/// for any missing segment so callers can chain without `Option`.
fn get_value_at_path<'a>(value: &'a Value, path: &Path) -> &'a Value {
    path.iter()
        .fold(value, |current, segment| match (segment, current) {
            (PathElement::Key(k, _), Value::Object(obj)) => {
                obj.get(k.as_str()).unwrap_or(&Value::Null)
            }
            (PathElement::Index(i), Value::Array(arr)) => arr.get(*i).unwrap_or(&Value::Null),
            _ => &Value::Null,
        })
}

fn typename_of(value: &Value) -> Option<&str> {
    match value {
        Value::Object(obj) => obj.get(TYPENAME).and_then(|v| v.as_str()),
        _ => None,
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Skip tree
// ──────────────────────────────────────────────────────────────────────────

/// Unified tree mirroring response navigation, with skipped-fetch coverage
/// folded in. Built once at end-of-execution from all `(entity_path, tree)`
/// entries; consulted by the walker via a `SkipState` cursor that advances
/// in lockstep with the response-data walk. Coverage check is O(1) per leaf.
///
/// Two kinds of navigation: by response key (composite descent) and by array
/// index (list element descent). A node with no `by_key`/`by_index` entries
/// is a leaf-level skip target — the skipped fetch ended here.
#[derive(Default, Debug)]
struct SkipTreeNode {
    by_key: HashMap<String, SkipTreeNode>,
    by_index: HashMap<usize, SkipTreeNode>,
}

impl SkipTreeNode {
    fn build(skipped: &[(Path, Arc<ResponseKeyTree>)]) -> Self {
        let mut root = SkipTreeNode::default();
        for (entity_path, tree) in skipped {
            // Navigate to the entity_path, creating intermediate nodes.
            let mut cursor = &mut root;
            for segment in entity_path.iter() {
                match segment {
                    PathElement::Key(k, _) => {
                        cursor = cursor.by_key.entry(k.clone()).or_default();
                    }
                    PathElement::Index(i) => {
                        cursor = cursor.by_index.entry(*i).or_default();
                    }
                    // Flatten/Fragment shouldn't appear in entity paths recorded by
                    // `Variables::new`; if they do, skip gracefully.
                    _ => return root,
                }
            }
            graft_response_key_tree(cursor, tree);
        }
        root
    }
}

/// Copy a `ResponseKeyTree`'s structure into a `SkipTreeNode`. A leaf in the
/// source tree (no children) maps to a leaf in the target (no `by_key`/
/// `by_index` entries).
fn graft_response_key_tree(target: &mut SkipTreeNode, tree: &ResponseKeyTree) {
    for (key, child) in &tree.children {
        let target_child = target.by_key.entry(key.as_str().to_owned()).or_default();
        graft_response_key_tree(target_child, child);
    }
}

/// Cursor into a `SkipTreeNode`, advanced in lockstep with the response-data walk.
#[derive(Copy, Clone)]
enum SkipState<'a> {
    None,
    At(&'a SkipTreeNode),
}

impl<'a> SkipState<'a> {
    fn root(tree: &'a SkipTreeNode) -> Self {
        Self::At(tree)
    }

    fn descend_key(self, key: &str) -> Self {
        match self {
            Self::At(t) => match t.by_key.get(key) {
                Some(child) => Self::At(child),
                None => Self::None,
            },
            Self::None => Self::None,
        }
    }

    /// Walk one step into a list element.
    /// - No per-index entries → pass through (`by_key` applies uniformly to
    ///   every element).
    /// - `by_index` has this index → descend into its per-entity subtree.
    /// - `by_index` has entries but not this one → the index is not covered.
    fn descend_index(self, i: usize) -> Self {
        match self {
            Self::At(t) => {
                if t.by_index.is_empty() {
                    Self::At(t)
                } else if let Some(child) = t.by_index.get(&i) {
                    Self::At(child)
                } else {
                    Self::None
                }
            }
            Self::None => Self::None,
        }
    }

    /// True iff this state corresponds to any node in the master tree (leaf
    /// or intermediate). The walker emits at a missing/null position whenever
    /// the position is in the skip tree at all.
    fn is_present(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// End-of-execution entry point: fold the per-entity `(Path, ResponseKeyTree)` entries into a
/// single `SkipTreeNode`, then walk the user's operation against `value` and append
/// `UNSATISFIED_FETCH_CONDITION` errors to `errors`. No-op when `skipped` is empty so callers can
/// invoke unconditionally.
fn emit_unsatisfied_fetch_errors(
    skipped: &[(Path, Arc<ResponseKeyTree>)],
    query: &Query,
    schema: &Schema,
    variables: &Object,
    value: &Value,
    errors: &mut Vec<Error>,
) {
    if skipped.is_empty() {
        return;
    }
    let master = SkipTreeNode::build(skipped);
    collect_unsatisfied_fetch_errors(
        &query.operation.selection_set,
        query.operation.kind().default_type_name(),
        &query.fragments,
        schema,
        variables,
        value,
        &Path::empty(),
        SkipState::root(&master),
        errors,
    );
}

/// True iff the user's selection set at this composite contains a non-null
/// field that the skipped fetch was going to provide and that is missing from
/// the response data. When this is true, `format_response` will null-bubble
/// from that leaf up to this composite, so the walker coalesces per-leaf
/// emissions into a single error at the composite path.
///
/// Recurses through inline fragments and fragment spreads, honoring
/// `@skip`/`@include` and applying `static_type_condition_match` strictly:
/// only definite matches are descended into.
fn composite_has_nonnull_miss(
    selection_set: &[Selection],
    current_type: &str,
    fragments: &Fragments,
    schema: &Schema,
    variables: &Object,
    value: &Value,
    skip: SkipState<'_>,
) -> bool {
    for selection in selection_set {
        match selection {
            Selection::Field {
                name,
                alias,
                field_type,
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if name.as_str() == TYPENAME {
                    continue;
                }
                if !field_type.is_non_null() {
                    continue;
                }
                let response_key = alias.as_ref().unwrap_or(name).as_str();
                let field_value = value.as_object().and_then(|o| o.get(response_key));
                let missing = match field_value {
                    None => true,
                    Some(v) => v.is_null(),
                };
                if !missing {
                    continue;
                }
                if skip.descend_key(response_key).is_present() {
                    return true;
                }
            }
            Selection::InlineFragment {
                type_condition,
                selection_set: sub,
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if static_type_condition_match(schema, current_type, type_condition) == Some(true) {
                    let refined = walker_refine_type(schema, current_type, type_condition.as_str());
                    if composite_has_nonnull_miss(
                        sub, refined, fragments, schema, variables, value, skip,
                    ) {
                        return true;
                    }
                }
            }
            Selection::FragmentSpread {
                name, include_skip, ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                let Some(fragment) = fragments.get(name) else {
                    continue;
                };
                if static_type_condition_match(schema, current_type, &fragment.type_condition)
                    == Some(true)
                {
                    let refined =
                        walker_refine_type(schema, current_type, &fragment.type_condition);
                    if composite_has_nonnull_miss(
                        &fragment.selection_set,
                        refined,
                        fragments,
                        schema,
                        variables,
                        value,
                        skip,
                    ) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// End-of-execution walker: traverse the user's selection set against the
/// final merged response data and emit `UNSATISFIED_FETCH_CONDITION` for each
/// missing field whose `SkipState` cursor reports coverage.
///
/// Walking the master skip tree in lockstep with the response walk turns the
/// per-leaf coverage check into O(1) — one HashMap lookup per descend step.
#[allow(clippy::too_many_arguments)]
fn collect_unsatisfied_fetch_errors(
    selection_set: &[Selection],
    parent_type: &str,
    fragments: &Fragments,
    schema: &Schema,
    variables: &Object,
    value: &Value,
    current_path: &Path,
    skip: SkipState<'_>,
    output: &mut Vec<Error>,
) {
    let current_type = value
        .as_object()
        .and_then(|o| o.get(TYPENAME))
        .and_then(|v| v.as_str())
        .unwrap_or(parent_type);

    // Pre-scan: if a non-null missing field below this composite is covered
    // by the skip tree, format_response will null-bubble to here. Emit one
    // error at this composite instead of descending and emitting per-leaf.
    if composite_has_nonnull_miss(
        selection_set,
        current_type,
        fragments,
        schema,
        variables,
        value,
        skip,
    ) {
        output.push(
            Error::builder()
                .message("Could not fetch field")
                .path(current_path.clone())
                .extension_code("UNSATISFIED_FETCH_CONDITION")
                .build(),
        );
        return;
    }

    for selection in selection_set {
        match selection {
            Selection::Field {
                name,
                alias,
                selection_set: sub,
                include_skip,
                field_type,
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

                let missing = match field_value {
                    None => true,
                    Some(v) => v.is_null(),
                };

                let field_skip = skip.descend_key(response_key);

                if missing {
                    // Field is null/missing AND in the skip tree → emit here.
                    if field_skip.is_present() {
                        let mut field_path = current_path.clone();
                        field_path.push(PathElement::Key(response_key.to_string(), None));
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

                if let Some(sub_sel) = sub
                    && let Some(field_value) = field_value
                {
                    let mut field_path = current_path.clone();
                    field_path.push(PathElement::Key(response_key.to_string(), None));
                    descend_into_value(
                        sub_sel,
                        field_type.inner_named_type().as_str(),
                        fragments,
                        schema,
                        variables,
                        field_value,
                        &field_path,
                        field_skip,
                        output,
                    );
                }
            }
            Selection::InlineFragment {
                type_condition,
                selection_set: sub,
                include_skip,
                ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                if static_type_condition_match(schema, current_type, type_condition) == Some(true) {
                    let refined = walker_refine_type(schema, current_type, type_condition.as_str());
                    collect_unsatisfied_fetch_errors(
                        sub,
                        refined,
                        fragments,
                        schema,
                        variables,
                        value,
                        current_path,
                        skip,
                        output,
                    );
                }
            }
            Selection::FragmentSpread {
                name, include_skip, ..
            } => {
                if include_skip.should_skip(variables) {
                    continue;
                }
                let Some(fragment) = fragments.get(name) else {
                    continue;
                };
                if static_type_condition_match(schema, current_type, &fragment.type_condition)
                    == Some(true)
                {
                    let refined =
                        walker_refine_type(schema, current_type, &fragment.type_condition);
                    collect_unsatisfied_fetch_errors(
                        &fragment.selection_set,
                        refined,
                        fragments,
                        schema,
                        variables,
                        value,
                        current_path,
                        skip,
                        output,
                    );
                }
            }
        }
    }
}

/// Step into a response value carrying a composite selection set: iterate
/// array elements one at a time (advancing the skip-tree index cursor), or
/// dispatch the non-array case to `collect_unsatisfied_fetch_errors`.
#[allow(clippy::too_many_arguments)]
fn descend_into_value(
    selection_set: &[Selection],
    parent_type: &str,
    fragments: &Fragments,
    schema: &Schema,
    variables: &Object,
    value: &Value,
    current_path: &Path,
    skip: SkipState<'_>,
    output: &mut Vec<Error>,
) {
    if let Some(arr) = value.as_array() {
        for (i, item) in arr.iter().enumerate() {
            let mut item_path = current_path.clone();
            item_path.push(PathElement::Index(i));
            descend_into_value(
                selection_set,
                parent_type,
                fragments,
                schema,
                variables,
                item,
                &item_path,
                skip.descend_index(i),
                output,
            );
        }
    } else {
        collect_unsatisfied_fetch_errors(
            selection_set,
            parent_type,
            fragments,
            schema,
            variables,
            value,
            current_path,
            skip,
            output,
        );
    }
}
