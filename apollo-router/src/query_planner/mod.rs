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
pub(crate) mod fetch;
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
                        let fut = deferred_node.execute(
                            parameters,
                            parent_value,
                            sender.clone(),
                            &primary_sender,
                            &mut deferred_fetches,
                        );

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

    fn execute<'a, 'b, SF>(
        &'b self,
        parameters: &'a ExecutionParameters<'a, SF>,
        parent_value: &Value,
        sender: futures::channel::mpsc::Sender<Response>,
        primary_sender: &Sender<Value>,
        deferred_fetches: &mut HashMap<String, Sender<(Value, Vec<Error>)>>,
    ) -> impl Future<Output = ()>
    where
        SF: SubgraphServiceFactory,
    {
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
        let deferred_path = self.path.clone();
        let subselection = self.subselection();
        let label = self.label.clone();
        let mut tx = sender;
        let sc = parameters.schema.clone();
        let orig = parameters.supergraph_request.clone();
        let sf = parameters.service_factory.clone();
        let ctx = parameters.context.clone();
        let opt = parameters.options.clone();
        let query = parameters.query.clone();
        let mut primary_receiver = primary_sender.subscribe();
        let mut value = parent_value.clone();

        async move {
            let mut errors = Vec::new();

            if is_depends_empty {
                let primary_value = primary_receiver.recv().await.unwrap_or_default();
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
                    let primary_value = primary_receiver.recv().await.unwrap_or_default();
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
                let primary_value = primary_receiver.recv().await.unwrap_or_default();
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
        }
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
mod tests;
