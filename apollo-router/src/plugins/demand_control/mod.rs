//! Demand control plugin.
//! This plugin will use the cost calculation algorithm to determine if a query should be allowed to execute.
//! On the request path it will use estimated
use std::future;
use std::ops::ControlFlow;
use std::sync::Arc;

use ahash::HashMap;
use ahash::HashMapExt;
use apollo_compiler::schema::FieldLookupError;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use apollo_compiler::ExecutableDocument;
use displaydoc::Display;
use futures::future::Either;
use futures::stream;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::json_ext::Object;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::demand_control::cost_calculator::schema::DemandControlledSchema;
use crate::plugins::demand_control::strategy::Strategy;
use crate::plugins::demand_control::strategy::StrategyFactory;
use crate::register_plugin;
use crate::services::execution;
use crate::services::execution::BoxService;
use crate::services::subgraph;
use crate::Context;

pub(crate) mod cost_calculator;
pub(crate) mod strategy;

// TODO: Do these need to be `pub`?
pub(crate) const COST_ESTIMATED_CONTEXT_KEY: &str = "cost.estimated";
pub(crate) const COST_ACTUAL_CONTEXT_KEY: &str = "cost.actual";
pub(crate) const COST_RESULT_CONTEXT_KEY: &str = "cost.result";
pub(crate) const COST_STRATEGY_CONTEXT_KEY: &str = "cost.strategy";

/// Algorithm for calculating the cost of an incoming query.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum StrategyConfig {
    /// A simple, statically-defined cost mapping for operations and types.
    ///
    /// Operation costs:
    /// - Mutation: 10
    /// - Query: 0
    /// - Subscription 0
    ///
    /// Type costs:
    /// - Object: 1
    /// - Interface: 1
    /// - Union: 1
    /// - Scalar: 0
    /// - Enum: 0
    StaticEstimated {
        /// The assumed length of lists returned by the operation.
        list_size: u32,
        /// The maximum cost of a query
        max: f64,
    },

    #[cfg(test)]
    Test {
        stage: test::TestStage,
        error: test::TestError,
    },
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Mode {
    Measure,
    Enforce,
}

/// Demand control configuration
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DemandControlConfig {
    /// Enable demand control
    enabled: bool,
    /// The mode that the demand control plugin should operate in.
    /// - Measure: The plugin will measure the cost of incoming requests but not reject them.
    /// - Enforce: The plugin will enforce the cost of incoming requests and reject them if the algorithm indicates that they should be rejected.
    mode: Mode,
    /// The strategy used to reject requests.
    strategy: StrategyConfig,
}

#[derive(Debug, Display, Error)]
pub(crate) enum DemandControlError {
    /// query estimated cost {estimated_cost} exceeded configured maximum {max_cost}
    EstimatedCostTooExpensive {
        /// The estimated cost of the query
        estimated_cost: f64,
        /// The maximum cost of the query
        max_cost: f64,
    },
    /// auery actual cost {actual_cost} exceeded configured maximum {max_cost}
    #[allow(dead_code)]
    ActualCostTooExpensive {
        /// The actual cost of the query
        actual_cost: f64,
        /// The maximum cost of the query
        max_cost: f64,
    },
    /// Query could not be parsed: {0}
    QueryParseFailure(String),
    /// {0}
    SubgraphOperationNotInitialized(crate::query_planner::fetch::SubgraphOperationNotInitialized),
    /// {0}
    ContextSerializationError(String),
}

