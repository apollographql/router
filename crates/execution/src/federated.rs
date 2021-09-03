use crate::{
    FetchError, GraphQLFetcher, GraphQLRequest, GraphQLResponse, GraphQLResponseStream,
    ServiceRegistry,
};
use apollo_json_ext::prelude::*;
use async_recursion::async_recursion;
use derivative::Derivative;
use futures::lock::Mutex;
use futures::prelude::*;
use query_planner::model::{FetchNode, FlattenNode, PlanNode, QueryPlan};
use query_planner::QueryPlanner;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::Instrument;
use tracing_futures::WithSubscriber;

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

/// Recursively validate a query plan node making sure that all variable usages are known before we
/// go for execution.
///
/// This simplifies processing later as we can always guarantee that the variable usages are
/// available for the plan.
///
/// # Arguments
///
///  *   `plan`: The root query plan node to validate.
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

#[derive(Derivative)]
#[derivative(Debug)]
pub struct FederatedGraph {
    #[derivative(Debug = "ignore")]
    query_planner: Box<dyn QueryPlanner>,
    service_registry: Arc<dyn ServiceRegistry>,
}

impl FederatedGraph {
    /// Create a `FederatedGraph` instance used to execute a GraphQL query.
    pub fn new(
        query_planner: Box<dyn QueryPlanner>,
        service_registry: Arc<dyn ServiceRegistry>,
    ) -> Self {
        Self {
            query_planner,
            service_registry,
        }
    }
}

impl GraphQLFetcher for FederatedGraph {
    fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream {
        let span = tracing::info_span!("federated_query");
        let _guard = span.enter();

        log::trace!("Request received:\n{:#?}", request);

        let plan = match self.query_planner.get(
            request.query.to_owned(),
            request.operation_name.to_owned(),
            Default::default(),
        ) {
            Ok(QueryPlan { node: Some(root) }) => root,
            Ok(_) => return stream::empty().boxed(),
            Err(err) => return stream::iter(vec![FetchError::from(err).to_response(true)]).boxed(),
        };
        let service_registry = self.service_registry.clone();
        let request = Arc::new(request);

        let mut early_errors = Vec::new();

        for err in validate_services_against_plan(service_registry.clone(), &plan) {
            early_errors.push(err.to_graphql_error(None));
        }

        for err in validate_request_variables_against_plan(request.clone(), &plan) {
            early_errors.push(err.to_graphql_error(None));
        }

        // If we have any errors so far then let's abort the query
        // Planning/validation/variables are candidates to abort.
        if !early_errors.is_empty() {
            return stream::once(
                async move { GraphQLResponse::builder().errors(early_errors).build() },
            )
            .boxed();
        }

        stream::once(
            async move {
                let response = Arc::new(Mutex::new(GraphQLResponse::builder().build()));
                let root = Path::empty();

                execute(
                    response.clone(),
                    &root,
                    &plan,
                    request,
                    service_registry.clone(),
                )
                .await;

                // TODO: this is not great but there is no other way
                Arc::try_unwrap(response)
                    .expect("todo: how to prove?")
                    .into_inner()
            }
            .in_current_span()
            .with_current_subscriber(),
        )
        .boxed()
    }
}

#[async_recursion]
async fn execute(
    response: Arc<Mutex<GraphQLResponse>>,
    current_dir: &Path,
    plan: &PlanNode,
    request: Arc<GraphQLRequest>,
    service_registry: Arc<dyn ServiceRegistry>,
) {
    let span = tracing::info_span!("execution");
    let _guard = span.enter();

    log::trace!("Executing plan:\n{:#?}", plan);

    match plan {
        PlanNode::Sequence { nodes } => {
            for node in nodes {
                execute(
                    response.clone(),
                    current_dir,
                    node,
                    request.clone(),
                    service_registry.clone(),
                )
                .await;
            }
        }
        PlanNode::Parallel { nodes } => {
            future::join_all(nodes.iter().map(|plan| {
                execute(
                    response.clone(),
                    current_dir,
                    plan,
                    request.clone(),
                    service_registry.clone(),
                )
            }))
            .await;
        }
        PlanNode::Fetch(info) => {
            match fetch_node(
                response.clone(),
                current_dir,
                info,
                request.clone(),
                service_registry.clone(),
            )
            .await
            {
                Ok(()) => {
                    log::trace!(
                        "New data:\n{}",
                        serde_json::to_string_pretty(&response.lock().await.data).unwrap(),
                    );
                }
                Err(err) => {
                    debug_assert!(false, "{}", err);
                    log::error!("Fetch error: {}", err);
                    response
                        .lock()
                        .await
                        .errors
                        .push(err.to_graphql_error(Some(current_dir.to_owned())));
                }
            }
        }
        PlanNode::Flatten(FlattenNode { path, node }) => {
            // this is the only command that actually changes the "current dir"
            let current_dir = current_dir.join(path);
            execute(
                response.clone(),
                // a path can go over multiple json node!
                &current_dir,
                node,
                request.clone(),
                service_registry.clone(),
            )
            .await;
        }
    }
}

