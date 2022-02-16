mod caching_query_planner;
mod router_bridge_query_planner;
mod selection;
use crate::prelude::graphql::*;
pub use caching_query_planner::*;
use fetch::OperationKind;
use futures::prelude::*;
pub use router_bridge_query_planner::*;
use serde::Deserialize;
use std::collections::HashSet;
use tracing::Instrument;
/// Query planning options.
#[derive(Clone, Eq, Hash, PartialEq, Debug, Default)]
pub struct QueryPlanOptions {}

#[derive(Debug)]
pub struct QueryPlan {
    pub(crate) root: PlanNode,
}

/// This default impl is useful for plugin_utils users
/// who will need `QueryPlan`s to work with the `QueryPlannerService` and the `ExecutionService`
impl Default for QueryPlan {
    fn default() -> Self {
        Self {
            root: PlanNode::Sequence { nodes: Vec::new() },
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
    pub fn contains_mutations(&self) -> bool {
        match self {
            Self::Sequence { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Parallel { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Fetch(fetch_node) => fetch_node.operation_kind() == &OperationKind::Mutation,
            Self::Flatten(_) => false,
        }
    }
}

impl QueryPlan {
    /// Validate the entire request for variables and services used.
    #[tracing::instrument(skip_all, name = "validate", level = "debug")]
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
        &'a self,
        context: &'a Context,
        service_registry: &'a ServiceRegistry,
        schema: &'a Schema,
    ) -> Response {
        let root = Path::empty();

        let (value, errors) = self
            .root
            .execute_recursively(&root, context, service_registry, schema, &Value::default())
            .await;

        Response::builder().data(value).errors(errors).build()
    }

    pub fn contains_mutations(&self) -> bool {
        self.root.contains_mutations()
    }
}

impl PlanNode {
    fn execute_recursively<'a>(
        &'a self,
        current_dir: &'a Path,
        context: &'a Context,
        service_registry: &'a ServiceRegistry,
        schema: &'a Schema,
        parent_value: &'a Value,
    ) -> future::BoxFuture<(Value, Vec<Error>)> {
        Box::pin(async move {
            tracing::trace!("Executing plan:\n{:#?}", self);
            let mut value;
            let mut errors = Vec::new();

            match self {
                PlanNode::Sequence { nodes } => {
                    value = parent_value.clone();
                    let span = tracing::info_span!("sequence");
                    for node in nodes {
                        let (v, err) = node
                            .execute_recursively(
                                current_dir,
                                context,
                                service_registry,
                                schema,
                                &value,
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

                    let span = tracing::info_span!("parallel");
                    let mut stream: stream::FuturesUnordered<_> = nodes
                        .iter()
                        .map(|plan| {
                            plan.execute_recursively(
                                current_dir,
                                context,
                                service_registry,
                                schema,
                                parent_value,
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
                            parent_value,
                        )
                        .instrument(tracing::trace_span!("flatten"))
                        .await;

                    value = v;
                    errors.extend(err.into_iter());
                }
                PlanNode::Fetch(fetch_node) => {
                    match fetch_node
                        .fetch_node(parent_value, current_dir, context, service_registry, schema)
                        .instrument(tracing::info_span!("fetch"))
                        .await
                    {
                        Ok(v) => value = v,
                        Err(err) => {
                            failfast_error!("Fetch error: {}", err);
                            errors.push(err.to_graphql_error(Some(current_dir.to_owned())));
                            value = Value::default();
                        }
                    }
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
    use super::selection::{select_object, Selection};
    use crate::prelude::graphql::*;
    use serde::Deserialize;
    use std::sync::Arc;
    use tower::ServiceExt;
    use tracing::{instrument, Instrument};

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

        /// The GraphQL operation kind that is used for the fetch.
        operation_kind: OperationKind,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum OperationKind {
        Query,
        Mutation,
        Subscription,
    }

    struct Variables {
        variables: Object,
        paths: Vec<Path>,
    }

    impl Variables {
        #[instrument(skip_all, level = "debug", name = "make_variables")]
        async fn new(
            requires: &[Selection],
            variable_usages: &[String],
            data: &Value,
            current_dir: &Path,
            context: &Context,
            schema: &Schema,
        ) -> Result<Variables, FetchError> {
            let body = context.request.body();
            if !requires.is_empty() {
                let mut variables = Object::with_capacity(1 + variable_usages.len());

                variables.extend(variable_usages.iter().filter_map(|key| {
                    body.variables
                        .get_key_value(key.as_str())
                        .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                }));

                let mut paths = Vec::new();
                let mut values = Vec::new();
                data.select_values_and_paths(current_dir, |path, value| {
                    match value {
                        Value::Object(content) => {
                            let object = select_object(content, requires, schema)?;
                            if let Some(value) = object {
                                paths.push(path);
                                values.push(value)
                            }
                        }
                        _ => {
                            return Err(FetchError::ExecutionInvalidContent {
                                reason: "not an object".to_string(),
                            })
                        }
                    }
                    Ok(())
                })?;
                let representations = Value::Array(values);

                variables.insert("representations", representations);

                Ok(Variables { variables, paths })
            } else {
                Ok(Variables {
                    variables: variable_usages
                        .iter()
                        .filter_map(|key| {
                            body.variables
                                .get_key_value(key.as_str())
                                .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                        })
                        .collect::<Object>(),
                    paths: Vec::new(),
                })
            }
        }
    }

    impl FetchNode {
        pub(crate) async fn fetch_node<'a>(
            &'a self,
            data: &'a Value,
            current_dir: &'a Path,
            context: &'a Context,
            service_registry: &'a ServiceRegistry,
            schema: &'a Schema,
        ) -> Result<Value, FetchError> {
            let FetchNode {
                operation,
                operation_kind,
                service_name,
                ..
            } = self;

            let Variables { variables, paths } = Variables::new(
                &self.requires,
                self.variable_usages.as_ref(),
                data,
                current_dir,
                context,
                schema,
            )
            .await?;

            let subgraph_request = SubgraphRequest {
                http_request: http::Request::builder()
                    .method(http::Method::POST)
                    .body(
                        Request::builder()
                            .query(operation)
                            .variables(Arc::new(variables))
                            .build(),
                    )
                    .unwrap()
                    .into(),
                context: context.clone(),
                operation_kind: *operation_kind,
            };

            let service = service_registry
                .get(service_name)
                .expect("we already checked that the service exists during planning; qed");

            // TODO not sure if we need a RouterReponse here as we don't do anything with it
            let (_parts, response) = service
                .oneshot(subgraph_request)
                .instrument(tracing::trace_span!("subfetch_stream"))
                .await
                .map_err(|e| FetchError::SubrequestHttpError {
                    service: service_name.to_string(),
                    reason: e.to_string(),
                })?
                .response
                .into_parts();

            if !response.is_primary() {
                return Err(FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_owned(),
                });
            }

            self.response_at_path(current_dir, paths, response)
        }

        #[instrument(skip_all, level = "debug", name = "response_insert")]
        fn response_at_path<'a>(
            &'a self,
            current_dir: &'a Path,
            paths: Vec<Path>,
            subgraph_response: Response,
        ) -> Result<Value, FetchError> {
            let Response { data, .. } = subgraph_response;

            if !self.requires.is_empty() {
                // we have to nest conditions and do early returns here
                // because we need to take ownership of the inner value
                if let Value::Object(mut map) = data {
                    if let Some(entities) = map.remove("_entities") {
                        tracing::trace!("Received entities: {:?}", &entities);

                        if let Value::Array(array) = entities {
                            let mut value = Value::default();

                            for (entity, path) in array.into_iter().zip(paths.into_iter()) {
                                value.insert(&path, entity)?;
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
}
