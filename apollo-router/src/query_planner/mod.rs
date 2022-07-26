use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

pub(crate) use bridge_query_planner::*;
pub(crate) use caching_query_planner::*;
pub use fetch::OperationKind;
use futures::future::join_all;
use futures::prelude::*;
use opentelemetry::trace::SpanKind;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use tokio::sync::broadcast::Sender;
use tokio_stream::wrappers::BroadcastStream;
use tracing::Instrument;

use crate::error::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::services::subgraph_service::SubgraphServiceFactory;
use crate::*;

mod bridge_query_planner;
mod caching_query_planner;
mod selection;

/// Query planning options.
#[derive(Clone, Eq, Hash, PartialEq, Debug, Default)]
pub struct QueryPlanOptions {
    /// Enable the variable deduplication optimization on the QueryPlan
    pub enable_variable_deduplication: bool,
}

/// A plan for a given GraphQL query
#[derive(Debug)]
pub struct QueryPlan {
    pub usage_reporting: UsageReporting,
    pub(crate) root: PlanNode,
    options: QueryPlanOptions,
}

/// This default impl is useful for test users
/// who will need `QueryPlan`s to work with the `QueryPlannerService` and the `ExecutionService`
#[buildstructor::buildstructor]
impl QueryPlan {
    #[builder]
    pub(crate) fn fake_new(
        root: Option<PlanNode>,
        usage_reporting: Option<UsageReporting>,
    ) -> Self {
        Self {
            usage_reporting: usage_reporting.unwrap_or_else(|| UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            }),
            root: root.unwrap_or_else(|| PlanNode::Sequence { nodes: Vec::new() }),
            options: QueryPlanOptions::default(),
        }
    }
}

/// Query plans are composed of a set of nodes.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub(crate) enum PlanNode {
    /// These nodes must be executed in order.
    Sequence {
        /// The plan nodes that make up the sequence execution.
        nodes: Vec<PlanNode>,
    },

    /// These nodes may be executed in parallel.
    Parallel {
        /// The plan nodes that make up the parallel execution.
        nodes: Vec<PlanNode>,
    },

    /// Fetch some data from a subgraph.
    Fetch(fetch::FetchNode),

    /// Merge the current resultset with the response.
    Flatten(FlattenNode),

    Defer {
        primary: Primary,
        deferred: Vec<DeferredNode>,
    },

    #[serde(rename_all = "camelCase")]
    Condition {
        condition: String,
        if_clause: Option<Box<PlanNode>>,
        else_clause: Option<Box<PlanNode>>,
    },
}

