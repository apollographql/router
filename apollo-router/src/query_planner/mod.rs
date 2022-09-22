//! GraphQL operation planning.

#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

pub(crate) use bridge_query_planner::*;
pub(crate) use caching_query_planner::*;
use futures::future::join_all;
use futures::prelude::*;
use opentelemetry::trace::SpanKind;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::broadcast::Sender;
use tokio_stream::wrappers::BroadcastStream;
use tracing::Instrument;

pub(crate) use self::fetch::OperationKind;
use crate::error::Error;
use crate::error::QueryPlannerError;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::services::subgraph_service::SubgraphServiceFactory;
use crate::*;

mod bridge_query_planner;
mod caching_query_planner;
mod selection;

pub(crate) const FETCH_SPAN_NAME: &str = "fetch";
pub(crate) const FLATTEN_SPAN_NAME: &str = "flatten";
pub(crate) const SEQUENCE_SPAN_NAME: &str = "sequence";
pub(crate) const PARALLEL_SPAN_NAME: &str = "parallel";

/// Query planning options.
#[derive(Clone, Eq, Hash, PartialEq, Debug, Default)]
pub(crate) struct QueryPlanOptions {
    /// Enable the variable deduplication optimization on the QueryPlan
    pub(crate) enable_deduplicate_variables: bool,
}
/// A planner key.
///
/// This type consists of a query string, an optional operation string and the
/// [`QueryPlanOptions`].
pub(crate) type QueryKey = (String, Option<String>);

/// A plan for a given GraphQL query
#[derive(Debug)]
pub struct QueryPlan {
    usage_reporting: UsageReporting,
    pub(crate) root: PlanNode,
    /// String representation of the query plan (not a json representation)
    pub(crate) formatted_query_plan: Option<String>,
    pub(crate) query: Arc<Query>,
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
            formatted_query_plan: Default::default(),
            query: Arc::new(Query::default()),
            options: QueryPlanOptions::default(),
        }
    }
}

impl QueryPlan {
    pub(crate) fn is_deferred(&self, operation: Option<&str>, variables: &Object) -> bool {
        self.root.is_deferred(operation, variables, &self.query)
    }
}

