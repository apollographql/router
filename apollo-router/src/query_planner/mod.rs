use std::collections::HashMap;
use std::collections::HashSet;
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
use crate::error::FetchError;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::service_registry::ServiceRegistry;
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
}

impl PlanNode {
    pub(crate) fn contains_mutations(&self) -> bool {
        match self {
            Self::Sequence { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Parallel { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Fetch(fetch_node) => fetch_node.operation_kind() == &OperationKind::Mutation,
            Self::Defer { primary, .. } => primary.node.contains_mutations(),
            Self::Flatten(_) => false,
        }
    }
}

impl QueryPlan {
    /// Pass some options to the QueryPlan
    pub fn with_options(mut self, options: QueryPlanOptions) -> Self {
        self.options = options;
        self
    }

    /// Validate the entire request for variables and services used.
    #[tracing::instrument(skip_all, level = "debug", name = "validate")]
    pub fn validate(&self, service_registry: &ServiceRegistry) -> Result<(), Response> {
        let mut early_errors = Vec::new();
        for err in self.root.validate_services_against_plan(service_registry) {
            early_errors.push(err.to_graphql_error(None));
        }

        if !early_errors.is_empty() {
            Err(Response::builder().errors(early_errors).build())
        } else {
            Ok(())
        }
    }

    /// Execute the plan and return a [`Response`].
    pub async fn execute<'a>(
        &self,
        context: &'a Context,
        service_registry: &'a ServiceRegistry,
        originating_request: http_ext::Request<Request>,
        schema: &'a Schema,
        sender: futures::channel::mpsc::Sender<Response>,
    ) -> Response {
        let root = Path::empty();

        log::trace_query_plan(&self.root);
        let (value, errors) = self
            .root
            .execute_recursively(
                &root,
                context,
                service_registry,
                schema,
                originating_request,
                &Value::default(),
                &HashMap::new(),
                sender,
                &self.options,
            )
            .await;

        Response::builder().data(value).errors(errors).build()
    }

    pub fn contains_mutations(&self) -> bool {
        self.root.contains_mutations()
    }
}

impl PlanNode {
    #[allow(clippy::too_many_arguments)]
    fn execute_recursively<'a>(
        &'a self,
        current_dir: &'a Path,
        context: &'a Context,
        service_registry: &'a ServiceRegistry,
        schema: &'a Schema,
        originating_request: http_ext::Request<Request>,
        parent_value: &'a Value,
        deferred_fetches: &'a HashMap<String, Sender<Value>>,
        sender: futures::channel::mpsc::Sender<Response>,
        options: &'a QueryPlanOptions,
    ) -> future::BoxFuture<(Value, Vec<Error>)> {
        Box::pin(async move {
            tracing::trace!("executing plan:\n{:#?}", self);
            let mut value;
            let mut errors;

            match self {
                PlanNode::Sequence { nodes } => {
                    value = parent_value.clone();
                    errors = Vec::new();
                    let span = tracing::info_span!("sequence");
                    for node in nodes {
                        let (v, err) = node
                            .execute_recursively(
                                current_dir,
                                context,
                                service_registry,
                                schema,
                                originating_request.clone(),
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
                                service_registry,
                                schema,
                                originating_request.clone(),
                                parent_value,
                                deferred_fetches,
                                sender.clone(),
                                options,
                            )
                            .instrument(span.clone())
                        })
                        .collect();

                    while let Some((v, err)) = stream
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
                    let (v, err) = node
                        .execute_recursively(
                            // this is the only command that actually changes the "current dir"
                            &current_dir.join(path),
                            context,
                            service_registry,
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
                }
                PlanNode::Fetch(fetch_node) => {
                    match fetch_node
                        .fetch_node(
                            parent_value,
                            current_dir,
                            context,
                            service_registry,
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
                            path,
                            subselection,
                            node,
                        },
                    deferred,
                } => {
                    let mut deferred_fetches = HashMap::new();
                    let mut futures = Vec::new();

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

                        let mut stream: stream::FuturesUnordered<_> =
                            deferred_receivers.into_iter().collect();
                        //FIXME/ is there a solution without cloning the entire node? Maybe it could be moved instead?
                        let deferred_inner = deferred_node.node.clone();
                        let deferred_path = deferred_node.path.clone();
                        let mut tx = sender.clone();
                        let sc = schema.clone();
                        let orig = originating_request.clone();
                        let sr = service_registry.clone();
                        let ctx = context.clone();
                        let opt = options.clone();
                        let fut = async move {
                            let mut value = Value::default();

                            while let Some(v) = stream.next().await {
                                let deferred_value = v.0.unwrap().unwrap();
                                println!("got deferred value: {:?}", deferred_value);
                                value.deep_merge(deferred_value);
                            }

                            let span = tracing::info_span!("deferred");

                            if let Some(node) = deferred_inner {
                                println!(
                                    "\nwill execute deferred node at path {}: {:?}",
                                    deferred_path, node
                                );
                                let (v, err) = node
                                    .execute_recursively(
                                        &Path::default(),
                                        &ctx,
                                        &sr,
                                        &sc,
                                        orig,
                                        &value,
                                        &HashMap::new(),
                                        tx.clone(),
                                        &opt,
                                    )
                                    .instrument(span.clone())
                                    .in_current_span()
                                    .await;
                                println!("returning deferred {:?}", v);

                                let _ = tx
                                    .send(Response::builder().data(v).errors(err).build())
                                    .await;
                            } else {
                                todo!()
                            }
                        };

                        futures.push(fut);
                    }

                    tokio::task::spawn(async move {
                        join_all(futures).await;
                    });

                    value = parent_value.clone();
                    errors = Vec::new();
                    let span = tracing::info_span!("primary");
                    let (v, err) = node
                        .execute_recursively(
                            current_dir,
                            context,
                            service_registry,
                            schema,
                            originating_request.clone(),
                            &value,
                            &deferred_fetches,
                            sender,
                            options,
                        )
                        .instrument(span.clone())
                        .in_current_span()
                        .await;
                    value.deep_merge(v);
                    errors.extend(err.into_iter());
                }
            }

            (value, errors)
        })
    }

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
            Self::Defer { primary, deferred } => Box::new(
                primary.node.service_usage().chain(
                    deferred
                        .iter()
                        .map(|d| d.node.iter().flat_map(|node| node.service_usage()))
                        .flatten(),
                ),
            ),
        }
    }

    /// Recursively validate a query plan node making sure that all services are known before we go
    /// for execution.
    ///
    /// This simplifies processing later as we can always guarantee that services are configured for
    /// the plan.
    ///
    /// # Arguments
    ///
    ///  *   `plan`: The root query plan node to validate.
    fn validate_services_against_plan(
        &self,
        service_registry: &ServiceRegistry,
    ) -> Vec<FetchError> {
        self.service_usage()
            .filter(|service| !service_registry.contains(service))
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|service| FetchError::ValidationUnknownServiceError {
                service: service.to_string(),
            })
            .collect::<Vec<_>>()
    }
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
    use crate::service_registry::ServiceRegistry;
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
            request: http_ext::Request<Request>,
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
                                        paths.insert(path, index);
                                    }
                                    None => {
                                        paths.insert(path, values.len());
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
                                paths.insert(path, values.len());
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
        pub(crate) async fn fetch_node<'a>(
            &'a self,
            data: &'a Value,
            current_dir: &'a Path,
            context: &'a Context,
            service_registry: &'a ServiceRegistry,
            originating_request: http_ext::Request<Request>,
            schema: &'a Schema,
            deferred_fetches: &'a HashMap<String, Sender<Value>>,
            options: &QueryPlanOptions,
        ) -> Result<(Value, Vec<Error>), FetchError> {
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
                originating_request.clone(),
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
                .originating_request(Arc::new(originating_request))
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

            let service = service_registry
                .get(service_name)
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
            let errors = response
                .errors
                .into_iter()
                .map(|error| Error {
                    locations: error.locations,
                    path: error.path.map(|path| current_dir.join(path)),
                    message: error.message,
                    extensions: error.extensions,
                })
                .collect();

            println!(
                "response_at_path({}), paths = {:?}, data = {:?}",
                current_dir, paths, response.data
            );
            match self.response_at_path(current_dir, paths, response.data.unwrap_or_default()) {
                Ok(value) => {
                    if let Some(id) = &self.id {
                        if let Some(sender) = deferred_fetches.get(id.as_str()) {
                            println!(
                                "will send data from fetch node '{}': {:?}",
                                id,
                                value.as_object().as_ref().unwrap() //.get("data")
                            );
                            sender.clone().send(
                                value
                                    //.as_object()
                                    //.and_then(|o| o.get("data"))
                                    //.unwrap()
                                    .clone(),
                            );
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
    node: Box<PlanNode>,
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

    use crate::json_ext::PathElement;
    use crate::query_planner::fetch::FetchNode;

    use super::*;
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

        let result = query_plan
            .execute(
                &Context::new(),
                &ServiceRegistry::new(HashMap::from([(
                    "product".into(),
                    ServiceBuilder::new()
                        .buffer(1)
                        .service(mock_products_service.build().boxed()),
                )])),
                http_ext::Request::fake_builder()
                    .headers(Default::default())
                    .body(Default::default())
                    .build()
                    .expect("fake builds should always work; qed"),
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

        let _response = query_plan
            .execute(
                &Context::new(),
                &ServiceRegistry::new(HashMap::from([(
                    "product".into(),
                    ServiceBuilder::new()
                        .buffer(1)
                        .service(mock_products_service.build().boxed()),
                )])),
                http_ext::Request::fake_builder()
                    .headers(Default::default())
                    .body(Default::default())
                    .build()
                    .expect("fake builds should always work; qed"),
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

        let _response = query_plan
            .execute(
                &Context::new(),
                &ServiceRegistry::new(HashMap::from([(
                    "product".into(),
                    ServiceBuilder::new()
                        .buffer(1)
                        .service(mock_products_service.build().boxed()),
                )])),
                http_ext::Request::fake_builder()
                    .headers(Default::default())
                    .body(Default::default())
                    .build()
                    .expect("fake builds should always work; qed"),
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
                    path: Some(Path(vec![
                        /*PathElement::Key("t".to_string()),
                        PathElement::Key("x".to_string()),*/
                    ])),
                    subselection: "{ t { x } }".to_string(),
                    node: Box::new(PlanNode::Fetch(FetchNode {
                        service_name: "X".to_string(),
                        requires: vec![],
                        variable_usages: vec![],
                        operation: "{ t { id __typename x } }".to_string(),
                        operation_name: Some("t".to_string()),
                        operation_kind: OperationKind::Query,
                        id: Some("fetch1".to_string()),
                    })),
                },
                deferred: vec![DeferredNode {
                    depends: vec![Depends {
                        id: "fetch1".to_string(),
                        defer_label: None,
                    }],
                    label: None,
                    path: Path(vec![PathElement::Key("t".to_string())]),
                    subselection: Some("{ ... on T { y } }".to_string()),
                    node: Some(Arc::new(PlanNode::Fetch(FetchNode {
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
                        operation: "{ t { y } }".to_string(),
                        operation_name: Some("t".to_string()),
                        operation_kind: OperationKind::Query,
                        id: Some("fetch2".to_string()),
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
                        "data": { "t": {"id": 1234,
                        "__typename": "T",
                         "x": "X"
                        }}
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

        let response = query_plan
            .execute(
                &Context::new(),
                &ServiceRegistry::new(HashMap::from([
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
                ])),
                http_ext::Request::fake_builder()
                    .headers(Default::default())
                    .body(Default::default())
                    .build()
                    .expect("fake builds should always work; qed"),
                &schema,
                sender,
            )
            .await;
        println!(
            "got primary response: {}",
            serde_json::to_string_pretty(&response.data.unwrap()).unwrap()
        );
        let response = receiver.next().await.unwrap();
        println!(
            "got deferred response: {}",
            serde_json::to_string_pretty(&response.data.unwrap()).unwrap()
        );

        panic!();
    }
}