impl PlanNode {
    pub(crate) fn contains_mutations(&self) -> bool {
        match self {
            Self::Sequence { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Parallel { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Fetch(fetch_node) => fetch_node.operation_kind() == &OperationKind::Mutation,
            Self::Defer { primary, .. } => primary
                .node
                .as_ref()
                .map(|n| n.contains_mutations())
                .unwrap_or(false),
            Self::Flatten(_) => false,
            Self::Condition {
                if_clause,
                else_clause,
                ..
            } => {
                if let Some(node) = if_clause {
                    if node.contains_mutations() {
                        return true;
                    }
                }
                if let Some(node) = else_clause {
                    if node.contains_mutations() {
                        return true;
                    }
                }
                false
            }
        }
    }

    pub(crate) fn contains_defer(&self) -> bool {
        match self {
            Self::Sequence { nodes } => nodes.iter().any(|n| n.contains_defer()),
            Self::Parallel { nodes } => nodes.iter().any(|n| n.contains_defer()),
            Self::Flatten(node) => node.node.contains_defer(),
            Self::Fetch(..) => false,
            Self::Defer { .. } => true,
            Self::Condition {
                if_clause,
                else_clause,
                ..
            } => {
                // right now ConditionNode is only used with defer, but it might be used
                // in the future to implement @skip and @include execution
                if let Some(node) = if_clause {
                    if node.contains_defer() {
                        return true;
                    }
                }
                if let Some(node) = else_clause {
                    if node.contains_defer() {
                        return true;
                    }
                }
                false
            }
        }
    }

    pub(crate) fn parse_subselections(
        &self,
        schema: &Schema,
    ) -> HashMap<(Option<Path>, String), Query> {
        if !self.contains_defer() {
            return HashMap::new();
        }
        // re-create full query with the right path
        // parse the subselection
        let mut subselections = HashMap::new();
        self.collect_subselections(schema, &Path::default(), &mut subselections);

        subselections
    }

    fn collect_subselections(
        &self,
        schema: &Schema,
        initial_path: &Path,
        subselections: &mut HashMap<(Option<Path>, String), Query>,
    ) {
        // re-create full query with the right path
        // parse the subselection
        match self {
            Self::Sequence { nodes } | Self::Parallel { nodes } => {
                nodes.iter().fold(subselections, |subs, current| {
                    current.collect_subselections(schema, initial_path, subs);

                    subs
                });
            }
            Self::Flatten(node) => {
                node.node
                    .collect_subselections(schema, initial_path, subselections);
            }
            Self::Defer { primary, deferred } => {
                // TODO rebuilt subselection from the root thanks to the path
                let primary_path = initial_path.join(&primary.path.clone().unwrap_or_default());
                let query = reconstruct_full_query(&primary_path, &primary.subselection);
                // ----------------------- Parse ---------------------------------
                let sub_selection =
                    Query::parse(&query, schema).expect("it must respect the schema");
                // ----------------------- END Parse ---------------------------------

                subselections.insert(
                    (Some(primary_path), primary.subselection.clone()),
                    sub_selection,
                );
                deferred.iter().fold(subselections, |subs, current| {
                    if let Some(subselection) = &current.subselection {
                        // TODO rebuilt subselection from the root thanks to the path
                        let query = reconstruct_full_query(&current.path, subselection);
                        // ----------------------- Parse ---------------------------------
                        let sub_selection =
                            Query::parse(&query, schema).expect("it must respect the schema");
                        // ----------------------- END Parse ---------------------------------

                        subs.insert(
                            (current.path.clone().into(), subselection.clone()),
                            sub_selection,
                        );
                    }
                    if let Some(current_node) = &current.node {
                        current_node.collect_subselections(
                            schema,
                            &initial_path.join(&current.path),
                            subs,
                        );
                    }

                    subs
                });
            }
            Self::Fetch(..) => {}
            Self::Condition {
                if_clause,
                else_clause,
                ..
            } => {
                if let Some(node) = if_clause {
                    node.collect_subselections(schema, initial_path, subselections);
                }
                if let Some(node) = else_clause {
                    node.collect_subselections(schema, initial_path, subselections);
                }
            }
        }
    }
}

impl QueryPlan {
    /// Pass some options to the QueryPlan
    pub fn with_options(mut self, options: QueryPlanOptions) -> Self {
        self.options = options;
        self
    }

    /// Execute the plan and return a [`Response`].
    pub async fn execute<'a, SF>(
        &self,
        context: &'a Context,
        service_factory: &'a Arc<SF>,
        originating_request: &'a Arc<http_ext::Request<Request>>,
        schema: &'a Schema,
        sender: futures::channel::mpsc::Sender<Response>,
    ) -> Response
    where
        SF: SubgraphServiceFactory,
    {
        let root = Path::empty();

        log::trace_query_plan(&self.root);
        let (value, subselection, errors) = self
            .root
            .execute_recursively(
                &root,
                context,
                service_factory,
                schema,
                originating_request,
                &Value::default(),
                &HashMap::new(),
                sender,
                &self.options,
            )
            .await;

        Response::builder()
            .data(value)
            .and_subselection(subselection)
            .errors(errors)
            .build()
    }

    pub fn contains_mutations(&self) -> bool {
        self.root.contains_mutations()
    }
}

impl PlanNode {
    #[allow(clippy::too_many_arguments)]
    fn execute_recursively<'a, SF>(
        &'a self,
        current_dir: &'a Path,
        context: &'a Context,
        service_factory: &'a Arc<SF>,
        schema: &'a Schema,
        originating_request: &'a Arc<http_ext::Request<Request>>,
        parent_value: &'a Value,
        deferred_fetches: &'a HashMap<String, Sender<(Value, Vec<Error>)>>,
        sender: futures::channel::mpsc::Sender<Response>,
        options: &'a QueryPlanOptions,
    ) -> future::BoxFuture<(Value, Option<String>, Vec<Error>)>
    where
        SF: SubgraphServiceFactory,
    {
        Box::pin(async move {
            tracing::trace!("executing plan:\n{:#?}", self);
            let mut value;
            let mut errors;
            let mut subselection = None;

            match self {
                PlanNode::Sequence { nodes } => {
                    value = parent_value.clone();
                    errors = Vec::new();
                    let span = tracing::info_span!("sequence");
                    for node in nodes {
                        let (v, subselect, err) = node
                            .execute_recursively(
                                current_dir,
                                context,
                                service_factory,
                                schema,
                                originating_request,
                                &value,
                                deferred_fetches,
                                sender.clone(),
                                options,
                            )
                            .instrument(span.clone())
                            .in_current_span()
                            .await;
                        value.deep_merge(v);
                        errors.extend(err.into_iter());
                        subselection = subselect;
                    }
                }
                PlanNode::Parallel { nodes } => {
                    value = Value::default();
                    errors = Vec::new();

                    let span = tracing::info_span!("parallel");
                    let mut stream: stream::FuturesUnordered<_> = nodes
                        .iter()
                        .map(|plan| {
                            plan.execute_recursively(
                                current_dir,
                                context,
                                service_factory,
                                schema,
                                originating_request,
                                parent_value,
                                deferred_fetches,
                                sender.clone(),
                                options,
                            )
                            .instrument(span.clone())
                        })
                        .collect();

                    while let Some((v, _subselect, err)) = stream
                        .next()
                        .instrument(span.clone())
                        .in_current_span()
                        .await
                    {
                        value.deep_merge(v);
                        errors.extend(err.into_iter());
                    }
                }
                PlanNode::Flatten(FlattenNode { path, node }) => {
                    let (v, subselect, err) = node
                        .execute_recursively(
                            // this is the only command that actually changes the "current dir"
                            &current_dir.join(path),
                            context,
                            service_factory,
                            schema,
                            originating_request,
                            parent_value,
                            deferred_fetches,
                            sender,
                            options,
                        )
                        .instrument(tracing::trace_span!("flatten"))
                        .await;

                    value = v;
                    errors = err;
                    subselection = subselect;
                }
                PlanNode::Fetch(fetch_node) => {
                    match fetch_node
                        .fetch_node(
                            parent_value,
                            current_dir,
                            context,
                            service_factory,
                            originating_request,
                            schema,
                            deferred_fetches,
                            options,
                        )
                        .instrument(tracing::info_span!(
                            "fetch",
                            "otel.kind" = %SpanKind::Internal,
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
                            subselection: primary_subselection,
                            node,
                        },
                    deferred,
                } => {
                    let mut deferred_fetches: HashMap<String, Sender<(Value, Vec<Error>)>> =
                        HashMap::new();
                    let mut futures = Vec::new();

                    let (primary_sender, _) = tokio::sync::broadcast::channel::<Value>(1);

                    for deferred_node in deferred {
                        let mut deferred_receivers = Vec::new();

                        for d in deferred_node.depends.iter() {
                            match deferred_fetches.get(&d.id) {
                                None => {
                                    let (sender, receiver) = tokio::sync::broadcast::channel(1);
                                    deferred_fetches.insert(d.id.clone(), sender.clone());
                                    deferred_receivers
                                        .push(BroadcastStream::new(receiver).into_future());
                                }
                                Some(sender) => {
                                    let receiver = sender.subscribe();
                                    deferred_receivers
                                        .push(BroadcastStream::new(receiver).into_future());
                                }
                            }
                        }

                        // if a deferred node has no depends (ie not waiting for data from fetches) then it has to
                        // wait until the primary response is entirely created.
                        //
                        // If the depends list is not empty, the inner node can start working on the fetched data, then
                        // it is merged into the primary response before applying the subselection
                        let is_depends_empty = deferred_node.depends.is_empty();

                        let mut stream: stream::FuturesUnordered<_> =
                            deferred_receivers.into_iter().collect();
                        //FIXME/ is there a solution without cloning the entire node? Maybe it could be moved instead?
                        let deferred_inner = deferred_node.node.clone();
                        let deferred_path = deferred_node.path.clone();
                        let subselection = deferred_node.subselection();
                        let label = deferred_node.label.clone();
                        let mut tx = sender.clone();
                        let sc = schema.clone();
                        let orig = originating_request.clone();
                        let sf = service_factory.clone();
                        let ctx = context.clone();
                        let opt = options.clone();
                        let mut primary_receiver = primary_sender.subscribe();
                        let mut value = parent_value.clone();
                        let fut = async move {
                            let mut errors = Vec::new();

                            if is_depends_empty {
                                let primary_value =
                                    primary_receiver.recv().await.unwrap_or_default();
                                value.deep_merge(primary_value);
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

                            let span = tracing::info_span!("deferred");

                            if let Some(node) = deferred_inner {
                                let (mut v, node_subselection, err) = node
                                    .execute_recursively(
                                        &Path::default(),
                                        &ctx,
                                        &sf,
                                        &sc,
                                        &orig,
                                        &value,
                                        &HashMap::new(),
                                        tx.clone(),
                                        &opt,
                                    )
                                    .instrument(span.clone())
                                    .in_current_span()
                                    .await;

                                if !is_depends_empty {
                                    let primary_value =
                                        primary_receiver.recv().await.unwrap_or_default();
                                    v.deep_merge(primary_value);
                                }

                                if let Err(e) = tx
                                    .send(
                                        Response::builder()
                                            .data(v)
                                            .errors(err)
                                            .and_path(Some(deferred_path.clone()))
                                            .and_subselection(subselection.or(node_subselection))
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
                            } else {
                                let primary_value =
                                    primary_receiver.recv().await.unwrap_or_default();
                                value.deep_merge(primary_value);

                                if let Err(e) = tx
                                    .send(
                                        Response::builder()
                                            .data(value)
                                            .errors(errors)
                                            .and_path(Some(deferred_path.clone()))
                                            .and_subselection(subselection)
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
                            };
                        };

                        futures.push(fut);
                    }

                    tokio::task::spawn(
                        async move {
                            join_all(futures).await;
                        }
                        .in_current_span(),
                    );

                    value = parent_value.clone();
                    errors = Vec::new();
                    let span = tracing::info_span!("primary");
                    if let Some(node) = node {
                        let (v, _subselect, err) = node
                            .execute_recursively(
                                current_dir,
                                context,
                                service_factory,
                                schema,
                                originating_request,
                                &value,
                                &deferred_fetches,
                                sender,
                                options,
                            )
                            .instrument(span.clone())
                            .in_current_span()
                            .await;
                        let _guard = span.enter();
                        value.deep_merge(v);
                        errors.extend(err.into_iter());
                        subselection = primary_subselection.clone().into();

                        let _ = primary_sender.send(value.clone());
                    } else {
                        let _guard = span.enter();

                        subselection = primary_subselection.clone().into();

                        let _ = primary_sender.send(value.clone());
                    }
                }
                PlanNode::Condition {
                    condition,
                    if_clause,
                    else_clause,
                } => {
                    value = Value::default();
                    errors = Vec::new();

                    if let Some(&Value::Bool(true)) =
                        originating_request.body().variables.get(condition.as_str())
                    {
                        //FIXME: should we show an error if the if_node was not present?
                        if let Some(node) = if_clause {
                            let span = tracing::info_span!("condition_if");
                            let (v, subselect, err) = node
                                .execute_recursively(
                                    current_dir,
                                    context,
                                    service_factory,
                                    schema,
                                    originating_request,
                                    parent_value,
                                    deferred_fetches,
                                    sender.clone(),
                                    options,
                                )
                                .instrument(span.clone())
                                .in_current_span()
                                .await;
                            value.deep_merge(v);
                            errors.extend(err.into_iter());
                            subselection = subselect;
                        }
                    } else if let Some(node) = else_clause {
                        let span = tracing::info_span!("condition_else");
                        let (v, subselect, err) = node
                            .execute_recursively(
                                current_dir,
                                context,
                                service_factory,
                                schema,
                                &originating_request.clone(),
                                parent_value,
                                deferred_fetches,
                                sender.clone(),
                                options,
                            )
                            .instrument(span.clone())
                            .in_current_span()
                            .await;
                        value.deep_merge(v);
                        errors.extend(err.into_iter());
                        subselection = subselect;
                    }
                }
            }

            (value, subselection, errors)
        })
    }

    #[cfg(test)]
    /// Retrieves all the services used across all plan nodes.
    ///
    /// Note that duplicates are not filtered.
    fn service_usage<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match self {
            Self::Sequence { nodes } | Self::Parallel { nodes } => {
                Box::new(nodes.iter().flat_map(|x| x.service_usage()))
            }
            Self::Fetch(fetch) => Box::new(Some(fetch.service_name()).into_iter()),
            Self::Flatten(flatten) => flatten.node.service_usage(),
            Self::Defer { primary, deferred } => primary
                .node
                .as_ref()
                .map(|n| {
                    Box::new(
                        n.service_usage().chain(
                            deferred
                                .iter()
                                .flat_map(|d| d.node.iter().flat_map(|node| node.service_usage())),
                        ),
                    ) as Box<dyn Iterator<Item = &'a str> + 'a>
                })
                .unwrap_or_else(|| {
                    Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &'a str> + 'a>
                }),

            Self::Condition {
                if_clause,
                else_clause,
                ..
            } => match (if_clause, else_clause) {
                (None, None) => Box::new(None.into_iter()),
                (None, Some(node)) => node.service_usage(),
                (Some(node), None) => node.service_usage(),
                (Some(if_node), Some(else_node)) => {
                    Box::new(if_node.service_usage().chain(else_node.service_usage()))
                }
            },
        }
    }
}

fn reconstruct_full_query(path: &Path, subselection: &str) -> String {
    let mut query = String::new();
    let mut len = 0;
    for path_elt in path.iter() {
        match path_elt {
            json_ext::PathElement::Flatten | json_ext::PathElement::Index(_) => {}
            json_ext::PathElement::Key(key) => {
                write!(&mut query, "{{ {key}")
                    .expect("writing to a String should not fail because it can reallocate");
                len += 1;
            }
        }
    }

    query.push_str(subselection);
    query.push_str(&" }".repeat(len));

    query
}

pub(crate) mod fetch {
    use std::collections::HashMap;
    use std::fmt::Display;
    use std::sync::Arc;

    use indexmap::IndexSet;
    use serde::Deserialize;
    use tokio::sync::broadcast::Sender;
    use tower::ServiceExt;
    use tracing::instrument;
    use tracing::Instrument;

    use super::selection::select_object;
    use super::selection::Selection;
    use super::QueryPlanOptions;
    use crate::error::Error;
    use crate::error::FetchError;
    use crate::graphql::Request;
    use crate::json_ext::Object;
    use crate::json_ext::Path;
    use crate::json_ext::Value;
    use crate::json_ext::ValueExt;
    use crate::services::subgraph_service::SubgraphServiceFactory;
    use crate::*;

    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum OperationKind {
        Query,
        Mutation,
        Subscription,
    }

    impl Display for OperationKind {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                OperationKind::Query => write!(f, "Query"),
                OperationKind::Mutation => write!(f, "Mutation"),
                OperationKind::Subscription => write!(f, "Subscription"),
            }
        }
    }

    impl Default for OperationKind {
        fn default() -> Self {
            OperationKind::Query
        }
    }

    /// A fetch node.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct FetchNode {
        /// The name of the service or subgraph that the fetch is querying.
        pub(crate) service_name: String,

        /// The data that is required for the subgraph fetch.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        #[serde(default)]
        pub(crate) requires: Vec<Selection>,

        /// The variables that are used for the subgraph fetch.
        pub(crate) variable_usages: Vec<String>,

        /// The GraphQL subquery that is used for the fetch.
        pub(crate) operation: String,

        /// The GraphQL subquery operation name.
        pub(crate) operation_name: Option<String>,

        /// The GraphQL operation kind that is used for the fetch.
        pub(crate) operation_kind: OperationKind,

        /// Optional id used by Deferred nodes
        pub(crate) id: Option<String>,
    }

