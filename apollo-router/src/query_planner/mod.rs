use std::sync::Arc;

pub(crate) use bridge_query_planner::*;
pub(crate) use caching_query_planner::*;
pub use fetch::OperationKind;
use futures::prelude::*;
use opentelemetry::trace::SpanKind;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
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
}

impl PlanNode {
    pub(crate) fn contains_mutations(&self) -> bool {
        match self {
            Self::Sequence { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Parallel { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Fetch(fetch_node) => fetch_node.operation_kind() == &OperationKind::Mutation,
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

    /// Execute the plan and return a [`Response`].
    pub async fn execute<'a, SF>(
        &self,
        context: &'a Context,
        service_factory: &'a Arc<SF>,
        originating_request: http_ext::Request<Request>,
        schema: &'a Schema,

        _sender: futures::channel::mpsc::Sender<Response>,
    ) -> Response
    where
        SF: SubgraphServiceFactory,
    {
        let root = Path::empty();

        log::trace_query_plan(&self.root);

        let (value, errors) = self
            .root
            .execute_recursively(
                &root,
                context,
                service_factory,
                schema,
                originating_request,
                &Value::default(),
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
    fn execute_recursively<'a, SF>(
        &'a self,
        current_dir: &'a Path,
        context: &'a Context,
        service_factory: &'a Arc<SF>,
        schema: &'a Schema,
        originating_request: http_ext::Request<Request>,
        parent_value: &'a Value,
        options: &'a QueryPlanOptions,
    ) -> future::BoxFuture<(Value, Vec<Error>)>
    where
        SF: SubgraphServiceFactory,
    {
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
                                service_factory,
                                schema,
                                originating_request.clone(),
                                &value,
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
                                service_factory,
                                schema,
                                originating_request.clone(),
                                parent_value,
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
                            service_factory,
                            schema,
                            originating_request,
                            parent_value,
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
                            service_factory,
                            originating_request,
                            schema,
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
            }

            (value, errors)
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
        }
    }
}

pub(crate) mod fetch {
    use std::collections::HashMap;
    use std::fmt::Display;
    use std::sync::Arc;

    use indexmap::IndexSet;
    use serde::Deserialize;
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
        service_name: String,

        /// The data that is required for the subgraph fetch.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        #[serde(default)]
        requires: Vec<Selection>,

        /// The variables that are used for the subgraph fetch.
        variable_usages: Vec<String>,

        /// The GraphQL subquery that is used for the fetch.
        operation: String,

        /// The GraphQL subquery operation name.
        operation_name: Option<String>,

        /// The GraphQL operation kind that is used for the fetch.
        operation_kind: OperationKind,
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
        pub(crate) async fn fetch_node<'a, SF>(
            &'a self,
            data: &'a Value,
            current_dir: &'a Path,
            context: &'a Context,
            service_factory: &'a Arc<SF>,
            originating_request: http_ext::Request<Request>,
            schema: &'a Schema,
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

            self.response_at_path(current_dir, paths, response.data.unwrap_or_default())
                .map(|value| (value, errors))
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
    use crate::plugin::test::MockSubgraphFactory;
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
}