impl IntoGraphQLErrors for DemandControlError {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        match self {
            DemandControlError::EstimatedCostTooExpensive {
                estimated_cost,
                max_cost,
            } => {
                let mut extensions = Object::new();
                extensions.insert("cost.estimated", estimated_cost.into());
                extensions.insert("cost.max", max_cost.into());
                Ok(vec![graphql::Error::builder()
                    .extension_code(self.code())
                    .extensions(extensions)
                    .message(self.to_string())
                    .build()])
            }
            DemandControlError::ActualCostTooExpensive {
                actual_cost,
                max_cost,
            } => {
                let mut extensions = Object::new();
                extensions.insert("cost.actual", actual_cost.into());
                extensions.insert("cost.max", max_cost.into());
                Ok(vec![graphql::Error::builder()
                    .extension_code(self.code())
                    .extensions(extensions)
                    .message(self.to_string())
                    .build()])
            }
            DemandControlError::QueryParseFailure(_) => Ok(vec![graphql::Error::builder()
                .extension_code(self.code())
                .message(self.to_string())
                .build()]),
            DemandControlError::SubgraphOperationNotInitialized(e) => Ok(e.into_graphql_errors()),
            DemandControlError::ContextSerializationError(_) => Ok(vec![graphql::Error::builder()
                .extension_code(self.code())
                .message(self.to_string())
                .build()]),
        }
    }
}

impl DemandControlError {
    fn code(&self) -> &'static str {
        match self {
            DemandControlError::EstimatedCostTooExpensive { .. } => "COST_ESTIMATED_TOO_EXPENSIVE",
            DemandControlError::ActualCostTooExpensive { .. } => "COST_ACTUAL_TOO_EXPENSIVE",
            DemandControlError::QueryParseFailure(_) => "COST_QUERY_PARSE_FAILURE",
            DemandControlError::SubgraphOperationNotInitialized(e) => e.code(),
            DemandControlError::ContextSerializationError(_) => "COST_CONTEXT_SERIALIZATION_ERROR",
        }
    }
}

impl<T> From<WithErrors<T>> for DemandControlError {
    fn from(value: WithErrors<T>) -> Self {
        DemandControlError::QueryParseFailure(format!("{}", value))
    }
}

impl<'a> From<FieldLookupError<'a>> for DemandControlError {
    fn from(value: FieldLookupError) -> Self {
        match value {
            FieldLookupError::NoSuchType => DemandControlError::QueryParseFailure(
                "Attempted to look up a type which does not exist in the schema".to_string(),
            ),
            FieldLookupError::NoSuchField(type_name, _) => {
                DemandControlError::QueryParseFailure(format!(
                    "Attempted to look up a field on type {}, but the field does not exist",
                    type_name
                ))
            }
        }
    }
}

