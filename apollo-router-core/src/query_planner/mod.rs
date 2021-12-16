mod caching_query_planner;
mod router_bridge_query_planner;
mod selection;

use crate::prelude::graphql::*;
pub use caching_query_planner::*;
use futures::lock::Mutex;
use futures::prelude::*;
pub use router_bridge_query_planner::*;
use selection::Selection;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::Instrument;

/// Query planning options.
#[derive(Clone, Eq, Hash, PartialEq, Debug, Default)]
pub struct QueryPlanOptions {}

#[derive(Debug)]
pub struct QueryPlan {
    root: PlanNode,
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
    Fetch(FetchNode),

    /// Merge the current resultset with the response.
    Flatten(FlattenNode),
}

impl QueryPlan {
    /// Validate the entire request for variables and services used.
    #[tracing::instrument(skip_all, name = "validation", level = "debug")]
    pub fn validate_request(
        &self,
        request: &Request,
        service_registry: Arc<dyn ServiceRegistry>,
    ) -> Result<(), Response> {
        let mut early_errors = Vec::new();
        for err in self
            .root
            .validate_services_against_plan(Arc::clone(&service_registry))
        {
            early_errors.push(err.to_graphql_error(None));
        }

        for err in self.root.validate_request_variables_against_plan(request) {
            early_errors.push(err.to_graphql_error(None));
        }

        if !early_errors.is_empty() {
            Err(Response::builder().errors(early_errors).build())
        } else {
            Ok(())
        }
    }

    /// Execute the plan and return a [`Response`].
    pub async fn execute(
        &self,
        request: Arc<Request>,
        service_registry: Arc<dyn ServiceRegistry>,
        schema: Arc<Schema>,
    ) -> Response {
        let response = Arc::new(Mutex::new(Response::builder().build()));
        let root = Path::empty();

        self.root
            .execute_recursively(
                Arc::clone(&response),
                &root,
                Arc::clone(&request),
                Arc::clone(&service_registry),
                Arc::clone(&schema),
            )
            .await;

        // TODO: this is not great but there is no other way
        Arc::try_unwrap(response)
            .expect("todo: how to prove?")
            .into_inner()
    }
}