/// Query plans are composed of a set of nodes.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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

    pub(crate) fn contains_condition_or_defer(&self) -> bool {
        match self {
            Self::Sequence { nodes } => nodes.iter().any(|n| n.contains_condition_or_defer()),
            Self::Parallel { nodes } => nodes.iter().any(|n| n.contains_condition_or_defer()),
            Self::Flatten(node) => node.node.contains_condition_or_defer(),
            Self::Fetch(..) => false,
            Self::Defer { .. } => true,
            Self::Condition { .. } => true,
        }
    }

    pub(crate) fn is_deferred(
        &self,
        operation: Option<&str>,
        variables: &Object,
        query: &Query,
    ) -> bool {
        match self {
            Self::Sequence { nodes } => nodes
                .iter()
                .any(|n| n.is_deferred(operation, variables, query)),
            Self::Parallel { nodes } => nodes
                .iter()
                .any(|n| n.is_deferred(operation, variables, query)),
            Self::Flatten(node) => node.node.is_deferred(operation, variables, query),
            Self::Fetch(..) => false,
            Self::Defer { .. } => true,
            Self::Condition {
                if_clause,
                else_clause,
                condition,
            } => {
                if query
                    .variable_value(operation, condition.as_str(), variables)
                    .map(|v| *v == Value::Bool(true))
                    .unwrap_or(true)
                {
                    // right now ConditionNode is only used with defer, but it might be used
                    // in the future to implement @skip and @include execution
                    if let Some(node) = if_clause {
                        if node.is_deferred(operation, variables, query) {
                            return true;
                        }
                    }
                } else if let Some(node) = else_clause {
                    if node.is_deferred(operation, variables, query) {
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
    ) -> Result<HashMap<(Option<Path>, String), Query>, QueryPlannerError> {
        // re-create full query with the right path
        // parse the subselection
        let mut subselections = HashMap::new();
        self.collect_subselections(schema, &Path::default(), &mut subselections)?;

        Ok(subselections)
    }

    fn collect_subselections(
        &self,
        schema: &Schema,
        initial_path: &Path,
        subselections: &mut HashMap<(Option<Path>, String), Query>,
    ) -> Result<(), QueryPlannerError> {
        // re-create full query with the right path
        // parse the subselection
        match self {
            Self::Sequence { nodes } | Self::Parallel { nodes } => {
                nodes.iter().try_fold(subselections, |subs, current| {
                    current.collect_subselections(schema, initial_path, subs)?;

                    Ok::<_, QueryPlannerError>(subs)
                })?;
                Ok(())
            }
            Self::Flatten(node) => {
                node.node
                    .collect_subselections(schema, initial_path, subselections)
            }
            Self::Defer { primary, deferred } => {
                // TODO rebuilt subselection from the root thanks to the path
                let primary_path = initial_path.join(&primary.path.clone().unwrap_or_default());
                if let Some(primary_subselection) = &primary.subselection {
                    let query = reconstruct_full_query(&primary_path, primary_subselection);
                    // ----------------------- Parse ---------------------------------
                    let sub_selection = Query::parse(&query, schema, &Default::default())?;
                    // ----------------------- END Parse ---------------------------------

                    subselections.insert(
                        (Some(primary_path), primary_subselection.clone()),
                        sub_selection,
                    );
                }

                deferred.iter().try_fold(subselections, |subs, current| {
                    if let Some(subselection) = &current.subselection {
                        let query = reconstruct_full_query(&current.path, subselection);
                        // ----------------------- Parse ---------------------------------
                        let sub_selection = Query::parse(&query, schema, &Default::default())?;
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
                        )?;
                    }

                    Ok::<_, QueryPlannerError>(subs)
                })?;
                Ok(())
            }
            Self::Fetch(..) => Ok(()),
            Self::Condition {
                if_clause,
                else_clause,
                ..
            } => {
                if let Some(node) = if_clause {
                    node.collect_subselections(schema, initial_path, subselections)?;
                }
                if let Some(node) = else_clause {
                    node.collect_subselections(schema, initial_path, subselections)?;
                }
                Ok(())
            }
        }
    }
}

impl QueryPlan {
    /// Execute the plan and return a [`Response`].
    pub(crate) async fn execute<'a, SF>(
        &self,
        context: &'a Context,
        service_factory: &'a Arc<SF>,
        supergraph_request: &'a Arc<http::Request<Request>>,
        schema: &'a Schema,
        sender: futures::channel::mpsc::Sender<Response>,
    ) -> Response
    where
        SF: SubgraphServiceFactory,
    {
        let root = Path::empty();

        log::trace_query_plan(&self.root);
        let deferred_fetches = HashMap::new();
        let (value, subselection, errors) = self
            .root
            .execute_recursively(
                &ExecutionParameters {
                    context,
                    service_factory,
                    schema,
                    supergraph_request,
                    deferred_fetches: &deferred_fetches,
                    query: &self.query,
                    options: &self.options,
                },
                &root,
                &Value::default(),
                sender,
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

// holds the query plan executon arguments that do not change between calls
pub(crate) struct ExecutionParameters<'a, SF> {
    context: &'a Context,
    service_factory: &'a Arc<SF>,
    schema: &'a Schema,
    supergraph_request: &'a Arc<http::Request<Request>>,
    deferred_fetches: &'a HashMap<String, Sender<(Value, Vec<Error>)>>,
    query: &'a Arc<Query>,
    options: &'a QueryPlanOptions,
}

impl PlanNode {
    fn execute_recursively<'a, SF>(
        &'a self,
        parameters: &'a ExecutionParameters<'a, SF>,
        current_dir: &'a Path,
        parent_value: &'a Value,
        sender: futures::channel::mpsc::Sender<Response>,
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
                    let span = tracing::info_span!(SEQUENCE_SPAN_NAME);
                    for node in nodes {
                        let (v, subselect, err) = node
                            .execute_recursively(parameters, current_dir, &value, sender.clone())
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

                    let span = tracing::info_span!(PARALLEL_SPAN_NAME);
                    let mut stream: stream::FuturesUnordered<_> = nodes
                        .iter()
                        .map(|plan| {
                            plan.execute_recursively(
                                parameters,
                                current_dir,
                                parent_value,
                                sender.clone(),
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
                    // Note that the span must be `info` as we need to pick this up in apollo tracing
                    let current_dir = current_dir.join(path);
                    let (v, subselect, err) = node
                        .execute_recursively(
                            parameters,
                            // this is the only command that actually changes the "current dir"
                            &current_dir,
                            parent_value,
                            sender,
                        )
                        .instrument(
                            tracing::info_span!(FLATTEN_SPAN_NAME, apollo_private.path = %current_dir),
                        )
                        .await;

                    value = v;
                    errors = err;
                    subselection = subselect;
                }
                PlanNode::Fetch(fetch_node) => {
                    let fetch_time_offset =
                        parameters.context.created_at.elapsed().as_nanos() as i64;
                    match fetch_node
                        .fetch_node(parameters, parent_value, current_dir)
                        .instrument(tracing::info_span!(
                            FETCH_SPAN_NAME,
                            "otel.kind" = %SpanKind::Internal,
                            "service.name" = fetch_node.service_name.as_str(),
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
                        let sc = parameters.schema.clone();
                        let orig = parameters.supergraph_request.clone();
                        let sf = parameters.service_factory.clone();
                        let ctx = parameters.context.clone();
                        let opt = parameters.options.clone();
                        let query = parameters.query.clone();
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
                            let deferred_fetches = HashMap::new();

                            if let Some(node) = deferred_inner {
                                let (mut v, node_subselection, err) = node
                                    .execute_recursively(
                                        &ExecutionParameters {
                                            context: &ctx,
                                            service_factory: &sf,
                                            schema: &sc,
                                            supergraph_request: &orig,
                                            deferred_fetches: &deferred_fetches,
                                            query: &query,
                                            options: &opt,
                                        },
                                        &Path::default(),
                                        &value,
                                        tx.clone(),
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
                                tx.disconnect();
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
                                tx.disconnect();
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
                                &ExecutionParameters {
                                    context: parameters.context,
                                    service_factory: parameters.service_factory,
                                    schema: parameters.schema,
                                    supergraph_request: parameters.supergraph_request,
                                    deferred_fetches: &deferred_fetches,
                                    options: parameters.options,
                                    query: parameters.query,
                                },
                                current_dir,
                                &value,
                                sender,
                            )
                            .instrument(span.clone())
                            .in_current_span()
                            .await;
                        let _guard = span.enter();
                        value.deep_merge(v);
                        errors.extend(err.into_iter());
                        subselection = primary_subselection.clone();

                        let _ = primary_sender.send(value.clone());
                    } else {
                        let _guard = span.enter();

                        subselection = primary_subselection.clone();

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
                            let span = tracing::info_span!("condition_if");
                            let (v, subselect, err) = node
                                .execute_recursively(
                                    parameters,
                                    current_dir,
                                    parent_value,
                                    sender.clone(),
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
                                parameters,
                                current_dir,
                                parent_value,
                                sender.clone(),
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
    use serde::Serialize;
    use tower::ServiceExt;
    use tracing::instrument;
    use tracing::Instrument;

    use super::selection::select_object;
    use super::selection::Selection;
    use super::ExecutionParameters;
    use crate::error::Error;
    use crate::error::FetchError;
    use crate::graphql::Request;
    use crate::json_ext::Object;
    use crate::json_ext::Path;
    use crate::json_ext::Value;
    use crate::json_ext::ValueExt;
    use crate::services::subgraph_service::SubgraphServiceFactory;
    use crate::*;

    /// GraphQL operation type.
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    #[non_exhaustive]
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
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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
            request: &Arc<http::Request<Request>>,
            schema: &Schema,
            enable_deduplicate_variables: bool,
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
                let (paths, representations) = if enable_deduplicate_variables {
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
                // with nested operations (Query or Mutation has an operation returning a Query or Mutation),
                // when the first fetch fails, the query plan will still execute up until the second fetch,
                // where `requires` is empty (not a federated fetch), the current dir is not emmpty (child of
                // the previous operation field) and the data is null. In that case, we recognize that we
                // should not perform the next fetch
                if !current_dir.is_empty()
                    && data
                        .get_path(current_dir)
                        .map(|value| value.is_null())
                        .unwrap_or(true)
                {
                    return None;
                }

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
            parameters: &'a ExecutionParameters<'a, SF>,
            data: &'a Value,
            current_dir: &'a Path,
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
                parameters.supergraph_request,
                parameters.schema,
                parameters.options.enable_deduplicate_variables,
            )
            .await
            {
                Some(variables) => variables,
                None => {
                    return Ok((Value::Object(Object::default()), Vec::new()));
                }
            };

            let subgraph_url = parameters
                .schema
                .subgraphs()
                .find_map(|(name, url)| (name == service_name).then(|| url))
                .ok_or_else(|| FetchError::SubrequestHttpError {
                    service: service_name.to_string(),
                    reason: format!(
                        "schema uri for subgraph '{}' should already have been checked",
                        service_name
                    ),
                })?
                .clone();

            let request = http_ext::Request::builder()
                .method(http::Method::POST)
                .uri(subgraph_url)
                .body(
                    Request::builder()
                        .query(operation)
                        .and_operation_name(operation_name.clone())
                        .variables(variables.clone())
                        .build(),
                )
                .build()
                .map_err(|e| FetchError::SubrequestHttpError {
                    service: service_name.to_string(),
                    reason: format!("could not construct subgraph request: {}", e),
                })?;

            let subgraph_request = SubgraphRequest::builder()
                .supergraph_request(parameters.supergraph_request.clone())
                .subgraph_request(request)
                .operation_kind(*operation_kind)
                .context(parameters.context.clone())
                .build();

            let service = parameters
                .service_factory
                .new_service(service_name)
                .ok_or_else(|| FetchError::SubrequestHttpError {
                    service: service_name.to_string(),
                    reason: "could not create subgraph service".to_string(),
                })?;

            // TODO not sure if we need a RouterReponse here as we don't do anything with it
            let (_parts, response) = service
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
                .response
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
                        if let Some(sender) = parameters.deferred_fetches.get(id.as_str()) {
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
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlattenNode {
    /// The path when result should be merged.
    path: Path,

    /// The child execution plan.
    node: Box<PlanNode>,
}

/// A primary query for a Defer node, the non deferred part
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Primary {
    /// Optional path, set if and only if the defer node is a
    /// nested defer. If set, `subselection` starts at that `path`.
    path: Option<Path>,

    /// The part of the original query that "selects" the data to
    /// send in that primary response (once the plan in `node` completes).
    subselection: Option<String>,

    // The plan to get all the data for that primary part
    node: Option<Box<PlanNode>>,
}

/// The "deferred" parts of the defer (note that it's an array). Each
/// of those deferred elements will correspond to a different chunk of
/// the response to the client (after the initial non-deferred one that is).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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
                PlanNode::Defer { primary, .. } => primary.subselection.clone(),
                _ => None,
            })
        })
    }
}
/// A deferred node.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use http::Method;
    use serde_json_bytes::json;

    use super::*;
    use crate::json_ext::PathElement;
    use crate::plugin::test::MockSubgraph;
    use crate::plugin::test::MockSubgraphFactory;
    use crate::query_planner::fetch::FetchNode;
    use crate::services::subgraph_service::MakeSubgraphService;

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
    #[should_panic(expected = "this panic should be propagated to the test harness")]
    async fn mock_subgraph_service_withf_panics_should_be_reported_as_service_closed() {
        let query_plan: QueryPlan = QueryPlan {
            root: serde_json::from_str(test_query_plan!()).unwrap(),
            formatted_query_plan: Default::default(),
            options: QueryPlanOptions::default(),
            query: Arc::new(Query::default()),
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
        };

        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service.expect_call().times(1).withf(|_| {
            panic!("this panic should be propagated to the test harness");
        });
        mock_products_service.expect_clone().return_once(|| {
            let mut mock_products_service = plugin::test::MockSubgraphService::new();
            mock_products_service.expect_call().times(1).withf(|_| {
                panic!("this panic should be propagated to the test harness");
            });
            mock_products_service
        });

        let (sender, _) = futures::channel::mpsc::channel(10);
        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([(
                "product".into(),
                Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
            )]),
            plugins: Default::default(),
        });

        let result = query_plan
            .execute(
                &Context::new(),
                &sf,
                &Default::default(),
                &Schema::parse(test_schema!(), &Default::default()).unwrap(),
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
            formatted_query_plan: Default::default(),
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
            query: Arc::new(Query::default()),
            options: QueryPlanOptions::default(),
        };

        let succeeded: Arc<AtomicBool> = Default::default();
        let inner_succeeded = Arc::clone(&succeeded);

        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service.expect_clone().return_once(|| {
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
            mock_products_service
        });

        let (sender, _) = futures::channel::mpsc::channel(10);

        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([(
                "product".into(),
                Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
            )]),
            plugins: Default::default(),
        });

        let _response = query_plan
            .execute(
                &Context::new(),
                &sf,
                &Default::default(),
                &Schema::parse(test_schema!(), &Default::default()).unwrap(),
                sender,
            )
            .await;

        assert!(succeeded.load(Ordering::SeqCst), "incorrect operation name");
    }

    #[tokio::test]
    async fn fetch_makes_post_requests() {
        let query_plan: QueryPlan = QueryPlan {
            root: serde_json::from_str(test_query_plan!()).unwrap(),
            formatted_query_plan: Default::default(),
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
            query: Arc::new(Query::default()),
            options: QueryPlanOptions::default(),
        };

        let succeeded: Arc<AtomicBool> = Default::default();
        let inner_succeeded = Arc::clone(&succeeded);

        let mut mock_products_service = plugin::test::MockSubgraphService::new();

        mock_products_service.expect_clone().return_once(|| {
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
            mock_products_service
        });

        let (sender, _) = futures::channel::mpsc::channel(10);

        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([(
                "product".into(),
                Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
            )]),
            plugins: Default::default(),
        });

        let _response = query_plan
            .execute(
                &Context::new(),
                &sf,
                &Default::default(),
                &Schema::parse(test_schema!(), &Default::default()).unwrap(),
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
            formatted_query_plan: Default::default(),
            root: PlanNode::Defer {
                primary: Primary {
                    path: None,
                    subselection: Some("{ t { x } }".to_string()),
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
            query: Arc::new(Query::default()),
            options: QueryPlanOptions::default(),
        };

        let mut mock_x_service = plugin::test::MockSubgraphService::new();
        mock_x_service.expect_clone().return_once(|| {
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
            mock_x_service
        });

        let mut mock_y_service = plugin::test::MockSubgraphService::new();
        mock_y_service.expect_clone().return_once(|| {
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
            mock_y_service
        });

        let (sender, mut receiver) = futures::channel::mpsc::channel(10);

        let schema = include_str!("testdata/defer_schema.graphql");
        let schema = Schema::parse(schema, &Default::default()).unwrap();
        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([
                (
                    "X".into(),
                    Arc::new(mock_x_service) as Arc<dyn MakeSubgraphService>,
                ),
                (
                    "Y".into(),
                    Arc::new(mock_y_service) as Arc<dyn MakeSubgraphService>,
                ),
            ]),
            plugins: Default::default(),
        });

        let response = query_plan
            .execute(&Context::new(), &sf, &Default::default(), &schema, sender)
            .await;

        // primary response
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json! {{"data":{"t":{"id":1234,"__typename":"T","x":"X"}}}}
        );

        let response = receiver.next().await.unwrap();

        // deferred response
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            // the primary response appears there because the deferred response gets data from it
            // unneeded parts are removed in response formatting
            serde_json::json! {{"data":{"t":{"y":"Y","__typename":"T","id":1234,"x":"X"}},"path":["t"]}}
        );
    }

    #[tokio::test]
    async fn defer_if_condition() {
        let query = r#"
        query Me($shouldDefer: Boolean) {
            me {
              id
              ... @defer(if: $shouldDefer) {
                name
                username
              }
            }
          }"#;

        let schema = include_str!("testdata/defer_clause.graphql");
        let schema = Schema::parse(schema, &Default::default()).unwrap();

        let root: PlanNode =
            serde_json::from_str(include_str!("testdata/defer_clause_plan.json")).unwrap();

        let query_plan = QueryPlan {
            root,
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
            query: Arc::new(
                Query::parse(
                    query,
                    &schema,
                    &Configuration::fake_builder().build().unwrap(),
                )
                .unwrap(),
            ),
            options: QueryPlanOptions::default(),
            formatted_query_plan: None,
        };

        let mocked_accounts = MockSubgraph::builder()
        // defer if true
        .with_json(
            serde_json::json! {{"query":"query Me__accounts__0{me{__typename id}}", "operationName":"Me__accounts__0"}},
            serde_json::json! {{"data": {"me": {"__typename": "User", "id": "1"}}}},
        )
        .with_json(
            serde_json::json! {{"query":"query Me__accounts__1($representations:[_Any!]!){_entities(representations:$representations){...on User{name username}}}", "operationName":"Me__accounts__1", "variables":{"representations":[{"__typename":"User","id":"1"}]}}},
            serde_json::json! {{"data": {"_entities": [{"name": "Ada Lovelace", "username": "@ada"}]}}},
        )
        // defer if false
        .with_json(serde_json::json! {{"query": "query Me__accounts__2{me{id name username}}", "operationName":"Me__accounts__2"}},
        serde_json::json! {{"data": {"me": {"id": "1", "name": "Ada Lovelace", "username": "@ada"}}}},
    )
        .build();

        let (sender, mut receiver) = futures::channel::mpsc::channel(10);

        let service_factory = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([(
                "accounts".into(),
                Arc::new(mocked_accounts) as Arc<dyn MakeSubgraphService>,
            )]),
            plugins: Default::default(),
        });

        let defer_primary_response = query_plan
            .execute(
                &Context::new(),
                &service_factory,
                &Arc::new(
                    http::Request::builder()
                        .body(
                            request::Request::fake_builder()
                                .variables(
                                    json!({ "shouldDefer": true }).as_object().unwrap().clone(),
                                )
                                .build(),
                        )
                        .unwrap(),
                ),
                &schema,
                sender,
            )
            .await;

        // shouldDefer: true
        insta::assert_json_snapshot!(defer_primary_response);
        let deferred_response = receiver.next().await.unwrap();
        insta::assert_json_snapshot!(deferred_response);
        assert!(receiver.next().await.is_none());

        // shouldDefer: not provided, should default to true
        let (default_sender, mut default_receiver) = futures::channel::mpsc::channel(10);
        let default_primary_response = query_plan
            .execute(
                &Context::new(),
                &service_factory,
                &Default::default(),
                &schema,
                default_sender,
            )
            .await;

        assert_eq!(defer_primary_response, default_primary_response);
        assert_eq!(deferred_response, default_receiver.next().await.unwrap());
        assert!(default_receiver.next().await.is_none());

        // shouldDefer: false, only 1 response
        let (sender, mut no_defer_receiver) = futures::channel::mpsc::channel(10);
        let defer_disabled = query_plan
            .execute(
                &Context::new(),
                &service_factory,
                &Arc::new(
                    http::Request::builder()
                        .body(
                            request::Request::fake_builder()
                                .variables(
                                    json!({ "shouldDefer": false }).as_object().unwrap().clone(),
                                )
                                .build(),
                        )
                        .unwrap(),
                ),
                &schema,
                sender,
            )
            .await;
        insta::assert_json_snapshot!(defer_disabled);
        assert!(no_defer_receiver.next().await.is_none());
    }

    #[tokio::test]
    async fn dependent_mutations() {
        let schema = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1"),
        @core(feature: "https://specs.apollo.dev/join/v0.1")
      {
        query: Query
        mutation: Mutation
      }

      directive @core(feature: String!) repeatable on SCHEMA
      directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
      directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
      directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
      directive @join__graph(name: String!, url: String!) on ENUM_VALUE
      scalar join__FieldSet

      enum join__Graph {
        A @join__graph(name: "A" url: "http://localhost:4001")
        B @join__graph(name: "B" url: "http://localhost:4004")
      }

      type Mutation {
          mutationA: Mutation @join__field(graph: A)
          mutationB: Boolean @join__field(graph: B)
      }

      type Query {
          query: Boolean @join__field(graph: A)
      }"#;

        let query_plan: QueryPlan = QueryPlan {
            // generated from:
            // mutation {
            //   mutationA {
            //     mutationB
            //   }
            // }
            formatted_query_plan: Default::default(),
            root: serde_json::from_str(
                r#"{
                "kind": "Sequence",
                "nodes": [
                    {
                        "kind": "Fetch",
                        "serviceName": "A",
                        "variableUsages": [],
                        "operation": "mutation{mutationA{__typename}}",
                        "operationKind": "mutation"
                    },
                    {
                        "kind": "Flatten",
                        "path": [
                            "mutationA"
                        ],
                        "node": {
                            "kind": "Fetch",
                            "serviceName": "B",
                            "variableUsages": [],
                            "operation": "mutation{...on Mutation{mutationB}}",
                            "operationKind": "mutation"
                        }
                    }
                ]
            }"#,
            )
            .unwrap(),
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
            query: Arc::new(Query::default()),
            options: QueryPlanOptions::default(),
        };

        let mut mock_a_service = plugin::test::MockSubgraphService::new();
        mock_a_service.expect_clone().returning(|| {
            let mut mock_a_service = plugin::test::MockSubgraphService::new();
            mock_a_service
                .expect_call()
                .times(1)
                .returning(|_| Ok(SubgraphResponse::fake_builder().build()));

            mock_a_service
        });

        // the first fetch returned null, so there should never be a call to B
        let mut mock_b_service = plugin::test::MockSubgraphService::new();
        mock_b_service.expect_call().never();

        let sf = Arc::new(MockSubgraphFactory {
            subgraphs: HashMap::from([
                (
                    "A".into(),
                    Arc::new(mock_a_service) as Arc<dyn MakeSubgraphService>,
                ),
                (
                    "B".into(),
                    Arc::new(mock_b_service) as Arc<dyn MakeSubgraphService>,
                ),
            ]),
            plugins: Default::default(),
        });

        let (sender, _) = futures::channel::mpsc::channel(10);
        let _response = query_plan
            .execute(
                &Context::new(),
                &sf,
                &Default::default(),
                &Schema::parse(schema, &Default::default()).unwrap(),
                sender,
            )
            .await;
    }
}