    struct Variables {
        variables: Object,
        paths: HashMap<Path, usize>,
    }

    impl Variables {
        #[instrument(skip_all, level = "debug", name = "make_variables")]
        async fn new(
            requires: &[Selection],
            variable_usages: &[String],
            data: &Value,
            current_dir: &Path,
            request: &Arc<http_ext::Request<Request>>,
            schema: &Schema,
            enable_variable_deduplication: bool,
        ) -> Option<Variables> {
            let body = request.body();
            if !requires.is_empty() {
                let mut variables = Object::with_capacity(1 + variable_usages.len());

                variables.extend(variable_usages.iter().filter_map(|key| {
                    body.variables
                        .get_key_value(key.as_str())
                        .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                }));

                let mut paths: HashMap<Path, usize> = HashMap::new();
                let (paths, representations) = if enable_variable_deduplication {
                    let mut values: IndexSet<Value> = IndexSet::new();
                    data.select_values_and_paths(current_dir, |path, value| {
                        if let Value::Object(content) = value {
                            if let Ok(Some(value)) = select_object(content, requires, schema) {
                                match values.get_index_of(&value) {
                                    Some(index) => {
                                        paths.insert(path.clone(), index);
                                    }
                                    None => {
                                        paths.insert(path.clone(), values.len());
                                        values.insert(value);
                                    }
                                }
                            }
                        }
                    });

                    if values.is_empty() {
                        return None;
                    }

                    (paths, Value::Array(Vec::from_iter(values)))
                } else {
                    let mut values: Vec<Value> = Vec::new();
                    data.select_values_and_paths(current_dir, |path, value| {
                        if let Value::Object(content) = value {
                            if let Ok(Some(value)) = select_object(content, requires, schema) {
                                paths.insert(path.clone(), values.len());
                                values.push(value);
                            }
                        }
                    });

                    if values.is_empty() {
                        return None;
                    }

                    (paths, Value::Array(Vec::from_iter(values)))
                };
                variables.insert("representations", representations);

                Some(Variables { variables, paths })
            } else {
                Some(Variables {
                    variables: variable_usages
                        .iter()
                        .filter_map(|key| {
                            body.variables
                                .get_key_value(key.as_str())
                                .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                        })
                        .collect::<Object>(),
                    paths: HashMap::new(),
                })
            }
        }
    }