async fn fetch_node<'a>(
    response: Arc<Mutex<GraphQLResponse>>,
    current_dir: &'a Path,
    FetchNode {
        variable_usages,
        requires,
        operation,
        service_name,
    }: &'a FetchNode,
    request: Arc<GraphQLRequest>,
    service_registry: Arc<dyn ServiceRegistry>,
) -> Result<(), FetchError> {
    if let Some(requires) = requires {
        // We already checked that the service exists during planning
        let fetcher = service_registry.get(service_name).unwrap();

        let mut variables = Object::with_capacity(1 + variable_usages.len());
        variables.extend(variable_usages.iter().filter_map(|key| {
            request
                .variables
                .get(key)
                .map(|value| (key.to_owned(), value.to_owned()))
        }));

        {
            let response = response.lock().await;
            log::trace!(
                "Creating representations at path '{}' for selections={:?} using data={}",
                current_dir,
                requires,
                serde_json::to_string(&response.data).unwrap(),
            );
            let representations = response.select(current_dir, requires)?;
            variables.insert("representations".into(), representations);
        }

        let (res, _tail) = fetcher
            .stream(
                GraphQLRequest::builder()
                    .query(operation)
                    .variables(variables)
                    .build(),
            )
            .into_future()
            .await;

        match res {
            Some(response) if !response.is_primary() => {
                Err(FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_owned(),
                })
            }
            Some(GraphQLResponse { data, .. }) => {
                if let Some(entities) = data.get("_entities") {
                    log::trace!(
                        "Received entities: {}",
                        serde_json::to_string(entities).unwrap(),
                    );
                    if let Some(array) = entities.as_array() {
                        let mut response = response.lock().await;

                        for (i, entity) in array.iter().enumerate() {
                            response.insert_data(
                                &current_dir.join(Path::from(i.to_string())),
                                entity.to_owned(),
                            )?;
                        }

                        Ok(())
                    } else {
                        Err(FetchError::ExecutionInvalidContent {
                            reason: "Received invalid type for key `_entities`!".to_string(),
                        })
                    }
                } else {
                    Err(FetchError::ExecutionInvalidContent {
                        reason: "Missing key `_entities`!".to_string(),
                    })
                }
            }
            None => Err(FetchError::SubrequestNoResponse {
                service: service_name.to_string(),
            }),
        }
    } else {
        let variables = Arc::new(
            variable_usages
                .iter()
                .filter_map(|key| {
                    request
                        .variables
                        .get(key)
                        .map(|value| (key.to_owned(), value.to_owned()))
                })
                .collect::<Object>(),
        );

        // We already validated that the service exists during planning
        let fetcher = service_registry.get(service_name).unwrap();

        let (res, _tail) = fetcher
            .stream(
                GraphQLRequest::builder()
                    .query(operation.clone())
                    .variables(variables.clone())
                    .build(),
            )
            .into_future()
            .await;

        match res {
            Some(response) if !response.is_primary() => {
                Err(FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_owned(),
                })
            }
            Some(GraphQLResponse { data, .. }) => {
                response.lock().await.insert_data(current_dir, data)?;
                Ok(())
            }
            None => Err(FetchError::SubrequestNoResponse {
                service: service_name.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_service_registry::HttpServiceRegistry;
    use crate::http_subgraph::HttpSubgraphFetcher;
    use configuration::Configuration;
    use maplit::hashmap;
    use query_planner::harmonizer::HarmonizerQueryPlanner;
    use serde_json::to_string_pretty;
    use std::collections::hash_map::Entry;
    use std::collections::HashMap;

    #[ctor::ctor]
    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    macro_rules! assert_federated_response {
        ($query:expr, $service_requests:expr $(,)?) => {
            init();
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
            let (mut actual, registry) = query_rust(request.clone());
            let mut expected = query_node(request.clone());

            let actual = actual.next().await.unwrap();
            let expected = expected.next().await.unwrap();
            eprintln!("actual: {}", to_string_pretty(&actual).unwrap());
            eprintln!("expected: {}", to_string_pretty(&expected).unwrap());

            // The current implementation does not cull extra properties that should not make is to the
            // output yet, so we check that the nodejs implementation returns a subset of the
            // output of the rust output.
            assert!(expected.data.is_subset(&actual.data));
            assert_eq!(registry.totals(), $service_requests);
        };
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
            .flat_map(|x| stream::iter(x.errors))
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
        let federated = FederatedGraph::new(Box::new(planner), registry.clone());
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
