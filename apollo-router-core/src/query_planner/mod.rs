mod caching_query_planner;
mod router_bridge_query_planner;
mod selection;

use crate::prelude::graphql::*;
use crate::query_planner::selection::{select_object, select_values};
pub use caching_query_planner::*;
use futures::lock::Mutex;
use futures::prelude::*;
use futures::stream::FuturesUnordered;
pub use router_bridge_query_planner::*;
use selection::Selection;
use serde::Deserialize;
use serde_json::Map;
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
    #[tracing::instrument(name = "validation", level = "debug")]
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

        println!("WILL EXECUTE\n*********");
        let (value, errors) = self
            .root
            .execute_recursively(
                Arc::clone(&response),
                &root,
                Arc::clone(&request),
                Arc::clone(&service_registry),
                Arc::clone(&schema),
                &Value::default(),
            )
            .await;

        println!(
            "res1:{}\nres2:{}",
            serde_json::to_string_pretty(&response.lock().await.data).unwrap(),
            serde_json::to_string_pretty(&value).unwrap()
        );
        assert!(response.lock().await.data.eq_and_ordered(&value));
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
        parent_value: &'a Value,
    ) -> future::BoxFuture<(Value, Vec<Error>)> {
        Box::pin(async move {
            tracing::trace!("Executing plan:\n{:#?}", self);
            let mut value = Value::default();
            let mut errors = Vec::new();

            match self {
                PlanNode::Sequence { nodes } => {
                    println!("{} Sequence", current_dir);
                    for node in nodes {
                        let (v, err) = node
                            .execute_recursively(
                                Arc::clone(&response),
                                current_dir,
                                Arc::clone(&request),
                                Arc::clone(&service_registry),
                                Arc::clone(&schema),
                                &value,
                            )
                            .instrument(tracing::info_span!("sequence"))
                            .await;
                        println!("SEQUENCE will merge\n{:?}\nwith\n{:?}", value, v);
                        value.deep_merge(v);
                        println!("\nSEQUENCE after merge: {:?}", value);
                        println!("response data is {:?}", response.lock().await.data);
                        errors.extend(err.into_iter());
                    }
                }
                PlanNode::Parallel { nodes } => {
                    println!("{} Parallel", current_dir);

                    async {
                        let mut resv = Value::default();

                        {
                            let mut stream: FuturesUnordered<_> = nodes
                                .iter()
                                .map(|plan| {
                                    plan.execute_recursively(
                                        Arc::clone(&response),
                                        current_dir,
                                        Arc::clone(&request),
                                        Arc::clone(&service_registry),
                                        Arc::clone(&schema),
                                        &value,
                                    )
                                })
                                .collect();

                            while let Some((v, err)) = stream.next().await {
                                println!("PARALLEL MERGING {:?}\nwith {:?}", resv, v);
                                resv.deep_merge(v);
                                //FIXME errors
                            }
                        }

                        value.deep_merge(resv);
                    }
                    .instrument(tracing::info_span!("parallel"))
                    .await;
                }
                PlanNode::Flatten(FlattenNode { path, node }) => {
                    println!(
                        "\n\n{} FLATTEN: {} parent {:?}",
                        current_dir, path, parent_value
                    );

                    let (v, err) = node
                        .execute_recursively(
                            Arc::clone(&response),
                            // this is the only command that actually changes the "current dir"
                            &path,
                            Arc::clone(&request),
                            Arc::clone(&service_registry),
                            Arc::clone(&schema),
                            &parent_value,
                        )
                        .instrument(tracing::trace_span!("flatten"))
                        .await;

                    let m = Map::new();
                    println!("FLATTEN will try to insert at {}: {:?}", path, v);
                    println!("current response is {:?}", response.lock().await.data);
                    //value.insert_data(path, v).unwrap();
                    value = Value::from_path(current_dir, v);
                    println!("FLATTEN value is now: {:?}", value);
                    errors.extend(err.into_iter());
                }
                PlanNode::Fetch(fetch_node) => {
                    println!(
                        "==============\n{} | {:?} FETCH({}) parent = {:?}",
                        current_dir, current_dir, fetch_node.service_name, parent_value
                    );
                    let (variables, paths) = {
                        let mut response = response.lock().await;
                        match fetch_node.make_variables(
                            parent_value,
                            current_dir,
                            &request,
                            &schema,
                        ) {
                            Ok(v) => v,
                            Err(err) => {
                                failfast_error!("Fetch error: {}", err);
                                response
                                    .errors
                                    .push(err.to_graphql_error(Some(current_dir.to_owned())));
                                errors.push(err.to_graphql_error(Some(current_dir.to_owned())));
                                return (value, errors);
                            }
                        }
                    };
                    println!("FETCH variables: {:?}", variables);
                    match fetch_node
                        .fetch_node(Arc::clone(&service_registry), variables)
                        .instrument(tracing::info_span!("fetch"))
                        .await
                    {
                        Ok(mut subgraph_response) => {
                            let mut response = response.lock().await;
                            response.append_errors(&mut subgraph_response.errors);
                            println!("FETCH sub response: {:?}", subgraph_response);

                            match fetch_node.merge_response(
                                &mut response.data,
                                current_dir,
                                subgraph_response.clone(),
                            ) {
                                Ok(()) => {}
                                Err(err) => {
                                    failfast_error!("Fetch error: {}", err);
                                    response
                                        .errors
                                        .push(err.to_graphql_error(Some(current_dir.to_owned())));

                                    errors.push(err.to_graphql_error(Some(current_dir.to_owned())));
                                    return (value, errors);
                                }
                            }

                            value = fetch_node
                                .response_at_path(current_dir, paths, subgraph_response.clone())
                                .unwrap();
                            println!("FETCH value after merge: {:?}\n==============\n", value);
                        }
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
            }

            (value, errors)
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
            .as_ref()
            .map(|v| v.keys().map(|x| x.as_str()).collect::<HashSet<_>>())
            .unwrap_or_default();
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
    fn make_variables<'a>(
        &'a self,
        data: &Value,
        current_dir: &'a Path,
        request: &Arc<Request>,
        schema: &Arc<Schema>,
    ) -> Result<(Map<String, Value>, Option<Vec<Path>>), FetchError> {
        if let Some(requires) = &self.requires {
            let mut variables = Object::with_capacity(1 + self.variable_usages.len());
            variables.extend(self.variable_usages.iter().filter_map(|key| {
                request.variables.as_ref().map(|v| {
                    v.get(key)
                        .map(|value| (key.clone(), value.clone()))
                        .unwrap_or_default()
                })
            }));

            println!(
                "\nMAKE VARIABLES Creating representations at path '{}' for selections={:?} using data={}",
                current_dir,
                requires,
                serde_json::to_string(&data).unwrap(),
            );

            let values_and_paths = select_values(current_dir, data);
            let mut paths = Vec::new();
            let representations = Value::Array(
                values_and_paths
                    .into_iter()
                    .flat_map(|(path, value)| match (value, requires) {
                        (Value::Object(content), requires) => {
                            paths.push(path);
                            select_object(content, requires, schema).transpose()
                        }
                        (_, _) => Some(Err(FetchError::ExecutionInvalidContent {
                            reason: "not an object".to_string(),
                        })),
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
            );
            //let representations = selection::select_value(&data, current_dir, requires, &schema)?;
            variables.insert("representations".into(), representations);

            Ok((variables, Some(paths)))
        } else {
            Ok((
                self.variable_usages
                    .iter()
                    .filter_map(|key| {
                        request
                            .variables
                            .as_ref()
                            .map(|v| v.get(key).map(|value| (key.clone(), value.clone())))
                            .unwrap_or_default()
                    })
                    .collect::<Object>(),
                None,
            ))
        }
    }

    async fn fetch_node<'a>(
        &'a self,
        service_registry: Arc<dyn ServiceRegistry>,
        variables: Map<String, Value>,
    ) -> Result<Response, FetchError> {
        let FetchNode {
            operation,
            service_name,
            ..
        } = self;

        let query_span = tracing::info_span!("subfetch", service = service_name.as_str());

        // We already checked that the service exists during planning
        let fetcher = service_registry.get(service_name).unwrap();

        let (res, _tail) = fetcher
            .stream(
                Request::builder()
                    .query(operation)
                    .variables(Some(Arc::new(variables)))
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
            Some(subgraph_response) => Ok(subgraph_response),
            None => Err(FetchError::SubrequestNoResponse {
                service: service_name.to_string(),
            }),
        }
    }

    fn merge_response<'a>(
        &'a self,
        response_data: &mut Value,
        current_dir: &'a Path,
        subgraph_response: Response,
    ) -> Result<(), FetchError> {
        let Response { data, .. } = subgraph_response;

        if self.requires.is_some() {
            // we have to nest conditions and do early returns here
            // because we need to take ownership of the inner value
            if let Value::Object(mut map) = data {
                if let Some(entities) = map.remove("_entities") {
                    tracing::trace!(
                        "Received entities: {}",
                        serde_json::to_string(&entities).unwrap(),
                    );

                    if let Value::Array(array) = entities {
                        let span = tracing::trace_span!("response_insert");
                        let _guard = span.enter();
                        println!(
                            "\nMERGE inserting entity in global response: {}",
                            serde_json::to_string_pretty(&response_data).unwrap()
                        );
                        for (i, entity) in array.into_iter().enumerate() {
                            println!(
                                "MERGE insert entity at: {}: {:#?}",
                                current_dir.join(Path::from(i.to_string())),
                                serde_json::to_string(&entity).unwrap()
                            );
                            response_data.insert_data(
                                &current_dir.join(Path::from(i.to_string())),
                                entity,
                            )?;
                            println!(
                                "MERGE response is now {}",
                                serde_json::to_string(&response_data).unwrap()
                            );
                        }
                        println!("MERGE end\n");

                        return Ok(());
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
            let span = tracing::trace_span!("response_insert");
            let _guard = span.enter();

            response_data.insert_data(current_dir, data)?;

            Ok(())
        }
    }

    fn response_at_path<'a>(
        &'a self,
        current_dir: &'a Path,
        paths: Option<Vec<Path>>,
        subgraph_response: Response,
    ) -> Result<Value, FetchError> {
        let Response { data, .. } = subgraph_response;

        if self.requires.is_some() {
            // we have to nest conditions and do early returns here
            // because we need to take ownership of the inner value
            if let Value::Object(mut map) = data {
                if let Some(entities) = map.remove("_entities") {
                    tracing::info!(
                        "Received entities: {}",
                        serde_json::to_string(&entities).unwrap(),
                    );

                    if let Value::Array(array) = entities {
                        let span = tracing::trace_span!("response_insert");
                        let _guard = span.enter();

                        let mut value = Value::default();

                        let paths = paths.unwrap();
                        for (entity, path) in array.into_iter().zip(paths.into_iter()) {
                            println!(
                                "RESPONSE_AT_PATH {} for entity: {}",
                                path,
                                serde_json::to_string(&entity).unwrap()
                            );
                            let v = Value::from_path(&path, entity);
                            println!("RESPONSE_AT_PATH merging\n{:?}\nwith\n{:?}", value, v);
                            value.deep_merge(v);
                        }
                        println!(
                            "RESPONSE_at_path ({}): inserted in value: {:?}",
                            current_dir, value
                        );
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
            let span = tracing::trace_span!("response_insert");
            let _guard = span.enter();

            Ok(Value::from_path(current_dir, data))
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