    impl FetchNode {
        #[allow(clippy::too_many_arguments)]
        pub(crate) async fn fetch_node<'a, SF>(
            &'a self,
            data: &'a Value,
            current_dir: &'a Path,
            context: &'a Context,
            service_factory: &'a Arc<SF>,
            originating_request: &'a Arc<http_ext::Request<Request>>,
            schema: &'a Schema,
            deferred_fetches: &'a HashMap<String, Sender<(Value, Vec<Error>)>>,
            options: &QueryPlanOptions,
        ) -> Result<(Value, Vec<Error>), FetchError>
        where
            SF: SubgraphServiceFactory,
        {
            let FetchNode {
                operation,
                operation_kind,
                operation_name,
                service_name,
                ..
            } = self;

            let Variables { variables, paths } = match Variables::new(
                &self.requires,
                self.variable_usages.as_ref(),
                data,
                current_dir,
                // Needs the original request here
                originating_request,
                schema,
                options.enable_variable_deduplication,
            )
            .await
            {
                Some(variables) => variables,
                None => {
                    return Ok((Value::from_path(current_dir, Value::Null), Vec::new()));
                }
            };

            let subgraph_request = SubgraphRequest::builder()
                .originating_request(originating_request.clone())
                .subgraph_request(
                    http_ext::Request::builder()
                        .method(http::Method::POST)
                        .uri(
                            schema
                                .subgraphs()
                                .find_map(|(name, url)| (name == service_name).then(|| url))
                                .unwrap_or_else(|| {
                                    panic!(
                        "schema uri for subgraph '{}' should already have been checked",
                        service_name
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
                        .expect(
                            "it won't fail because the url is correct and already checked; qed",
                        ),
                )
                .operation_kind(*operation_kind)
                .context(context.clone())
                .build();

            let service = service_factory
                .new_service(service_name)
                .expect("we already checked that the service exists during planning; qed");

            // TODO not sure if we need a RouterReponse here as we don't do anything with it
            let (_parts, response) = http::Response::from(
                service
                    .oneshot(subgraph_request)
                    .instrument(tracing::trace_span!("subfetch_stream"))
                    .await
                    // TODO this is a problem since it restores details about failed service
                    // when errors have been redacted in the include_subgraph_errors module.
                    // Unfortunately, not easy to fix here, because at this point we don't
                    // know if we should be redacting errors for this subgraph...
                    .map_err(|e| FetchError::SubrequestHttpError {
                        service: service_name.to_string(),
                        reason: e.to_string(),
                    })?
                    .response,
            )
            .into_parts();

            super::log::trace_subfetch(service_name, operation, &variables, &response);

            if !response.is_primary() {
                return Err(FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_owned(),
                });
            }

            // fix error path and erase subgraph error messages (we cannot expose subgraph information
            // to the client)
            let errors: Vec<Error> = response
                .errors
                .into_iter()
                .map(|error| Error {
                    locations: error.locations,
                    path: error.path.map(|path| current_dir.join(path)),
                    message: error.message,
                    extensions: error.extensions,
                })
                .collect();

            match self.response_at_path(current_dir, paths, response.data.unwrap_or_default()) {
                Ok(value) => {
                    if let Some(id) = &self.id {
                        if let Some(sender) = deferred_fetches.get(id.as_str()) {
                            if let Err(e) = sender.clone().send((value.clone(), errors.clone())) {
                                tracing::error!("error sending fetch result at path {} and id {:?} for deferred response building: {}", current_dir, self.id, e);
                            }
                        }
                    }

                    Ok((value, errors))
                }
                Err(e) => Err(e),
            }
        }

        #[instrument(skip_all, level = "debug", name = "response_insert")]
        fn response_at_path<'a>(
            &'a self,
            current_dir: &'a Path,
            paths: HashMap<Path, usize>,
            data: Value,
        ) -> Result<Value, FetchError> {
            if !self.requires.is_empty() {
                // we have to nest conditions and do early returns here
                // because we need to take ownership of the inner value
                if let Value::Object(mut map) = data {
                    if let Some(entities) = map.remove("_entities") {
                        tracing::trace!("received entities: {:?}", &entities);

                        if let Value::Array(array) = entities {
                            let mut value = Value::default();

                            for (path, entity_idx) in paths {
                                value.insert(
                                    &path,
                                    array
                                        .get(entity_idx)
                                        .ok_or_else(|| FetchError::ExecutionInvalidContent {
                                            reason: "Received invalid content for key `_entities`!"
                                                .to_string(),
                                        })?
                                        .clone(),
                                )?;
                            }
                            return Ok(value);
                        } else {
                            return Err(FetchError::ExecutionInvalidContent {
                                reason: "Received invalid type for key `_entities`!".to_string(),
                            });
                        }
                    }
                }

                Err(FetchError::ExecutionInvalidContent {
                    reason: "Missing key `_entities`!".to_string(),
                })
            } else {
                Ok(Value::from_path(current_dir, data))
            }
        }

        #[cfg(test)]
        pub(crate) fn service_name(&self) -> &str {
            &self.service_name
        }

        pub(crate) fn operation_kind(&self) -> &OperationKind {
            &self.operation_kind
        }
    }
}

/// A flatten node.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlattenNode {
    /// The path when result should be merged.
    path: Path,

