use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use futures::lock::Mutex;
use futures::prelude::*;
use serde_json::{Map, Value};

use query_planner::model::{FetchNode, FlattenNode, PlanNode, QueryPlan, SelectionSet};
use query_planner::{QueryPlanOptions, QueryPlanner, QueryPlannerError};

use crate::traverser::Traverser;
use crate::{
    FetchError, GraphQLFetcher, GraphQLPrimaryResponse, GraphQLRequest, GraphQLResponse,
    GraphQLResponseStream, Path, PathElement, ServiceRegistry,
};
use futures::{FutureExt, StreamExt};

type TraverserStream = Pin<Box<dyn Stream<Item = Traverser> + Send>>;
type EmptyFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Federated graph fetcher creates a query plan and executes the plan against one or more
/// subgraphs. For information on how the algorithm works refer to the README for this crate.
#[derive(Clone, Debug)]
pub struct FederatedGraph {
    query_planner: Arc<Mutex<dyn QueryPlanner>>,
    service_registry: Arc<dyn ServiceRegistry>,
    concurrency_factor: usize,
    chunk_size: usize,
}

impl FederatedGraph {
    /// Create a new federated graph fetcher.
    /// query_planner is shared between threads and requires a lock for planning:
    /// 1. query planners may be mutable for caching state.
    /// 2. we can clone FederatedGraph for use across threads so we can make use of syntax.
    ///
    /// service_registry is shared between threads, but is send and sync and therefore does not need
    /// a mutex.
    ///
    /// concurrency_factor and chunk_size are not exposed right now. Setting chunk_size to 1 has
    /// the effect of serializing the execution in a predictable order, which can be useful for
    /// debugging.
    ///
    /// In future we may allow concurrency_factor and chunk_size to be set explicitly to allow
    /// clients to avoid stalled execution at the cost of making more downstream calls.
    ///
    /// # Arguments
    ///
    /// * `query_planner`: The query planner to use to for planning.
    /// * `service_registry`: The registry of service name to fetcher.
    ///
    /// returns: FederatedGraph
    ///
    pub fn new<T>(query_planner: T, service_registry: Arc<dyn ServiceRegistry>) -> Self
    where
        T: QueryPlanner + 'static,
    {
        Self {
            concurrency_factor: 100000,
            chunk_size: 100000,
            query_planner: Arc::new(Mutex::new(query_planner)),
            service_registry,
        }
    }

    /// Create a query plan via the query planner   
    ///
    /// # Arguments
    ///
    /// * `request`: The request to be planned.
    ///
    /// returns: Result<QueryPlan, FetchError>
    ///
    async fn plan(self, request: Arc<GraphQLRequest>) -> Result<QueryPlan, FetchError> {
        let mut query_planner = self.query_planner.lock().await;
        let query_plan = query_planner.get(
            request.query.to_owned(),
            request.operation_name.to_owned(),
            QueryPlanOptions::default(),
        )?;

        Ok(query_plan)
    }

