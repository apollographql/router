mod caching_query_planner;
mod router_bridge_query_planner;
mod selection;
use crate::prelude::graphql::*;
pub use caching_query_planner::*;
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
    Fetch(fetch::FetchNode),

    /// Merge the current resultset with the response.
    Flatten(FlattenNode),
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
        Box::pin(
            async move {
                tracing::trace!("Executing plan:\n{:#?}", self);
                let mut value;
                let mut errors = Vec::new();

                match self {
                    PlanNode::Sequence { nodes } => {
                        let sequence_span = tracing::info_span!("sequence");
                        let _entered = sequence_span.enter();

                        value = parent_value.clone();
                        for node in nodes {
                            let (v, err) = node
                                .execute_recursively(
                                    current_dir,
                                    context,
                                    service_registry,
                                    schema,
                                    &value,
                                )
                                .in_current_span()
                                .await;
                            value.deep_merge(v);
                            errors.extend(err.into_iter());
                        }
                    }
                    PlanNode::Parallel { nodes } => {
                        let parallel_span = tracing::info_span!("parallel");
                        let _entered = parallel_span.enter();
                        value = Value::default();

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
                                .in_current_span()
                            })
                            .collect();

                        while let Some((v, err)) = stream.next().in_current_span().await {
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
                            .fetch_node(
                                parent_value,
                                current_dir,
                                context,
                                service_registry,
                                schema,
                            )
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
            }
            .instrument(tracing::info_span!("step")),
        )
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

mod fetch {
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
    }

    struct Variables {
        variables: Object,
        paths: Vec<Path>,
    }

    impl Variables {
        #[instrument(level = "debug", name = "make_variables", skip_all)]
        fn new(
            requires: &[Selection],
            variable_usages: &[String],
            data: &Value,
            current_dir: &Path,
            context: &Context,
            schema: &Schema,
        ) -> Result<Variables, FetchError> {
            if !requires.is_empty() {
                let mut variables = Object::with_capacity(1 + variable_usages.len());
                variables.extend(variable_usages.iter().filter_map(|key| {
                    context
                        .request
                        .body()
                        .variables
                        .get_key_value(key.as_str())
                        .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                }));

                let mut paths = Vec::new();
                let mut values = Vec::new();
                data.select_values_and_paths(current_dir, |_path, value| {
                    paths.push(_path);
                    values.push(value)
                })?;

                let representations = Value::Array(
                    values
                        .into_iter()
                        .flat_map(|value| match value {
                            Value::Object(content) => {
                                select_object(content, requires, schema).transpose()
                            }
                            _ => Some(Err(FetchError::ExecutionInvalidContent {
                                reason: "not an object".to_string(),
                            })),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                );
                variables.insert("representations", representations);

                Ok(Variables { variables, paths })
            } else {
                Ok(Variables {
                    variables: variable_usages
                        .iter()
                        .filter_map(|key| {
                            context
                                .request
                                .body()
                                .variables
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
                service_name,
                ..
            } = self;

            let query_span = tracing::info_span!("subfetch", service = service_name.as_str());

            let Variables { variables, paths } = query_span.in_scope(|| {
                Variables::new(
                    &self.requires,
                    self.variable_usages.as_ref(),
                    data,
                    current_dir,
                    context,
                    schema,
                )
            })?;

            let subgraph_request = SubgraphRequest {
                http_request: http::Request::builder()
                    .method(http::Method::POST)
                    .body(
                        Request::builder()
                            .query(operation)
                            .variables(Arc::new(variables))
                            .build(),
                    )
                    .unwrap(),
                context: context.clone(),
            };

            let service = service_registry
                .get(service_name)
                .expect("we already checked that the service exists during planning; qed");

            // TODO not sure if we need a RouterReponse here as we don't do anything with it
            let (_parts, response) = service
                .oneshot(subgraph_request)
                .instrument(tracing::info_span!(parent: &query_span, "subfetch_stream"))
                .await
                .map_err(|e| FetchError::SubrequestHttpError {
                    service: service_name.to_string(),
                    reason: e.to_string(),
                })?
                .response
                .into_parts();

            query_span.in_scope(|| {
                if !response.is_primary() {
                    return Err(FetchError::SubrequestUnexpectedPatchResponse {
                        service: service_name.to_owned(),
                    });
                }

                self.response_at_path(current_dir, paths, response)
            })
        }

        #[instrument(level = "debug", name = "response_insert", skip_all)]
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