    /// The child execution plan.
    node: Box<PlanNode>,
}

/// A primary query for a Defer node, the non deferred part
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Primary {
    /// Optional path, set if and only if the defer node is a
    /// nested defer. If set, `subselection` starts at that `path`.
    path: Option<Path>,

    /// The part of the original query that "selects" the data to
    /// send in that primary response (once the plan in `node` completes).
    subselection: String,

    // The plan to get all the data for that primary part
    node: Option<Box<PlanNode>>,
}

/// The "deferred" parts of the defer (note that it's an array). Each
/// of those deferred elements will correspond to a different chunk of
/// the response to the client (after the initial non-deferred one that is).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeferredNode {
    /// References one or more fetch node(s) (by `id`) within
    /// `primary.node`. The plan of this deferred part should not
    /// be started before all those fetches returns.
    depends: Vec<Depends>,

    /// The optional defer label.
    label: Option<String>,
    /// Path to the @defer this correspond to. `subselection` start at that `path`.
    path: Path,
    /// The part of the original query that "selects" the data to send
    /// in that deferred response (once the plan in `node` completes).
    /// Will be set _unless_ `node` is a `DeferNode` itself.
    subselection: Option<String>,
    /// The plan to get all the data for that deferred part
    node: Option<Arc<PlanNode>>,
}