    /// Visit a stream of traversers with a plan node.
    /// Dispatches the visit to the fetch, sequence, parallel, or flatten operations.
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `node`: The query plan node.
    /// * `request`: The GraphQL original request.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    fn visit(
        self,
        traversers: TraverserStream,
        node: PlanNode,
        request: Arc<GraphQLRequest>,
    ) -> EmptyFuture {
        let concurrency_factor = self.concurrency_factor;

        let variables = match node {
            PlanNode::Fetch(ref fetch) if fetch.requires.is_none() => Arc::new(
                fetch
                    .variable_usages
                    .iter()
                    .filter_map(|key| {
                        request
                            .variables
                            .get(key)
                            .map(|value| (key.to_owned(), value.to_owned()))
                    })
                    .collect::<Map<_, _>>(),
            ),
            _ => Default::default(),
        };

        traversers
            .chunks(self.chunk_size)
            .map(move |traversers| {
                let traverser_stream = stream::iter(traversers).boxed();
                let clone = self.to_owned();
                match node.to_owned() {
                    PlanNode::Sequence { nodes } => {
                        clone.visit_sequence(traverser_stream, nodes, request.clone())
                    }
                    PlanNode::Parallel { nodes } => {
                        clone.visit_parallel(traverser_stream, nodes, request.clone())
                    }
                    PlanNode::Fetch(fetch) if fetch.requires.is_none() => {
                        clone.visit_fetch_no_select(traverser_stream, fetch, variables.clone())
                    }
                    PlanNode::Fetch(fetch) => {
                        clone.visit_fetch_select(traverser_stream, fetch, request.clone())
                    }
                    PlanNode::Flatten(flatten) => {
                        clone.visit_flatten(traverser_stream, flatten, request.clone())
                    }
                }
                .boxed()
            })
            .buffer_unordered(concurrency_factor)
            .for_each(|_| future::ready(()))
            .boxed()
    }

    /// Fetch where the plan node has a selection.
    /// Selection fetches are performed in bulk and the results are merged back into the originating
    /// traverser.
    ///
    /// For each traverser we try and obtain data from the content that matches the selection.
    /// Any traversers that do not match anything are dropped.
    ///
    /// The selections are aggregated and sent to the downstream service, the result is merged back
    /// in with the originating traverser.  
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `fetch`: The fetch plan node.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    ///
    fn visit_fetch_select(
        self,
        traversers: TraverserStream,
        fetch: FetchNode,
        request: Arc<GraphQLRequest>,
    ) -> EmptyFuture {
        traversers
            .collect::<Vec<Traverser>>()
            .map(move |traversers| {
                let service_name = fetch.service_name.to_owned();
                // We already checked that the service exists during planning
                let fetcher = self.service_registry.get(&service_name).unwrap();
                let (mut traversers, selections) =
                    traversers_with_selections(&fetch.requires, traversers);

                let mut variables = Map::with_capacity(1 + fetch.variable_usages.len());
                variables.extend(fetch.variable_usages.iter().filter_map(|key| {
                    request
                        .variables
                        .get(key)
                        .map(|value| (key.to_owned(), value.to_owned()))
                }));
                variables.insert(
                    "representations".into(),
                    construct_representations(selections),
                );

                fetcher
                    .stream(
                        GraphQLRequest::builder()
                            .query(fetch.operation)
                            .variables(variables)
                            .build(),
                    )
                    .into_future()
                    .map(move |(primary, _rest)| match primary {
                        // If we got results we zip the stream up with the original traverser and merge the results.
                        Some(GraphQLResponse::Primary(primary)) => {
                            merge_response(&mut traversers, primary);
                        }
                        Some(GraphQLResponse::Patch(_)) => {
                            traversers.iter_mut().for_each(|t| {
                                t.add_error(&FetchError::SubrequestMalformedResponse {
                                    service: service_name.to_owned(),
                                    reason: "Subrequest sent patch response as primary".to_string(),
                                })
                            });
                        }
                        _ => {
                            traversers.iter_mut().for_each(|t| {
                                t.add_error(&FetchError::SubrequestNoResponse {
                                    service: service_name.to_owned(),
                                })
                            });
                        }
                    })
                    .boxed()
            })
            .flatten()
            .boxed()
    }

    /// Perform a fetch with no selections.
    /// Without selections the queries for each traverser must be made independently and cannot be
    /// batched.
    ///
    /// In practice non selection queries are likely to happen only at the top level of a query plan
    /// and will therefore only have one traverser.
    ///
    /// If a non-selection query does happen at a lower level with multiple traversers the requests
    /// happen in parallel.
    ///
    /// # Arguments
    ///
    /// * `traversers`: The traversers to process.
    /// * `fetch`: The fetch node.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    ///
    fn visit_fetch_no_select(
        self,
        traversers: TraverserStream,
        fetch: FetchNode,
        variables: Arc<Map<String, Value>>,
    ) -> EmptyFuture {
        let concurrency_factor = self.concurrency_factor;
        traversers
            .map(move |mut traverser| {
                let service_name = fetch.service_name.to_owned();
                // We already validated that the service exists during planning
                let fetcher = self.service_registry.get(&service_name).unwrap();

                fetcher
                    .stream(
                        GraphQLRequest::builder()
                            .query(fetch.operation.clone())
                            .variables(variables.clone())
                            .build(),
                    )
                    .into_future()
                    .map(move |(primary, _rest)| match primary {
                        Some(GraphQLResponse::Primary(primary)) => {
                            traverser.merge(Some(&Value::Object(primary.data)));
                        }
                        Some(GraphQLResponse::Patch(_)) => {
                            panic!("Should not have had patch response as primary!")
                        }
                        None => traverser.add_error(&FetchError::SubrequestNoResponse {
                            service: service_name,
                        }),
                    })
                    .boxed()
            })
            .buffered(concurrency_factor)
            .for_each(|_| future::ready(()))
            .boxed()
    }

    /// Visit a sequence of plan nodes in turn.
    /// Execution waits for the previous operations to complete before executing the next operation
    /// in the query plan.
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `nodes`: The plan nodes in the sequence.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    fn visit_sequence(
        self,
        traversers: TraverserStream,
        nodes: Vec<PlanNode>,
        request: Arc<GraphQLRequest>,
    ) -> EmptyFuture {
        traversers
            .collect::<Vec<Traverser>>()
            .map(move |traversers| {
                // We now have a chunk of traversers
                nodes
                    .iter()
                    .fold(future::ready(()).boxed(), |acc, node| {
                        let next = self
                            .to_owned()
                            .visit(
                                stream::iter(traversers.to_owned()).boxed(),
                                node.to_owned(),
                                request.clone(),
                            )
                            .boxed();

                        acc.then(|_| next).boxed()
                    })
                    .boxed()
            })
            .flatten()
            .boxed()
    }

    /// Visit a set of plan nodes in parallel.
    /// Execution of all child operations happens in parallel, however the parallel operation cannot
    /// complete until all child operations have completed.
    ///
    /// With large chunk sizes there is the potential that a stalled operation will stall the
    /// entire pipeline.
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `nodes`: The pan nodes to execute in parallel.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    fn visit_parallel(
        self,
        traversers: TraverserStream,
        nodes: Vec<PlanNode>,
        request: Arc<GraphQLRequest>,
    ) -> EmptyFuture {
        traversers
            .collect::<Vec<Traverser>>()
            .map(move |traversers| {
                // We now have a chunk of traversers
                // For each parallel branch we send clones of those traversers through the pipeline
                let tasks = nodes
                    .iter()
                    .map(move |node| {
                        self.to_owned().visit(
                            stream::iter(traversers.to_owned()).boxed(),
                            node.to_owned(),
                            request.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                future::join_all(tasks).map(|_| ())
            })
            .flatten()
            .boxed()
    }

    /// Visit a flatten plan node.
    /// Given a traverser this will create a stream of traversers that match the path provided in
    /// the plan.
    ///
    /// For instance given:
    /// ```json
    /// {
    ///     'a': {
    ///         'b':[{'c':1}, {'c':2}]
    ///     }
    /// ```
    /// a traverser at path `a`
    /// and a plan path of `b/@/c`
    /// The traversers generated will be:
    /// `a/b/0/c' and `a/b/1/c'
    ///
    /// # Arguments
    ///
    /// * `traversers`: The stream of traversers to process.
    /// * `flatten`: The flatten plan node.
    ///
    /// returns Pin<Box<dyn Future<Output = ()> + Send>>
    ///
    fn visit_flatten(
        &self,
        traversers: TraverserStream,
        flatten: FlattenNode,
        request: Arc<GraphQLRequest>,
    ) -> EmptyFuture {
        let path = Path::parse(flatten.path.join("/"));
        let expanded = traversers
            .flat_map(move |traverser| traverser.stream_descendants(&path))
            .boxed();
        self.to_owned().visit(expanded, *flatten.node, request)
    }
}

impl GraphQLFetcher for FederatedGraph {
    fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream {
        let request = Arc::new(request);
        let clone = self.clone();

        self.clone()
            .plan(request.clone())
            .map(move |plan| match plan {
                Ok(QueryPlan { node: Some(root) }) => {
                    let mut start = Traverser::new(request.clone());

                    start.add_errors(&validate_services_against_plan(
                        clone.service_registry.to_owned(),
                        &root,
                    ));
                    start.add_errors(&validate_request_variables_against_plan(
                        request.to_owned(),
                        &root,
                    ));

                    // If we have any errors so far then let's abort the query
                    // Planning/validation/variables are candidates to abort.
                    if start.has_errors() {
                        return stream::iter(vec![start.to_primary()]).boxed();
                    }

                    clone
                        .visit(stream::iter(vec![start.to_owned()]).boxed(), root, request)
                        .map(move |_| stream::iter(vec![start.to_primary()]))
                        .flatten_stream()
                        .boxed()
                }
                Ok(_) => stream::empty().boxed(),
                Err(err) => stream::iter(vec![err.to_primary()]).boxed(),
            })
            .into_stream()
            .flatten()
            .boxed()
    }
}

impl From<QueryPlannerError> for FetchError {
    fn from(err: QueryPlannerError) -> Self {
        FetchError::ValidationPlanningError {
            reason: err.to_string(),
        }
    }
}

/// Given a vec of selections merge them into an array value.
///
/// # Arguments
///
/// * `selections`: The selections to merge.
///
/// returns: Value
///
fn construct_representations(selections: Vec<Value>) -> Value {
    Value::Array(selections.iter().map(|value| value.to_owned()).collect())
}

/// Get the list of traversers and corresponding selections for sending to a downstream service.
/// Any traverser that does not result in a selection will be dropped.
///
/// # Arguments
///
/// * `fetch`: The fetch node that defines the
/// * `traversers`: The vec of traversers to process.
///
/// returns: (Vec<Traverser>, Vec<Value>)
///
fn traversers_with_selections(
    requires: &Option<SelectionSet>,
    mut traversers: Vec<Traverser>,
) -> (Vec<Traverser>, Vec<Value>) {
    traversers
        .iter_mut()
        .map(|traverser| (traverser.to_owned(), traverser.select(requires)))
        .filter_map(|(mut traverser, selection)| match selection {
            Ok(Some(x)) => Some((traverser, x)),
            Ok(None) => None,
            Err(err) => {
                traverser.add_error(&err);
                None
            }
        })
        .unzip()
}

/// Merge the results of a selection query with the originating traverser.
/// Each result is paired with the originating traverser before merging.
///
/// # Arguments
///
/// * `traversers`: The vec of traversers to merge with.
/// * `primary`: The response from the downstream server
///
/// returns: Vec<Traverser>
///
fn merge_response(traversers: &mut [Traverser], primary: GraphQLPrimaryResponse) {
    if let Some(Value::Array(array)) = primary.data.get("_entities") {
        traversers
            .iter()
            .zip(array.iter())
            .for_each(|(traverser, result)| {
                traverser.to_owned().merge(Some(result));
            });
    }
    //We may have some errors that relate to entities. Find them and add them to the appropriate
    //traverser
    for mut err in primary.errors {
        if err.path[0].eq(&PathElement::Key("_entities".to_string())) {
            if let PathElement::Index(index) = err.path[1] {
                err.path.splice(0..2, vec![]);
                traversers[index].add_graphql_error(&err);
            }
        } else {
            log::error!("Subquery had errors that did not map to entities.");
        }
    }
}

/// Recursively validate a query plan node making sure that all services are known before we go
/// for execution.
/// This simplifies processing later as we can always guarantee that services are configured for
/// the plan.  
///
/// # Arguments
///
/// * `plan`: The root query plan node to validate.
///
/// returns: Result<(), FetchError>
///
fn validate_services_against_plan(
    service_registry: Arc<dyn ServiceRegistry>,
    plan: &PlanNode,
) -> Vec<FetchError> {
    plan.service_usage()
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|service| !service_registry.has(service))
        .map(|service| FetchError::ValidationUnknownServiceError {
            service: service.to_string(),
        })
        .collect::<Vec<_>>()
}

fn validate_request_variables_against_plan(
    request: Arc<GraphQLRequest>,
    plan: &PlanNode,
) -> Vec<FetchError> {
    let required = plan.variable_usage().collect::<HashSet<_>>();
    let provided = request
        .variables
        .keys()
        .map(|x| x.as_str())
        .collect::<HashSet<_>>();
    required
        .difference(&provided)
        .map(|x| FetchError::ValidationMissingVariable {
            name: x.to_string(),
        })
        .collect::<Vec<_>>()
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::Entry;
    use std::collections::HashMap;

    use futures::prelude::*;
    use maplit::hashmap;
    use serde_json::json;
    use serde_json::to_string_pretty;

    use configuration::Configuration;
    use query_planner::harmonizer::HarmonizerQueryPlanner;

    use crate::http_service_registry::HttpServiceRegistry;
    use crate::http_subgraph::HttpSubgraphFetcher;
    use crate::json_utils::is_subset;

    use super::*;

    #[ctor::ctor]
    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    macro_rules! assert_federated_response {
        ($query:expr, $service_requests:expr $(,)?) => {
            let request = GraphQLRequest::builder()
                .query($query)
                .variables(Arc::new(
                    vec![
                        ("topProductsFirst".to_string(), 2.into()),
                        ("reviewsForAuthorAuthorId".to_string(), 1.into()),
                    ]
                    .into_iter()
                    .collect(),
                ))
                .build();
            let mut expected = query_node(request.clone());
            let (mut actual, registry) = query_rust(request.clone());

            let actual = actual.next().await.unwrap().primary();
            let expected = expected.next().await.unwrap().primary();
            log::debug!("{}", to_string_pretty(&actual).unwrap());
            log::debug!("{}", to_string_pretty(&expected).unwrap());

            // The current implementation does not cull extra properties that should not make is to the
            // output yet, so we check that the nodejs implementation returns a subset of the
            // output of the rust output.
            assert!(is_subset(
                &Value::Object(expected.data),
                &Value::Object(actual.data)
            ));
            assert_eq!(registry.totals(), $service_requests);
        };
    }

    #[tokio::test]
    async fn test_merge_response() {
        let mut traverser = Traverser::new(Arc::new(GraphQLRequest::builder().query("").build()));
        traverser.merge(Some(&json!({"arr":[{}, {}]})));

        let mut children = traverser
            .stream_descendants(&Path::parse("arr/@"))
            .collect::<Vec<_>>()
            .await;
        merge_response(
            &mut children,
            GraphQLPrimaryResponse {
                data: json!({
                    "_entities": [
                        {"prop1": "val1"},
                        {"prop1": "val2"},
                    ]
                })
                .as_object()
                .unwrap()
                .to_owned(),
                has_next: false,
                errors: vec![FetchError::MalformedResponse {
                    reason: "Something".to_string(),
                }
                .to_graphql_error(Some(Path::parse("_entities/1")))],
                extensions: Default::default(),
            },
        );

        assert_eq!(
            traverser.to_primary().primary(),
            GraphQLPrimaryResponse {
                data: json!({
                    "arr": [
                        {"prop1": "val1"},
                        {"prop1": "val2"},
                    ],
                })
                .as_object()
                .unwrap()
                .to_owned(),
                has_next: false,
                errors: vec![FetchError::MalformedResponse {
                    reason: "Something".to_string(),
                }
                .to_graphql_error(Some(Path::parse("arr/1")))],
                extensions: Default::default(),
            }
        );
    }

    #[tokio::test]
    async fn basic_request() {
        assert_federated_response!(
            r#"{ topProducts { name } }"#,
            hashmap! {
                "products".to_string()=>1,
            },
        );
    }

    #[tokio::test]
    async fn basic_composition() {
        assert_federated_response!(
            r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#,
            hashmap! {
                "products".to_string()=>2,
                "reviews".to_string()=>1,
                "accounts".to_string()=>1,
            },
        );
    }

    #[tokio::test]
    async fn basic_mutation() {
        assert_federated_response!(
            r#"mutation {
              createProduct(upc:"8", name:"Bob") {
                upc
                name
                reviews {
                  body
                }
              }
              createReview(upc: "8", id:"100", body: "Bif"){
                id
                body
              }
            }"#,
            hashmap! {
                "products".to_string()=>1,
                "reviews".to_string()=>2,
            },
        );
    }

    #[tokio::test]
    async fn variables() {
        init();
        assert_federated_response!(
            r#"
            query ExampleQuery($topProductsFirst: Int, $reviewsForAuthorAuthorId: ID!) {
                topProducts(first: $topProductsFirst) {
                    name
                    reviewsForAuthor(authorID: $reviewsForAuthorAuthorId) {
                        body
                        author {
                            id
                            name
                        }
                    }
                }
            }
            "#,
            hashmap! {
                "products".to_string()=>1,
                "reviews".to_string()=>1,
                "accounts".to_string()=>1,
            },
        );
    }

    #[tokio::test]
    async fn missing_variables() {
        let request = GraphQLRequest::builder()
            .query(
                r#"
                query ExampleQuery($missingVariable: Int, $yetAnotherMissingVariable: ID!) {
                    topProducts(first: $missingVariable) {
                        name
                        reviewsForAuthor(authorID: $yetAnotherMissingVariable) {
                            body
                        }
                    }
                }
                "#,
            )
            .build();
        let (response, _) = query_rust(request.clone());
        let data = response
            .flat_map(|x| stream::iter(x.primary().errors))
            .collect::<Vec<_>>()
            .await;
        let expected = vec![
            FetchError::ValidationMissingVariable {
                name: "yetAnotherMissingVariable".to_string(),
            }
            .to_graphql_error(None),
            FetchError::ValidationMissingVariable {
                name: "missingVariable".to_string(),
            }
            .to_graphql_error(None),
        ];
        assert!(data.iter().all(|x| expected.contains(x)));
    }

    fn query_node(request: GraphQLRequest) -> GraphQLResponseStream {
        let nodejs_impl =
            HttpSubgraphFetcher::new("federated".into(), "http://localhost:4000/graphql".into());
        nodejs_impl.stream(request)
    }

    fn query_rust(
        request: GraphQLRequest,
    ) -> (GraphQLResponseStream, Arc<CountingServiceRegistry>) {
        let planner =
            HarmonizerQueryPlanner::new(include_str!("testdata/supergraph.graphql").into());
        let config =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let registry = Arc::new(CountingServiceRegistry::new(HttpServiceRegistry::new(
            &config,
        )));
        let federated = FederatedGraph::new(planner, registry.to_owned());
        (federated.stream(request), registry)
    }

    #[derive(Debug)]
    struct CountingServiceRegistry {
        counts: Arc<parking_lot::Mutex<HashMap<String, usize>>>,
        delegate: HttpServiceRegistry,
    }

    impl CountingServiceRegistry {
        fn new(delegate: HttpServiceRegistry) -> CountingServiceRegistry {
            CountingServiceRegistry {
                counts: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                delegate,
            }
        }

        fn totals(&self) -> HashMap<String, usize> {
            self.counts.lock().clone()
        }
    }

    impl ServiceRegistry for CountingServiceRegistry {
        fn get(&self, service: &str) -> Option<&dyn GraphQLFetcher> {
            let mut counts = self.counts.lock();
            match counts.entry(service.to_owned()) {
                Entry::Occupied(mut e) => {
                    *e.get_mut() += 1;
                }
                Entry::Vacant(e) => {
                    e.insert(1);
                }
            }
            self.delegate.get(service)
        }

        fn has(&self, service: &str) -> bool {
            self.delegate.has(service)
        }
    }
}