impl PlanNode {
    fn execute_recursively<'a>(
        &'a self,
        response: Arc<Mutex<Response>>,
        current_dir: &'a Path,
        request: Arc<Request>,
        service_registry: Arc<dyn ServiceRegistry>,
        schema: Arc<Schema>,
    ) -> future::BoxFuture<()> {
        Box::pin(async move {
            tracing::trace!("Executing plan:\n{:#?}", self);

            match self {
                PlanNode::Sequence { nodes } => {
                    for node in nodes {
                        node.execute_recursively(
                            Arc::clone(&response),
                            current_dir,
                            Arc::clone(&request),
                            Arc::clone(&service_registry),
                            Arc::clone(&schema),
                        )
                        .instrument(tracing::info_span!("sequence"))
                        .await;
                    }
                }
                PlanNode::Parallel { nodes } => {
                    future::join_all(nodes.iter().map(|plan| {
                        plan.execute_recursively(
                            Arc::clone(&response),
                            current_dir,
                            Arc::clone(&request),
                            Arc::clone(&service_registry),
                            Arc::clone(&schema),
                        )
                    }))
                    .instrument(tracing::info_span!("parallel"))
                    .await;
                }
                PlanNode::Fetch(fetch_node) => {
                    match fetch_node
                        .fetch_node(
                            Arc::clone(&response),
                            current_dir,
                            Arc::clone(&request),
                            Arc::clone(&service_registry),
                            Arc::clone(&schema),
                        )
                        .instrument(tracing::info_span!("fetch"))
                        .await
                    {
                        Ok(()) => {}
                        Err(err) => {
                            failfast_error!("Fetch error: {}", err);
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
                    node.execute_recursively(
                        Arc::clone(&response),
                        // a path can go over multiple json node!
                        &current_dir,
                        Arc::clone(&request),
                        Arc::clone(&service_registry),
                        Arc::clone(&schema),
                    )
                    .instrument(tracing::trace_span!("flatten"))
                    .await;
                }
            }
        })
    }

    /// Retrieves all the variables used across all plan nodes.
    ///
    /// Note that duplicates are not filtered.
    fn variable_usage<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match self {
            Self::Sequence { nodes } | Self::Parallel { nodes } => {
                Box::new(nodes.iter().flat_map(|x| x.variable_usage()))
            }
            Self::Fetch(fetch) => Box::new(fetch.variable_usages.iter().map(|x| x.as_str())),
            Self::Flatten(flatten) => Box::new(flatten.node.variable_usage()),
        }
    }

    /// Retrieves all the services used across all plan nodes.
    ///
    /// Note that duplicates are not filtered.
    fn service_usage<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match self {
            Self::Sequence { nodes } | Self::Parallel { nodes } => {
                Box::new(nodes.iter().flat_map(|x| x.service_usage()))
            }
            Self::Fetch(fetch) => Box::new(vec![fetch.service_name.as_str()].into_iter()),
            Self::Flatten(flatten) => Box::new(flatten.node.service_usage()),
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
        service_registry: Arc<dyn ServiceRegistry>,
    ) -> Vec<FetchError> {
        self.service_usage()
            .filter(|service| !service_registry.has(service))
            .collect::<HashSet<_>>()
            .into_iter()
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
    fn validate_request_variables_against_plan(&self, request: &Request) -> Vec<FetchError> {
        let required = self.variable_usage().collect::<HashSet<_>>();
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
}

/// A fetch node.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchNode {
    /// The name of the service or subgraph that the fetch is querying.
    service_name: String,

    /// The data that is required for the subgraph fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    requires: Option<Vec<Selection>>,

    /// The variables that are used for the subgraph fetch.
    variable_usages: Vec<String>,

    /// The GraphQL subquery that is used for the fetch.
    operation: String,
}

impl FetchNode {
    async fn fetch_node<'a>(
        &'a self,
        response: Arc<Mutex<Response>>,
        current_dir: &'a Path,
        request: Arc<Request>,
        service_registry: Arc<dyn ServiceRegistry>,
        schema: Arc<Schema>,
    ) -> Result<(), FetchError> {
        let FetchNode {
            variable_usages,
            requires,
            operation,
            service_name,
        } = self;

        let query_span = tracing::info_span!("subfetch", service = service_name.as_str());

        if let Some(requires) = requires {
            // We already checked that the service exists during planning
            let fetcher = service_registry.get(service_name).unwrap();

            let mut variables = Object::with_capacity(1 + variable_usages.len());
            variables.extend(variable_usages.iter().filter_map(|key| {
                request
                    .variables
                    .get(key)
                    .map(|value| (key.clone(), value.clone()))
            }));

            {
                let response = response.lock().await;
                tracing::trace!(
                    "Creating representations at path '{}' for selections={:?} using data={}",
                    current_dir,
                    requires,
                    serde_json::to_string(&response.data).unwrap(),
                );
                let representations = selection::select(&response, current_dir, requires, &schema)?;
                variables.insert("representations".into(), representations);
            }

            let (res, _tail) = fetcher
                .stream(
                    Request::builder()
                        .query(operation)
                        .variables(Arc::new(variables))
                        .build(),
                )
                .await
                .into_future()
                .instrument(query_span)
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
                    // we have to nest conditions and do early returns here
                    // because we need to take ownership of the inner value
                    if let Value::Object(mut map) = data {
                        if let Some(entities) = map.remove("_entities") {
                            tracing::trace!(
                                "Received entities: {}",
                                serde_json::to_string(&entities).unwrap(),
                            );

                            if let Value::Array(array) = entities {
                                let mut response = response
                                    .lock()
                                    .instrument(tracing::trace_span!("response_lock_wait"))
                                    .await;

                                let span = tracing::trace_span!("response_insert");
                                let _guard = span.enter();
                                for (i, entity) in array.into_iter().enumerate() {
                                    response.insert_data(
                                        &current_dir.join(Path::from(i.to_string())),
                                        entity,
                                    )?;
                                }

                                return Ok(());
                            } else {
                                return Err(FetchError::ExecutionInvalidContent {
                                    reason: "Received invalid type for key `_entities`!"
                                        .to_string(),
                                });
                            }
                        }
                    }

                    let mut response = response
                        .lock()
                        .instrument(tracing::trace_span!("response_lock_wait"))
                        .await;

                    response.append_errors(&mut errors);
                    Err(FetchError::ExecutionInvalidContent {
                        reason: "Missing key `_entities`!".to_string(),
                    })
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
                            .map(|value| (key.clone(), value.clone()))
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
                .await
                .into_future()
                .instrument(query_span)
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
                        .instrument(tracing::trace_span!("response_lock_wait"))
                        .await;

                    let span = tracing::trace_span!("response_insert");
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

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! test_query_plan {
        () => {
            include_str!("testdata/query_plan.json")
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

    #[test]
    fn variable_usage() {
        assert_eq!(
            serde_json::from_str::<PlanNode>(test_query_plan!())
                .unwrap()
                .variable_usage()
                .collect::<Vec<_>>(),
            vec!["test_variable"]
        );
    }
}