impl DeferredNode {
    fn subselection(&self) -> Option<String> {
        self.subselection.clone().or_else(|| {
            self.node.as_ref().and_then(|node| match node.as_ref() {
                PlanNode::Defer { primary, .. } => Some(primary.subselection.clone()),
                _ => None,
            })
        })
    }
}
/// A deferred node.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Depends {
    id: String,
    defer_label: Option<String>,
}

// The code resides in a separate submodule to allow writing a log filter activating it
// separately from the query planner logs, as follows:
// `router -s supergraph.graphql --log info,crate::query_planner::log=trace`
mod log {
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Map;
    use serde_json_bytes::Value;

    use crate::query_planner::PlanNode;

    pub(crate) fn trace_query_plan(plan: &PlanNode) {
        tracing::trace!("query plan\n{:?}", plan);
    }

    pub(crate) fn trace_subfetch(
        service_name: &str,
        operation: &str,
        variables: &Map<ByteString, Value>,
        response: &crate::graphql::Response,
    ) {
        tracing::trace!(
            "subgraph fetch to {}: operation = '{}', variables = {:?}, response:\n{}",
            service_name,
            operation,
            variables,
            serde_json::to_string_pretty(&response).unwrap()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use http::Method;
    use tower::ServiceBuilder;
    use tower::ServiceExt;

    use super::*;
    use crate::json_ext::PathElement;
    use crate::plugin::test::MockSubgraphFactory;
    use crate::query_planner::fetch::FetchNode;

    macro_rules! test_query_plan {
        () => {
            include_str!("testdata/query_plan.json")
        };
    }

    macro_rules! test_schema {
        () => {
            include_str!("testdata/schema.graphql")
        };
    }

    #[test]
    fn query_plan_from_json() {
        let query_plan: PlanNode = serde_json::from_str(test_query_plan!()).unwrap();
        insta::assert_debug_snapshot!(query_plan);
    }

    #[test]
    fn service_usage() {
        assert_eq!(
            serde_json::from_str::<PlanNode>(test_query_plan!())
                .unwrap()
                .service_usage()
                .collect::<Vec<_>>(),
            vec!["product", "books", "product", "books", "product"]
        );
    }

    /// This test panics in the product subgraph. HOWEVER, this does not result in a panic in the
    /// test, since the buffer() functionality in the tower stack "loses" the panic and we end up
    /// with a closed service.
    ///
    /// See: https://github.com/tower-rs/tower/issues/455
    ///
    /// The query planner reports the failed subgraph fetch as an error with a reason of "service
    /// closed", which is what this test expects.
    #[tokio::test]
    async fn mock_subgraph_service_withf_panics_should_be_reported_as_service_closed() {
        let query_plan: QueryPlan = QueryPlan {
            root: serde_json::from_str(test_query_plan!()).unwrap(),
            options: QueryPlanOptions::default(),
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
        };

        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service.expect_call().times(1).withf(|_| {
            panic!("this panic should be propagated to the test harness");
        });

        let (sender, _) = futures::channel::mpsc::channel(10);
        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([(
                "product".into(),
                ServiceBuilder::new()
                    .buffer(1)
                    .service(mock_products_service.build().boxed()),
            )]),
            plugins: Default::default(),
        });

        let result = query_plan
            .execute(
                &Context::new(),
                &sf,
                &Arc::new(
                    http_ext::Request::fake_builder()
                        .headers(Default::default())
                        .body(Default::default())
                        .build()
                        .expect("fake builds should always work; qed"),
                ),
                &Schema::from_str(test_schema!()).unwrap(),
                sender,
            )
            .await;
        assert_eq!(result.errors.len(), 1);
        let reason: String = serde_json_bytes::from_value(
            result.errors[0].extensions.get("reason").unwrap().clone(),
        )
        .unwrap();
        assert_eq!(reason, "service closed".to_string());
    }

