use crate::prelude::graphql::*;
use derivative::Derivative;
use futures::lock::Mutex;
use futures::prelude::*;
use std::collections::HashSet;
use std::pin::Pin;
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
    request: Arc<Request>,
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

/// A federated graph that can be queried.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct FederatedGraph {
    #[derivative(Debug = "ignore")]
    query_planner: Box<dyn QueryPlanner>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
}

impl FederatedGraph {
    /// Create a `FederatedGraph` instance used to execute a GraphQL query.
    pub fn new(
        query_planner: Box<dyn QueryPlanner>,
        service_registry: Arc<dyn ServiceRegistry>,
        schema: Arc<Schema>,
    ) -> Self {
        Self {
            query_planner,
            service_registry,
            schema,
        }
    }
}

impl Fetcher for FederatedGraph {
    fn stream(&self, request: Request) -> ResponseStream {
        let span = tracing::info_span!("federated_query");
        let _guard = span.enter();

        log::trace!("Request received:\n{:#?}", request);

        let plan = {
            let span = tracing::trace_span!("query_planning");
            let _guard = span.enter();

            match self.query_planner.get(
                request.query.to_owned(),
                request.operation_name.to_owned(),
                Default::default(),
            ) {
                Ok(QueryPlan { node: Some(root) }) => root,
                Ok(_) => return stream::empty().boxed(),
                Err(err) => {
                    return stream::iter(vec![FetchError::from(err).to_response(true)]).boxed()
                }
            }
        };
        let service_registry = Arc::clone(&self.service_registry);
        let schema = Arc::clone(&self.schema);
        let request = Arc::new(request);

        let mut early_errors = Vec::new();

        for err in validate_services_against_plan(Arc::clone(&service_registry), &plan) {
            early_errors.push(err.to_graphql_error(None));
        }

        for err in validate_request_variables_against_plan(Arc::clone(&request), &plan) {
            early_errors.push(err.to_graphql_error(None));
        }

        // If we have any errors so far then let's abort the query
        // Planning/validation/variables are candidates to abort.
        if !early_errors.is_empty() {
            return stream::once(async move { Response::builder().errors(early_errors).build() })
                .boxed();
        }

        stream::once(
            async move {
                let response = Arc::new(Mutex::new(Response::builder().build()));
                let root = Path::empty();

                execute(
                    Arc::clone(&response),
                    &root,
                    &plan,
                    request,
                    Arc::clone(&service_registry),
                    Arc::clone(&schema),
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

fn execute<'a>(
    response: Arc<Mutex<Response>>,
    current_dir: &'a Path,
    plan: &'a PlanNode,
    request: Arc<Request>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    let span = tracing::info_span!("execution");
    let _guard = span.enter();

    Box::pin(async move {
        log::trace!("Executing plan:\n{:#?}", plan);

        match plan {
            PlanNode::Sequence { nodes } => {
                for node in nodes {
                    execute(
                        Arc::clone(&response),
                        current_dir,
                        node,
                        Arc::clone(&request),
                        Arc::clone(&service_registry),
                        Arc::clone(&schema),
                    )
                    .instrument(tracing::trace_span!("execute-sequence"))
                    .await;
                }
            }
            PlanNode::Parallel { nodes } => {
                future::join_all(nodes.iter().map(|plan| {
                    execute(
                        Arc::clone(&response),
                        current_dir,
                        plan,
                        Arc::clone(&request),
                        Arc::clone(&service_registry),
                        Arc::clone(&schema),
                    )
                }))
                .instrument(tracing::trace_span!("execute-parallel"))
                .await;
            }
            PlanNode::Fetch(info) => {
                match fetch_node(
                    Arc::clone(&response),
                    current_dir,
                    info,
                    Arc::clone(&request),
                    Arc::clone(&service_registry),
                    Arc::clone(&schema),
                )
                .instrument(tracing::trace_span!("execute-fetch"))
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
                    Arc::clone(&response),
                    // a path can go over multiple json node!
                    &current_dir,
                    node,
                    Arc::clone(&request),
                    Arc::clone(&service_registry),
                    Arc::clone(&schema),
                )
                .instrument(tracing::trace_span!("execute-flatten"))
                .await;
            }
        }
    })
}

async fn fetch_node<'a>(
    response: Arc<Mutex<Response>>,
    current_dir: &'a Path,
    FetchNode {
        variable_usages,
        requires,
        operation,
        service_name,
    }: &'a FetchNode,
    request: Arc<Request>,
    service_registry: Arc<dyn ServiceRegistry>,
    schema: Arc<Schema>,
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
            let representations = response.select(current_dir, requires, &schema)?;
            variables.insert("representations".into(), representations);
        }

        let (res, _tail) = fetcher
            .stream(
                Request::builder()
                    .query(operation)
                    .variables(variables)
                    .build(),
            )
            .into_future()
            .instrument(tracing::trace_span!("fetch-selections"))
            .await;

        match res {
            Some(response) if !response.is_primary() => {
                Err(FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_owned(),
                })
            }
            Some(Response { data, .. }) => {
                if let Some(entities) = data.get("_entities") {
                    log::trace!(
                        "Received entities: {}",
                        serde_json::to_string(entities).unwrap(),
                    );
                    if let Some(array) = entities.as_array() {
                        let mut response = response
                            .lock()
                            .instrument(tracing::trace_span!("response-lock-wait"))
                            .await;

                        let span = tracing::trace_span!("response-insert");
                        let _guard = span.enter();
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
                Request::builder()
                    .query(operation.clone())
                    .variables(Arc::clone(&variables))
                    .build(),
            )
            .into_future()
            .instrument(tracing::trace_span!("fetch-selections"))
            .await;

        match res {
            Some(response) if !response.is_primary() => {
                Err(FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_owned(),
                })
            }
            Some(Response {
                data, mut errors, ..
            }) => {
                let mut response = response
                    .lock()
                    .instrument(tracing::trace_span!("response-lock-wait"))
                    .await;

                let span = tracing::trace_span!("response-insert");
                let _guard = span.enter();
                response.append_errors(&mut errors);
                response.insert_data(current_dir, data)?;

                Ok(())
            }
            None => Err(FetchError::SubrequestNoResponse {
                service: service_name.to_string(),
            }),
        }
    }
}