impl Context {
    pub(crate) fn insert_estimated_cost(&self, cost: f64) -> Result<(), DemandControlError> {
        self.insert(COST_ESTIMATED_CONTEXT_KEY, cost)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_estimated_cost(&self) -> Result<Option<f64>, DemandControlError> {
        self.get::<&str, f64>(COST_ESTIMATED_CONTEXT_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn insert_actual_cost(&self, cost: f64) -> Result<(), DemandControlError> {
        self.insert(COST_ACTUAL_CONTEXT_KEY, cost)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_actual_cost(&self) -> Result<Option<f64>, DemandControlError> {
        self.get::<&str, f64>(COST_ACTUAL_CONTEXT_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn insert_cost_result(&self, result: String) -> Result<(), DemandControlError> {
        self.insert(COST_RESULT_CONTEXT_KEY, result)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_cost_result(&self) -> Result<Option<String>, DemandControlError> {
        self.get::<&str, String>(COST_RESULT_CONTEXT_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn insert_cost_strategy(&self, strategy: String) -> Result<(), DemandControlError> {
        self.insert(COST_STRATEGY_CONTEXT_KEY, strategy)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_cost_strategy(&self) -> Result<Option<String>, DemandControlError> {
        self.get::<&str, String>(COST_STRATEGY_CONTEXT_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }
}

pub(crate) struct DemandControl {
    config: DemandControlConfig,
    strategy_factory: StrategyFactory,
}

impl DemandControl {
    fn report_operation_metric(context: Context) {
        let result = context
            .get(COST_RESULT_CONTEXT_KEY)
            .ok()
            .flatten()
            .unwrap_or("NO_CONTEXT".to_string());
        u64_counter!(
            "apollo.router.operations.demand_control",
            "Total operations with demand control enabled",
            1,
            "demand_control.result" = result
        );
    }
}

#[async_trait::async_trait]
impl Plugin for DemandControl {
    type Config = DemandControlConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let demand_controlled_supergraph_schema =
            DemandControlledSchema::new(init.supergraph_schema.clone())?;
        let mut demand_controlled_subgraph_schemas = HashMap::new();
        for (subgraph_name, subgraph_schema) in init.subgraph_schemas.iter() {
            let demand_controlled_subgraph_schema =
                DemandControlledSchema::new(subgraph_schema.clone())?;
            demand_controlled_subgraph_schemas
                .insert(subgraph_name.clone(), demand_controlled_subgraph_schema);
        }

        Ok(DemandControl {
            strategy_factory: StrategyFactory::new(
                init.config.clone(),
                Arc::new(demand_controlled_supergraph_schema),
                Arc::new(demand_controlled_subgraph_schemas),
            ),
            config: init.config,
        })
    }

    fn execution_service(&self, service: BoxService) -> BoxService {
        if !self.config.enabled {
            service
        } else {
            let strategy = self.strategy_factory.create();
            ServiceBuilder::new()
                .checkpoint(move |req: execution::Request| {
                    req.context
                        .extensions()
                        .with_lock(|mut lock| lock.insert(strategy.clone()));
                    // On the request path we need to check for estimates, checkpoint is used to do this, short-circuiting the request if it's too expensive.
                    Ok(match strategy.on_execution_request(&req) {
                        Ok(_) => ControlFlow::Continue(req),
                        Err(err) => ControlFlow::Break(
                            execution::Response::builder()
                                .errors(
                                    err.into_graphql_errors()
                                        .expect("must be able to convert to graphql error"),
                                )
                                .context(req.context.clone())
                                .build()
                                .expect("Must be able to build response"),
                        ),
                    })
                })
                .map_response(|mut resp: execution::Response| {
                    let req = resp
                        .context
                        .unsupported_executable_document()
                        .expect("must have document");
                    let strategy = resp.context.extensions().with_lock(|lock| {
                        lock.get::<Strategy>().expect("must have strategy").clone()
                    });
                    let context = resp.context.clone();

                    // We want to sequence this code to run after all the subgraph responses have been scored.
                    // To do so without collecting all the results, we chain this "empty" stream onto the end.
                    let report_operation_metric =
                        futures::stream::unfold(resp.context.clone(), |ctx| async move {
                            Self::report_operation_metric(ctx);
                            None
                        });

                    resp.response = resp.response.map(move |resp| {
                        // Here we are going to abort the stream if the cost is too high
                        // First we map based on cost, then we use take while to abort the stream if an error is emitted.
                        // When we terminate the stream we still want to emit a graphql error, so the error response is emitted first before a termination error.
                        resp.flat_map(move |resp| {
                            match strategy.on_execution_response(&context, req.as_ref(), &resp) {
                                Ok(_) => Either::Left(stream::once(future::ready(Ok(resp)))),
                                Err(err) => {
                                    Either::Right(stream::iter(vec![
                                        // This is the error we are returning to the user
                                        Ok(graphql::Response::builder()
                                            .errors(
                                                err.into_graphql_errors().expect(
                                                    "must be able to convert to graphql error",
                                                ),
                                            )
                                            .extensions(crate::json_ext::Object::new())
                                            .build()),
                                        // This will terminate the stream
                                        Err(()),
                                    ]))
                                }
                            }
                        })
                        // Terminate the stream on error
                        .take_while(|resp| future::ready(resp.is_ok()))
                        // Unwrap the result. This is safe because we are terminating the stream on error.
                        .map(|i| i.expect("error used to terminate stream"))
                        .chain(report_operation_metric)
                        .boxed()
                    });
                    resp
                })
                .service(service)
                .boxed()
        }
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        if !self.config.enabled {
            service
        } else {
            let subgraph_name = subgraph_name.to_owned();
            let subgraph_name_map_fut = subgraph_name.to_owned();
            ServiceBuilder::new()
                .checkpoint(move |req: subgraph::Request| {
                    let strategy = req.context.extensions().with_lock(|lock| {
                        lock.get::<Strategy>().expect("must have strategy").clone()
                    });

                    // On the request path we need to check for estimates, checkpoint is used to do this, short-circuiting the request if it's too expensive.
                    Ok(match strategy.on_subgraph_request(&req) {
                        Ok(_) => ControlFlow::Continue(req),
                        Err(err) => ControlFlow::Break(
                            subgraph::Response::builder()
                                .errors(
                                    err.into_graphql_errors()
                                        .expect("must be able to convert to graphql error"),
                                )
                                .context(req.context.clone())
                                .extensions(crate::json_ext::Object::new())
                                .subgraph_name(subgraph_name.clone())
                                .build(),
                        ),
                    })
                })
                .map_future_with_request_data(
                    move |req: &subgraph::Request| {
                        //TODO convert this to expect
                        (
                            subgraph_name_map_fut.clone(),
                            req.executable_document.clone().unwrap_or_else(|| {
                                Arc::new(Valid::assume_valid(ExecutableDocument::new()))
                            }),
                        )
                    },
                    |(subgraph_name, req): (String, Arc<Valid<ExecutableDocument>>), fut| async move {
                        let resp: subgraph::Response = fut.await?;
                        let strategy = resp.context.extensions().with_lock(|lock| {
                            lock.get::<Strategy>().expect("must have strategy").clone()
                        });
                        Ok(match strategy.on_subgraph_response(req.as_ref(), &resp) {
                            Ok(_) => resp,
                            Err(err) => subgraph::Response::builder()
                                .errors(
                                    err.into_graphql_errors()
                                        .expect("must be able to convert to graphql error"),
                                )
                                .subgraph_name(subgraph_name)
                                .context(resp.context.clone())
                                .extensions(Object::new())
                                .build(),
                        })
                    },
                )
                .service(service)
                .boxed()
        }
    }
}

register_plugin!("apollo", "demand_control", DemandControl);

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use apollo_compiler::ast;
    use apollo_compiler::validation::Valid;
    use apollo_compiler::ExecutableDocument;
    use futures::StreamExt;
    use schemars::JsonSchema;
    use serde::Deserialize;

    use crate::graphql;
    use crate::graphql::Response;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::demand_control::DemandControl;
    use crate::plugins::demand_control::DemandControlError;
    use crate::plugins::test::PluginTestHarness;
    use crate::query_planner::fetch::QueryHash;
    use crate::services::execution;
    use crate::services::layers::query_analysis::ParsedDocument;
    use crate::services::layers::query_analysis::ParsedDocumentInner;
    use crate::services::subgraph;
    use crate::Context;

    #[tokio::test]
    async fn test_measure_on_execution_request() {
        let body = test_on_execution(include_str!(
            "fixtures/measure_on_execution_request.router.yaml"
        ))
        .await;
        insta::assert_yaml_snapshot!(body);
    }

    #[tokio::test]
    async fn test_enforce_on_execution_request() {
        let body = test_on_execution(include_str!(
            "fixtures/enforce_on_execution_request.router.yaml"
        ))
        .await;
        insta::assert_yaml_snapshot!(body);
    }

    #[tokio::test]
    async fn test_measure_on_execution_response() {
        let body = test_on_execution(include_str!(
            "fixtures/measure_on_execution_response.router.yaml"
        ))
        .await;
        insta::assert_yaml_snapshot!(body);
    }

    #[tokio::test]
    async fn test_enforce_on_execution_response() {
        let body = test_on_execution(include_str!(
            "fixtures/enforce_on_execution_response.router.yaml"
        ))
        .await;
        insta::assert_yaml_snapshot!(body);
    }

    #[tokio::test]
    async fn test_measure_on_subgraph_request() {
        let body = test_on_subgraph(include_str!(
            "fixtures/measure_on_subgraph_request.router.yaml"
        ))
        .await;
        insta::assert_yaml_snapshot!(body);
    }

    #[tokio::test]
    async fn test_enforce_on_subgraph_request() {
        let body = test_on_subgraph(include_str!(
            "fixtures/enforce_on_subgraph_request.router.yaml"
        ))
        .await;
        insta::assert_yaml_snapshot!(body);
    }

    #[tokio::test]
    async fn test_measure_on_subgraph_response() {
        let body = test_on_subgraph(include_str!(
            "fixtures/measure_on_subgraph_response.router.yaml"
        ))
        .await;
        insta::assert_yaml_snapshot!(body);
    }

    #[tokio::test]
    async fn test_enforce_on_subgraph_response() {
        let body = test_on_subgraph(include_str!(
            "fixtures/enforce_on_subgraph_response.router.yaml"
        ))
        .await;
        insta::assert_yaml_snapshot!(body);
    }

    #[tokio::test]
    async fn test_operation_metrics() {
        async {
            test_on_execution(include_str!(
                "fixtures/measure_on_execution_request.router.yaml"
            ))
            .await;
            assert_counter!(
                "apollo.router.operations.demand_control",
                1,
                "demand_control.result" = "COST_ESTIMATED_TOO_EXPENSIVE"
            );

            test_on_execution(include_str!(
                "fixtures/enforce_on_execution_response.router.yaml"
            ))
            .await;
            assert_counter!(
                "apollo.router.operations.demand_control",
                2,
                "demand_control.result" = "COST_ESTIMATED_TOO_EXPENSIVE"
            );

            // The metric should not be published on subgraph requests
            test_on_subgraph(include_str!(
                "fixtures/enforce_on_subgraph_request.router.yaml"
            ))
            .await;
            test_on_subgraph(include_str!(
                "fixtures/enforce_on_subgraph_response.router.yaml"
            ))
            .await;
            assert_counter!(
                "apollo.router.operations.demand_control",
                2,
                "demand_control.result" = "COST_ESTIMATED_TOO_EXPENSIVE"
            );
        }
        .with_metrics()
        .await
    }

    async fn test_on_execution(config: &'static str) -> Vec<Response> {
        let plugin = PluginTestHarness::<DemandControl>::builder()
            .config(config)
            .build()
            .await;

        let ctx = context();

        let resp = plugin
            .call_execution(
                execution::Request::fake_builder().context(ctx).build(),
                |req| {
                    execution::Response::fake_builder()
                        .context(req.context)
                        .build()
                        .unwrap()
                },
            )
            .await
            .unwrap();

        resp.response
            .into_body()
            .collect::<Vec<graphql::Response>>()
            .await
    }

    async fn test_on_subgraph(config: &'static str) -> Response {
        let plugin = PluginTestHarness::<DemandControl>::builder()
            .config(config)
            .build()
            .await;
        let strategy = plugin.strategy_factory.create();

        let ctx = context();
        ctx.extensions().with_lock(|mut lock| lock.insert(strategy));
        let mut req = subgraph::Request::fake_builder()
            .subgraph_name("test")
            .context(ctx)
            .build();
        req.executable_document = Some(Arc::new(Valid::assume_valid(ExecutableDocument::new())));
        let resp = plugin
            .call_subgraph(req, |req| {
                subgraph::Response::fake_builder()
                    .context(req.context)
                    .build()
            })
            .await
            .unwrap();

        resp.response.into_body()
    }

    fn context() -> Context {
        let parsed_document = ParsedDocumentInner {
            executable: Arc::new(Valid::assume_valid(ExecutableDocument::new())),
            hash: Arc::new(QueryHash::default()),
            ast: ast::Document::new(),
        };
        let ctx = Context::new();
        ctx.extensions()
            .with_lock(|mut lock| lock.insert(ParsedDocument::new(parsed_document)));
        ctx
    }

    #[derive(Clone, Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "snake_case")]
    pub(crate) enum TestStage {
        ExecutionRequest,
        ExecutionResponse,
        SubgraphRequest,
        SubgraphResponse,
    }

    #[derive(Clone, Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "snake_case")]
    pub(crate) enum TestError {
        EstimatedCostTooExpensive,
        ActualCostTooExpensive,
    }

    impl From<&TestError> for DemandControlError {
        fn from(value: &TestError) -> Self {
            match value {
                TestError::EstimatedCostTooExpensive => {
                    DemandControlError::EstimatedCostTooExpensive {
                        max_cost: 1.0,
                        estimated_cost: 2.0,
                    }
                }

                TestError::ActualCostTooExpensive => DemandControlError::ActualCostTooExpensive {
                    actual_cost: 1.0,
                    max_cost: 2.0,
                },
            }
        }
    }
}