    #[tokio::test]
    async fn fetch_includes_operation_name() {
        let query_plan: QueryPlan = QueryPlan {
            root: serde_json::from_str(test_query_plan!()).unwrap(),
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
            options: QueryPlanOptions::default(),
        };

        let succeeded: Arc<AtomicBool> = Default::default();
        let inner_succeeded = Arc::clone(&succeeded);

        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service
            .expect_call()
            .times(1)
            .withf(move |request| {
                let matches = request.subgraph_request.body().operation_name
                    == Some("topProducts_product_0".into());
                inner_succeeded.store(matches, Ordering::SeqCst);
                matches
            })
            .returning(|_| Ok(SubgraphResponse::fake_builder().build()));

        let (sender, _) = futures::channel::mpsc::channel(10);

        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([(
                "product".into(),
                ServiceBuilder::new()
                    .buffer(1)
                    .service(mock_products_service.build().boxed()),
            )]),
            plugins: Default::default(),
        });

        let _response = query_plan
            .execute(
                &Context::new(),
                &sf,
                &Arc::new(
                    http_ext::Request::fake_builder()
                        .headers(Default::default())
                        .body(Default::default())
                        .build()
                        .expect("fake builds should always work; qed"),
                ),
                &Schema::from_str(test_schema!()).unwrap(),
                sender,
            )
            .await;

        assert!(succeeded.load(Ordering::SeqCst), "incorrect operation name");
    }

    #[tokio::test]
    async fn fetch_makes_post_requests() {
        let query_plan: QueryPlan = QueryPlan {
            root: serde_json::from_str(test_query_plan!()).unwrap(),
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
            options: QueryPlanOptions::default(),
        };

        let succeeded: Arc<AtomicBool> = Default::default();
        let inner_succeeded = Arc::clone(&succeeded);

        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service
            .expect_call()
            .times(1)
            .withf(move |request| {
                let matches = request.subgraph_request.method() == Method::POST;
                inner_succeeded.store(matches, Ordering::SeqCst);
                matches
            })
            .returning(|_| Ok(SubgraphResponse::fake_builder().build()));
        let (sender, _) = futures::channel::mpsc::channel(10);

        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([(
                "product".into(),
                ServiceBuilder::new()
                    .buffer(1)
                    .service(mock_products_service.build().boxed()),
            )]),
            plugins: Default::default(),
        });

        let _response = query_plan
            .execute(
                &Context::new(),
                &sf,
                &Arc::new(
                    http_ext::Request::fake_builder()
                        .headers(Default::default())
                        .body(Default::default())
                        .build()
                        .expect("fake builds should always work; qed"),
                ),
                &Schema::from_str(test_schema!()).unwrap(),
                sender,
            )
            .await;

        assert!(
            succeeded.load(Ordering::SeqCst),
            "subgraph requests must be http post"
        );
    }

    #[tokio::test]
    async fn defer() {
        // plan for { t { x ... @defer { y } }}
        let query_plan: QueryPlan = QueryPlan {
            root: PlanNode::Defer {
                primary: Primary {
                    path: None,
                    subselection: "{ t { x } }".to_string(),
                    node: Some(Box::new(PlanNode::Fetch(FetchNode {
                        service_name: "X".to_string(),
                        requires: vec![],
                        variable_usages: vec![],
                        operation: "{ t { id __typename x } }".to_string(),
                        operation_name: Some("t".to_string()),
                        operation_kind: OperationKind::Query,
                        id: Some("fetch1".to_string()),
                    }))),
                },
                deferred: vec![DeferredNode {
                    depends: vec![Depends {
                        id: "fetch1".to_string(),
                        defer_label: None,
                    }],
                    label: None,
                    path: Path(vec![PathElement::Key("t".to_string())]),
                    subselection: Some("{ y }".to_string()),
                    node: Some(Arc::new(PlanNode::Flatten(FlattenNode {
                        path: Path(vec![PathElement::Key("t".to_string())]),
                        node: Box::new(PlanNode::Fetch(FetchNode {
                            service_name: "Y".to_string(),
                            requires: vec![query_planner::selection::Selection::InlineFragment(
                                query_planner::selection::InlineFragment {
                                    type_condition: Some("T".into()),
                                    selections: vec![
                                        query_planner::selection::Selection::Field(
                                            query_planner::selection::Field {
                                                alias: None,
                                                name: "id".into(),
                                                selections: None,
                                            },
                                        ),
                                        query_planner::selection::Selection::Field(
                                            query_planner::selection::Field {
                                                alias: None,
                                                name: "__typename".into(),
                                                selections: None,
                                            },
                                        ),
                                    ],
                                },
                            )],
                            variable_usages: vec![],
                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on T{y}}}".to_string(),
                            operation_name: None,
                            operation_kind: OperationKind::Query,
                            id: Some("fetch2".to_string()),
                        })),
                    }))),
                }],
            },
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
            options: QueryPlanOptions::default(),
        };

        let mut mock_x_service = plugin::test::MockSubgraphService::new();
        mock_x_service
            .expect_call()
            .times(1)
            .withf(move |_request| true)
            .returning(|_| {
                Ok(SubgraphResponse::fake_builder()
                    .data(serde_json::json! {{
                        "t": {"id": 1234,
                        "__typename": "T",
                         "x": "X"
                        }
                    }})
                    .build())
            });
        let mut mock_y_service = plugin::test::MockSubgraphService::new();
        mock_y_service
            .expect_call()
            .times(1)
            .withf(move |_request| true)
            .returning(|_| {
                Ok(SubgraphResponse::fake_builder()
                    .data(serde_json::json! {{
                        "_entities": [{"y": "Y", "__typename": "T"}]
                    }})
                    .build())
            });

        let (sender, mut receiver) = futures::channel::mpsc::channel(10);

        let schema = Schema::from_str(include_str!("testdata/defer_schema.graphql")).unwrap();
        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([
                (
                    "X".into(),
                    ServiceBuilder::new()
                        .buffer(1)
                        .service(mock_x_service.build().boxed()),
                ),
                (
                    "Y".into(),
                    ServiceBuilder::new()
                        .buffer(1)
                        .service(mock_y_service.build().boxed()),
                ),
            ]),
            plugins: Default::default(),
        });

        let response = query_plan
            .execute(
                &Context::new(),
                &sf,
                &Arc::new(
                    http_ext::Request::fake_builder()
                        .headers(Default::default())
                        .body(Default::default())
                        .build()
                        .expect("fake builds should always work; qed"),
                ),
                &schema,
                sender,
            )
            .await;

        // primary response
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"data":{"t":{"id":1234,"__typename":"T","x":"X"}}}"#
        );

        let response = receiver.next().await.unwrap();

        // deferred response
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            // the primary response appears there because the deferred response gets data from it
            // unneeded parts are removed in response formatting
            r#"{"data":{"t":{"y":"Y","__typename":"T","id":1234,"x":"X"}},"path":["t"]}"#
        );
    }
}
