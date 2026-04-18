use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::executable;
use futures::StreamExt;
use futures::future::join_all;
use futures::prelude::*;
use indexmap::IndexMap;
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

                        // Generate errors for skipped entities due to missing requirements.
                        let mut skipped_entity_errors = Vec::new();
                        for unsatisfied_path in &unsatisfied_paths {
                            skipped_entity_errors.extend(errors_for_skipped_entity::generate(
                                parameters,
                                fetch_node,
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
                                errors.extend(skipped_entity_errors);

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
                                errors = skipped_entity_errors;
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

mod errors_for_skipped_entity {
    use super::*;

    /// Generate errors for all leaf selections from the given entity fetch because the entity was
    /// skipped (for example, a non-nullable `@requires` input field was missing).
    ///
    /// Reports leaf-path errors for fields that are both (a) fetched by the subgraph query and (b)
    /// selected in the original input query at `current_dir`.
    /// Note: Reporting at leaves (rather than top-level response keys) matters for `@shareable`
    /// composite fields: if a parallel subgraph also resolves the shared parent, only the leaves
    /// the skipped fetch alone would have supplied should be flagged.
    pub(super) fn generate(
        parameters: &ExecutionParameters<'_>,
        fetch_node: &FetchNode,
        current_dir: &Path,
        parent_value: &Value,
    ) -> Vec<Error> {
        let entity_data = get_value_at_path(parent_value, current_dir);
        let entity_type = typename_of(entity_data);
        let ctx = FilterContext {
            schema: parameters.schema,
            variables: &parameters.supergraph_request.body().variables,
            fragments: &parameters.query.fragments,
        };

        // Step 1: build a tree of response keys the skipped fetch would have
        // produced for this entity. Recursing into composite fields preserves
        // depth so we can report at the leaf level.
        let Some(fetched_tree) =
            entity_response_key_tree(fetch_node, entity_type, ctx.schema, ctx.variables)
        else {
            return Vec::new();
        };

        // Step 2: intersect that tree with the input query at `current_dir`,
        // preserving structure so composite branches survive only when both
        // sides have matching sub-selections.
        let filtered_tree = filter_response_key_tree_at_path(
            &ctx,
            &fetched_tree,
            &parameters.query.operation.selection_set,
            &current_dir.0,
            parent_value,
        );

        // Step 3: walk the intersected tree alongside `entity_data`, emitting an
        // error for each leaf whose value isn't already populated on the entity.
        enumerate_leaf_paths(&filtered_tree, entity_data)
            .into_iter()
            .map(|segments| {
                let mut path = current_dir.clone();
                for segment in segments {
                    path.push(PathElement::Key(segment, None));
                }
                Error::builder()
                    .message("Could not fetch field")
                    .path(path)
                    .extension_code("UNSATISFIED_FETCH_CONDITION")
                    .build()
            })
            .collect()
    }

    /// Context shared by the helpers that walk the input query to intersect it
    /// with a fetched-response-key tree.
    struct FilterContext<'a> {
        schema: &'a Schema,
        variables: &'a Object,
        fragments: &'a Fragments,
    }

    /// Tree of response keys produced by a subgraph fetch. An entry with an
    /// empty `children` map is a leaf (scalar/enum field, or a composite pruned
    /// to empty during intersection); a non-empty map is a composite branch.
    /// `is_list_stop` marks leaves where we stopped descent because the field is
    /// a list — such leaves must emit an error even if another fetch has
    /// populated the array, since we can't report per-item.
    #[derive(Default, Debug)]
    struct ResponseKeyTree {
        is_list_stop: bool,
        children: IndexMap<Name, ResponseKeyTree>,
    }

    impl ResponseKeyTree {
        fn is_empty(&self) -> bool {
            self.children.is_empty() && !self.is_list_stop
        }
    }

    /// Build a tree of response keys for a subgraph `_entities` fetch, filtered
    /// to fragments matching `entity_type` and not excluded by `@skip`/`@include`.
    ///
    /// Returns `None` when the operation isn't an `_entities` fetch or yields no
    /// matching fields — callers treat that as "nothing to report".
    fn entity_response_key_tree(
        fetch_node: &FetchNode,
        entity_type: Option<&str>,
        schema: &Schema,
        variables: &Object,
    ) -> Option<ResponseKeyTree> {
        let parsed = fetch_node.operation.as_parsed().ok()?;
        let operation = parsed
            .operations
            .anonymous
            .as_ref()
            .or_else(|| parsed.operations.named.values().next())?;

        let mut tree = ResponseKeyTree::default();
        for sel in &operation.selection_set.selections {
            if let executable::Selection::Field(f) = sel
                && f.name.as_str() == "_entities"
            {
                collect_response_key_tree(
                    &f.selection_set,
                    parsed,
                    entity_type,
                    schema,
                    variables,
                    &mut tree,
                );
            }
        }
        if tree.is_empty() { None } else { Some(tree) }
    }

    /// Recursively populate `tree` from a subgraph selection set. Descends through
    /// inline fragments and resolves named fragment spreads from `document`,
    /// filtering by `entity_type` against each fragment's type condition. When
    /// recursing into a composite field, `entity_type` resets to `None` since we
    /// don't track field return types here — any fragment nested below that point
    /// with an explicit type condition is then rejected. Skips `__typename`.
    fn collect_response_key_tree(
        selection_set: &executable::SelectionSet,
        document: &executable::ExecutableDocument,
        entity_type: Option<&str>,
        schema: &Schema,
        variables: &Object,
        tree: &mut ResponseKeyTree,
    ) {
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
                    // Stop at list-typed fields: the tree has no array
                    // indices, so we can't address per-item leaves and must
                    // report at the list field itself. Mark the leaf so
                    // Step 3 emits unconditionally.
                    if f.ty().is_list() {
                        entry.is_list_stop = true;
                    } else if !f.selection_set.selections.is_empty() {
                        collect_response_key_tree(
                            &f.selection_set,
                            document,
                            None,
                            schema,
                            variables,
                            entry,
                        );
                    }
                }
                executable::Selection::InlineFragment(frag) => {
                    if IncludeSkip::parse(&frag.directives).should_skip(variables) {
                        continue;
                    }
                    // Inline fragment without a type condition inherits the
                    // parent type — always accept.
                    let matches = match &frag.type_condition {
                        None => true,
                        Some(cond) => type_condition_matches(schema, entity_type, cond.as_str()),
                    };
                    if matches {
                        collect_response_key_tree(
                            &frag.selection_set,
                            document,
                            entity_type,
                            schema,
                            variables,
                            tree,
                        );
                    }
                }
                executable::Selection::FragmentSpread(spread) => {
                    if IncludeSkip::parse(&spread.directives).should_skip(variables) {
                        continue;
                    }
                    if let Some(fragment) = document.fragments.get(&spread.fragment_name)
                        && type_condition_matches(
                            schema,
                            entity_type,
                            fragment.type_condition().as_str(),
                        )
                    {
                        collect_response_key_tree(
                            &fragment.selection_set,
                            document,
                            entity_type,
                            schema,
                            variables,
                            tree,
                        );
                    }
                }
            }
        }
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

    /// Walk the input query once and return the subtree of `fetched_tree` whose
    /// fields are also selected in the input query at `remaining_dir`. The result
    /// preserves `fetched_tree`'s shape, pruned to the intersection. Handles path
    /// navigation, inline fragments, named fragment spreads, and
    /// `@skip`/`@include`.
    fn filter_response_key_tree_at_path(
        ctx: &FilterContext<'_>,
        fetched_tree: &ResponseKeyTree,
        selection_set: &[Selection],
        remaining_dir: &[PathElement],
        current_value: &Value,
    ) -> ResponseKeyTree {
        let mut result = ResponseKeyTree::default();
        if fetched_tree.is_empty() {
            return result;
        }
        descend_tree_to_selection_set(
            ctx,
            fetched_tree,
            selection_set,
            remaining_dir,
            current_value,
            &mut result,
        );
        result
    }

    /// Navigate `remaining_dir` through the input query to reach the selection set
    /// at the target path, then intersect fetched_tree against selections at that
    /// level.
    fn descend_tree_to_selection_set(
        ctx: &FilterContext<'_>,
        fetched_tree: &ResponseKeyTree,
        selection_set: &[Selection],
        remaining_dir: &[PathElement],
        current_value: &Value,
        result: &mut ResponseKeyTree,
    ) {
        let current_type = typename_of(current_value);
        if let Some((segment, rest)) = remaining_dir.split_first() {
            match segment {
                PathElement::Key(k, _) => {
                    let key = k.as_str();
                    let child_value = match current_value {
                        Value::Object(obj) => obj.get(key).unwrap_or(&Value::Null),
                        _ => &Value::Null,
                    };
                    descend_tree_into_key(
                        ctx,
                        fetched_tree,
                        selection_set,
                        key,
                        rest,
                        child_value,
                        current_type,
                        result,
                    );
                }
                PathElement::Index(i) => {
                    // Array index doesn't correspond to a selection set field —
                    // navigate into the array element and keep the selection set.
                    let child_value = match current_value {
                        Value::Array(arr) => arr.get(*i).unwrap_or(&Value::Null),
                        _ => &Value::Null,
                    };
                    descend_tree_to_selection_set(
                        ctx,
                        fetched_tree,
                        selection_set,
                        rest,
                        child_value,
                        result,
                    );
                }
                _ => {}
            }
        } else {
            intersect_tree_with_selections(ctx, fetched_tree, selection_set, current_type, result);
        }
    }

    /// Recurse into `selection_set` to find a field whose response key is `key`,
    /// then continue navigating `remaining_dir` inside its sub-selection. Descends
    /// through inline fragments and named fragment spreads whose type condition
    /// matches `current_type`.
    #[allow(clippy::too_many_arguments)]
    fn descend_tree_into_key(
        ctx: &FilterContext<'_>,
        fetched_tree: &ResponseKeyTree,
        selection_set: &[Selection],
        key: &str,
        remaining_dir: &[PathElement],
        child_value: &Value,
        current_type: Option<&str>,
        result: &mut ResponseKeyTree,
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
                    if include_skip.should_skip(ctx.variables) {
                        continue;
                    }
                    let field_name = alias.as_ref().unwrap_or(name);
                    if field_name.as_str() == key {
                        descend_tree_to_selection_set(
                            ctx,
                            fetched_tree,
                            inner,
                            remaining_dir,
                            child_value,
                            result,
                        );
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    include_skip,
                    ..
                } => {
                    if include_skip.should_skip(ctx.variables) {
                        continue;
                    }
                    if type_condition_matches(ctx.schema, current_type, type_condition) {
                        descend_tree_into_key(
                            ctx,
                            fetched_tree,
                            selection_set,
                            key,
                            remaining_dir,
                            child_value,
                            current_type,
                            result,
                        );
                    }
                }
                Selection::FragmentSpread {
                    name, include_skip, ..
                } => {
                    if include_skip.should_skip(ctx.variables) {
                        continue;
                    }
                    if let Some(Fragment {
                        type_condition,
                        selection_set,
                    }) = ctx.fragments.get(name)
                        && type_condition_matches(ctx.schema, current_type, type_condition)
                    {
                        descend_tree_into_key(
                            ctx,
                            fetched_tree,
                            selection_set,
                            key,
                            remaining_dir,
                            child_value,
                            current_type,
                            result,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Walk `selection_set` and build `result` as the intersection against
    /// `fetched_tree`. For each field whose response key is in `fetched_tree`:
    /// if fetched is a leaf, add a leaf; if fetched is composite and the input
    /// has a sub-selection, recurse and attach the non-empty subtree. Descends
    /// through inline fragments and named fragment spreads whose type condition
    /// matches `entity_type`; when recursing into a composite field, `entity_type`
    /// resets to `None` since we don't track field return types here.
    fn intersect_tree_with_selections(
        ctx: &FilterContext<'_>,
        fetched_tree: &ResponseKeyTree,
        selection_set: &[Selection],
        entity_type: Option<&str>,
        result: &mut ResponseKeyTree,
    ) {
        for sel in selection_set {
            match sel {
                Selection::Field {
                    name,
                    alias,
                    selection_set: sub,
                    include_skip,
                    ..
                } => {
                    if include_skip.should_skip(ctx.variables) {
                        continue;
                    }
                    if name.as_str() == TYPENAME {
                        continue;
                    }
                    let response_key = Name::new_unchecked(alias.as_ref().unwrap_or(name).as_str());
                    let Some(fetched_sub) = fetched_tree.children.get(&response_key) else {
                        continue;
                    };
                    if fetched_sub.children.is_empty() {
                        // Leaf on the fetched side → leaf in result. Carry
                        // `is_list_stop` over so unconditional emission is
                        // preserved.
                        let entry = result.children.entry(response_key).or_default();
                        if fetched_sub.is_list_stop {
                            entry.is_list_stop = true;
                        }
                    } else if let Some(input_sub) = sub {
                        // Composite on both sides → recurse; only keep non-empty.
                        let mut subresult = ResponseKeyTree::default();
                        intersect_tree_with_selections(
                            ctx,
                            fetched_sub,
                            input_sub,
                            None,
                            &mut subresult,
                        );
                        if !subresult.is_empty() {
                            result.children.insert(response_key, subresult);
                        }
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    include_skip,
                    ..
                } => {
                    if include_skip.should_skip(ctx.variables) {
                        continue;
                    }
                    if type_condition_matches(ctx.schema, entity_type, type_condition) {
                        intersect_tree_with_selections(
                            ctx,
                            fetched_tree,
                            selection_set,
                            entity_type,
                            result,
                        );
                    }
                }
                Selection::FragmentSpread {
                    name, include_skip, ..
                } => {
                    if include_skip.should_skip(ctx.variables) {
                        continue;
                    }
                    if let Some(Fragment {
                        type_condition,
                        selection_set,
                    }) = ctx.fragments.get(name)
                        && type_condition_matches(ctx.schema, entity_type, type_condition)
                    {
                        intersect_tree_with_selections(
                            ctx,
                            fetched_tree,
                            selection_set,
                            entity_type,
                            result,
                        );
                    }
                }
            }
        }
    }

    /// Enumerate root-to-leaf paths in `tree`, traversing `entity_data` in step
    /// so a leaf is emitted only when its value isn't already populated on the
    /// entity. A leaf is considered populated when its key is present on the
    /// corresponding object (including when the value is null) — that key has
    /// been written by some other (successful) fetch. List-stop leaves are the
    /// exception: they emit unconditionally, since another fetch may have
    /// written the array but can't have filled in the per-item fields this
    /// fetch would have contributed.
    fn enumerate_leaf_paths(tree: &ResponseKeyTree, entity_data: &Value) -> Vec<Vec<String>> {
        let mut paths = Vec::new();
        let mut current = Vec::new();
        collect_leaf_paths(tree, entity_data, &mut current, &mut paths);
        paths
    }

    fn collect_leaf_paths(
        tree: &ResponseKeyTree,
        data: &Value,
        current: &mut Vec<String>,
        paths: &mut Vec<Vec<String>>,
    ) {
        for (key, subtree) in &tree.children {
            current.push(key.as_str().to_owned());
            if subtree.children.is_empty() {
                // List-stop leaves emit unconditionally: another fetch may
                // have populated the array, but we can't address per-item
                // leaves, so the miss must be reported at the list field.
                // This only arises with shareable list fields that are
                // partially fetched across subgraphs — a rare case where we
                // can't pinpoint per-item paths, so we report at the list
                // field itself as a best effort.
                let should_emit = if subtree.is_list_stop {
                    true
                } else {
                    let populated = match data {
                        Value::Object(obj) => obj.contains_key(key.as_str()),
                        _ => false,
                    };
                    !populated
                };
                if should_emit {
                    paths.push(current.clone());
                }
            } else {
                let child_data = match data {
                    Value::Object(obj) => obj.get(key.as_str()).unwrap_or(&Value::Null),
                    _ => &Value::Null,
                };
                collect_leaf_paths(subtree, child_data, current, paths);
            }
            current.pop();
        }
    }
}
