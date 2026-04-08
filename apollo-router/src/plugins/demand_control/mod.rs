//! Demand control plugin.
//! This plugin will use the cost calculation algorithm to determine if a query should be allowed to execute.
//! On the request path it will use estimated

use std::collections::HashSet;
use std::future;
use std::ops::ControlFlow;
use std::sync::Arc;

use ahash::HashMap;
use ahash::HashMapExt;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::schema::FieldLookupError;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use apollo_federation::error::FederationError;
use apollo_federation::query_plan::serializable_document::SerializableDocumentNotInitialized;
use displaydoc::Display;
use futures::StreamExt;
use futures::future::Either;
use futures::stream;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use thiserror::Error;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Context;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::error::Error;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::json_ext::Object;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::demand_control::cost_calculator::CostBySubgraph;
use crate::plugins::demand_control::cost_calculator::schema::DemandControlledSchema;
use crate::plugins::demand_control::strategy::Strategy;
use crate::plugins::demand_control::strategy::StrategyFactory;
use crate::plugins::telemetry::tracing::apollo_telemetry::emit_error_event;
use crate::services::execution;
use crate::services::subgraph;

pub(crate) mod cost_calculator;
pub(crate) mod strategy;

pub(crate) const COST_ESTIMATED_KEY: &str = "apollo::demand_control::estimated_cost";
pub(crate) const COST_ACTUAL_KEY: &str = "apollo::demand_control::actual_cost";
pub(crate) const COST_RESULT_KEY: &str = "apollo::demand_control::result";
pub(crate) const COST_STRATEGY_KEY: &str = "apollo::demand_control::strategy";

pub(crate) const COST_BY_SUBGRAPH_ACTUAL_KEY: &str =
    "apollo::demand_control::actual_cost_by_subgraph";
pub(crate) const COST_BY_SUBGRAPH_ESTIMATED_KEY: &str =
    "apollo::demand_control::estimated_cost_by_subgraph";
pub(crate) const COST_BY_SUBGRAPH_RESULT_KEY: &str = "apollo::demand_control::result_by_subgraph";

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

        /// The strategy used to calculate the actual cost incurred by an operation.
        ///
        /// * `by_subgraph` (default) computes the cost of each subgraph response and sums them
        ///   to get the total query cost.
        /// * `by_response_shape` computes the cost based on the final structure of the composed
        ///   response, not including any interim structures from subgraph responses that did not
        ///   make it to the composed response.
        #[serde(default)]
        actual_cost_mode: ActualCostMode,

        /// Cost control by subgraph
        #[serde(default)]
        subgraph: SubgraphConfiguration<SubgraphStrategyConfig>,
    },

    #[cfg(test)]
    Test {
        stage: test::TestStage,
        error: test::TestError,
    },
}

#[derive(Copy, Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActualCostMode {
    /// Computes the cost of each subgraph response and sums them to get the total query cost.
    #[default]
    BySubgraph,

    /// Computes the cost based on the final structure of the composed response, not including any
    /// interim structures from subgraph responses that did not make it to the composed response.
    #[deprecated(since = "TBD", note = "use `BySubgraph` instead")]
    #[warn(deprecated_in_future)]
    ByResponseShape,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize, JsonSchema)]
pub(crate) struct SubgraphStrategyConfig {
    /// The assumed length of lists returned by the operation for this subgraph.
    list_size: Option<u32>,

    /// The maximum query cost routed to this subgraph.
    max: Option<f64>,
}

impl StrategyConfig {
    fn validate(&self, subgraph_names: HashSet<&String>) -> Result<(), BoxError> {
        let (actual_cost_mode, subgraphs) = match self {
            StrategyConfig::StaticEstimated {
                actual_cost_mode,
                subgraph,
                ..
            } => (actual_cost_mode, subgraph),
            #[cfg(test)]
            StrategyConfig::Test { .. } => return Ok(()),
        };

        #[allow(deprecated_in_future)]
        if matches!(actual_cost_mode, ActualCostMode::ByResponseShape) {
            tracing::warn!(
                "Actual cost computation mode `by_response_shape` will be deprecated in the future; migrate to `by_subgraph` when possible",
            );
        }

        if subgraphs.all.max.is_some_and(|s| s < 0.0) {
            return Err("Maximum per-subgraph query cost for `all` is negative".into());
        }

        for (subgraph_name, subgraph_config) in subgraphs.subgraphs.iter() {
            if !subgraph_names.contains(subgraph_name) {
                tracing::warn!(
                    "Subgraph `{subgraph_name}` missing from schema but was specified in per-subgraph demand cost; it will be ignored"
                );
                continue;
            }

            if subgraph_config.max.is_some_and(|s| s < 0.0) {
                return Err(format!(
                    "Maximum per-subgraph query cost for `{subgraph_name}` is negative"
                )
                .into());
            }
        }

        Ok(())
    }
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
    /// query estimated cost {estimated_cost} exceeded configured maximum {max_cost} for subgraph {subgraph}
    EstimatedSubgraphCostTooExpensive {
        /// The name of the subgraph
        subgraph: String,
        /// The estimated total cost of the subgraph queries
        estimated_cost: f64,
        /// The maximum total cost of the subgraph queries
        max_cost: f64,
    },
    /// Query actual cost {actual_cost} exceeded configured maximum {max_cost}
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
    SubgraphOperationNotInitialized(SerializableDocumentNotInitialized),
    /// {0}
    ContextSerializationError(String),
    /// {0}
    FederationError(FederationError),
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
                Ok(vec![
                    graphql::Error::builder()
                        .extension_code(self.code())
                        .extensions(extensions)
                        .message(self.to_string())
                        .build(),
                ])
            }
            DemandControlError::EstimatedSubgraphCostTooExpensive {
                ref subgraph,
                estimated_cost,
                max_cost,
            } => {
                let mut extensions = Object::new();
                extensions.insert("cost.subgraph", subgraph.as_str().into());
                extensions.insert("cost.subgraph.estimated", estimated_cost.into());
                extensions.insert("cost.subgraph.max", max_cost.into());
                Ok(vec![
                    graphql::Error::builder()
                        .extension_code(self.code())
                        .extensions(extensions)
                        .message(self.to_string())
                        .build(),
                ])
            }
            DemandControlError::ActualCostTooExpensive {
                actual_cost,
                max_cost,
            } => {
                let mut extensions = Object::new();
                extensions.insert("cost.actual", actual_cost.into());
                extensions.insert("cost.max", max_cost.into());
                Ok(vec![
                    graphql::Error::builder()
                        .extension_code(self.code())
                        .extensions(extensions)
                        .message(self.to_string())
                        .build(),
                ])
            }
            DemandControlError::QueryParseFailure(_) => Ok(vec![
                graphql::Error::builder()
                    .extension_code(self.code())
                    .message(self.to_string())
                    .build(),
            ]),
            DemandControlError::SubgraphOperationNotInitialized(_) => Ok(vec![
                graphql::Error::builder()
                    .extension_code(self.code())
                    .message(self.to_string())
                    .build(),
            ]),
            DemandControlError::ContextSerializationError(_) => Ok(vec![
                graphql::Error::builder()
                    .extension_code(self.code())
                    .message(self.to_string())
                    .build(),
            ]),
            DemandControlError::FederationError(_) => Ok(vec![
                graphql::Error::builder()
                    .extension_code(self.code())
                    .message(self.to_string())
                    .build(),
            ]),
        }
    }
}

impl DemandControlError {
    fn code(&self) -> &'static str {
        match self {
            DemandControlError::EstimatedCostTooExpensive { .. } => "COST_ESTIMATED_TOO_EXPENSIVE",
            DemandControlError::EstimatedSubgraphCostTooExpensive { .. } => {
                "SUBGRAPH_COST_ESTIMATED_TOO_EXPENSIVE"
            }
            DemandControlError::ActualCostTooExpensive { .. } => "COST_ACTUAL_TOO_EXPENSIVE",
            DemandControlError::QueryParseFailure(_) => "COST_QUERY_PARSE_FAILURE",
            DemandControlError::SubgraphOperationNotInitialized(_) => {
                "SUBGRAPH_OPERATION_NOT_INITIALIZED"
            }
            DemandControlError::ContextSerializationError(_) => "COST_CONTEXT_SERIALIZATION_ERROR",
            DemandControlError::FederationError(_) => "FEDERATION_ERROR",
        }
    }
}

impl<T> From<WithErrors<T>> for DemandControlError {
    fn from(value: WithErrors<T>) -> Self {
        DemandControlError::QueryParseFailure(format!("{value}"))
    }
}

impl From<FieldLookupError<'_>> for DemandControlError {
    fn from(value: FieldLookupError) -> Self {
        match value {
            FieldLookupError::NoSuchType => DemandControlError::QueryParseFailure(
                "Attempted to look up a type which does not exist in the schema".to_string(),
            ),
            FieldLookupError::NoSuchField(type_name, _) => {
                DemandControlError::QueryParseFailure(format!(
                    "Attempted to look up a field on type {type_name}, but the field does not exist"
                ))
            }
        }
    }
}

impl From<FederationError> for DemandControlError {
    fn from(value: FederationError) -> Self {
        DemandControlError::FederationError(value)
    }
}

#[derive(Clone)]
pub(crate) struct DemandControlContext {
    pub(crate) strategy: Strategy,
    pub(crate) variables: Object,
}

impl Context {
    pub(crate) fn insert_estimated_cost(&self, cost: f64) -> Result<(), DemandControlError> {
        self.insert(COST_ESTIMATED_KEY, cost)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_estimated_cost(&self) -> Result<Option<f64>, DemandControlError> {
        self.get::<&str, f64>(COST_ESTIMATED_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn insert_actual_cost(&self, cost: f64) -> Result<(), DemandControlError> {
        self.insert(COST_ACTUAL_KEY, cost)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_actual_cost(&self) -> Result<Option<f64>, DemandControlError> {
        self.get::<&str, f64>(COST_ACTUAL_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn get_cost_delta(&self) -> Result<Option<f64>, DemandControlError> {
        let estimated = self.get_estimated_cost()?;
        let actual = self.get_actual_cost()?;
        Ok(estimated.zip(actual).map(|(est, act)| est - act))
    }

    pub(crate) fn insert_estimated_cost_by_subgraph(
        &self,
        cost: CostBySubgraph,
    ) -> Result<(), DemandControlError> {
        self.insert(COST_BY_SUBGRAPH_ESTIMATED_KEY, cost)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_estimated_cost_by_subgraph(
        &self,
    ) -> Result<Option<CostBySubgraph>, DemandControlError> {
        self.get::<&str, CostBySubgraph>(COST_BY_SUBGRAPH_ESTIMATED_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn get_actual_cost_by_subgraph(
        &self,
    ) -> Result<Option<CostBySubgraph>, DemandControlError> {
        self.get::<&str, CostBySubgraph>(COST_BY_SUBGRAPH_ACTUAL_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn update_actual_cost_by_subgraph(
        &self,
        subgraph: &str,
        cost: f64,
    ) -> Result<(), DemandControlError> {
        // combine this cost with the cost that already exists in the context
        self.upsert(
            COST_BY_SUBGRAPH_ACTUAL_KEY,
            |mut existing_cost: CostBySubgraph| {
                existing_cost.add_or_insert(subgraph, cost);
                existing_cost
            },
        )
        .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn insert_actual_cost_by_subgraph(
        &self,
        cost: CostBySubgraph,
    ) -> Result<(), DemandControlError> {
        // combine this cost with the cost that already exists in the context
        self.upsert(
            COST_BY_SUBGRAPH_ACTUAL_KEY,
            |mut existing_cost: CostBySubgraph| {
                existing_cost += cost;
                existing_cost
            },
        )
        .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn insert_cost_result(&self, result: String) -> Result<(), DemandControlError> {
        self.insert(COST_RESULT_KEY, result)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_cost_result(&self) -> Result<Option<String>, DemandControlError> {
        self.get::<&str, String>(COST_RESULT_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn insert_cost_by_subgraph_result(
        &self,
        subgraph: String,
        result: String,
    ) -> Result<(), DemandControlError> {
        self.upsert::<_, HashMap<String, String>>(
            COST_BY_SUBGRAPH_RESULT_KEY,
            |mut current_results| {
                current_results.insert(subgraph, result);
                current_results
            },
        )
        .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn insert_cost_strategy(&self, strategy: String) -> Result<(), DemandControlError> {
        self.insert(COST_STRATEGY_KEY, strategy)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn get_cost_strategy(&self) -> Result<Option<String>, DemandControlError> {
        self.get::<&str, String>(COST_STRATEGY_KEY)
            .map_err(|e| DemandControlError::ContextSerializationError(e.to_string()))
    }

    pub(crate) fn insert_demand_control_context(&self, ctx: DemandControlContext) {
        self.extensions().with_lock(|lock| lock.insert(ctx));
    }

    pub(crate) fn get_demand_control_context(&self) -> Option<DemandControlContext> {
        self.extensions().with_lock(|lock| lock.get().cloned())
    }
}

pub(crate) struct DemandControl {
    config: DemandControlConfig,
    strategy_factory: StrategyFactory,
}

impl DemandControl {
    fn report_operation_metric(context: Context) {
        let result = context
            .get(COST_RESULT_KEY)
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
        if !init.config.enabled {
            return Ok(DemandControl {
                strategy_factory: StrategyFactory::new(
                    init.config.clone(),
                    Arc::new(DemandControlledSchema::empty(
                        init.supergraph_schema.clone(),
                    )?),
                    Arc::new(HashMap::new()),
                ),
                config: init.config,
            });
        }

        let demand_controlled_supergraph_schema =
            DemandControlledSchema::new(init.supergraph_schema.clone())?;
        let mut demand_controlled_subgraph_schemas = HashMap::new();
        for (subgraph_name, subgraph_schema) in init.subgraph_schemas.iter() {
            let demand_controlled_subgraph_schema =
                DemandControlledSchema::new(subgraph_schema.clone())?;
            demand_controlled_subgraph_schemas
                .insert(subgraph_name.clone(), demand_controlled_subgraph_schema);
        }

        let subgraph_names = init.subgraph_schemas.keys().collect();
        init.config.strategy.validate(subgraph_names)?;

        Ok(DemandControl {
            strategy_factory: StrategyFactory::new(
                init.config.clone(),
                Arc::new(demand_controlled_supergraph_schema),
                Arc::new(demand_controlled_subgraph_schemas),
            ),
            config: init.config,
        })
    }

    fn execution_service(&self, service: execution::BoxCloneService) -> execution::BoxCloneService {
        if !self.config.enabled {
            service
        } else {
            let strategy = self.strategy_factory.create();
            ServiceBuilder::new()
                .checkpoint(move |req: execution::Request| {
                    req.context
                        .insert_demand_control_context(DemandControlContext {
                            strategy: strategy.clone(),
                            variables: req.supergraph_request.body().variables.clone(),
                        });

                    // On the request path we need to check for estimates, checkpoint is used to do this, short-circuiting the request if it's too expensive.
                    Ok(match strategy.on_execution_request(&req) {
                        Ok(_) => ControlFlow::Continue(req),
                        Err(err) => {
                            let graphql_errors = err
                                .into_graphql_errors()
                                .expect("must be able to convert to graphql error");
                            graphql_errors.iter().for_each(|mapped_error| {
                                if let Some(Value::String(error_code)) =
                                    mapped_error.extensions.get("code")
                                {
                                    emit_error_event(
                                        error_code.as_str(),
                                        &mapped_error.message,
                                        mapped_error.path.clone(),
                                    );
                                }
                            });
                            ControlFlow::Break(
                                execution::Response::builder()
                                    .errors(graphql_errors)
                                    .context(req.context.clone())
                                    .build()
                                    .expect("Must be able to build response"),
                            )
                        }
                    })
                })
                .map_response(|mut resp: execution::Response| {
                    let req = resp
                        .context
                        .executable_document()
                        .expect("must have document");
                    let strategy = resp
                        .context
                        .get_demand_control_context()
                        .map(|ctx| ctx.strategy)
                        .expect("must have strategy");
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
                .boxed_clone()
        }
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        if !self.config.enabled {
            service
        } else {
            let subgraph_name = subgraph_name.to_owned();
            let subgraph_name_map_fut = subgraph_name.to_owned();
            ServiceBuilder::new()
                .checkpoint(move |req: subgraph::Request| {
                    let strategy = req.context.get_demand_control_context().map(|c| c.strategy).expect("must have strategy");

                    // On the request path we need to check for estimates, checkpoint is used to do this, short-circuiting the request if it's too expensive.
                    Ok(match strategy.on_subgraph_request(&req) {
                        Ok(_) => ControlFlow::Continue(req),
                        Err(err) => ControlFlow::Break(
                            subgraph::Response::builder()
                                .errors(
                                    err.into_graphql_errors()
                                        .expect("must be able to convert to graphql error"),
                                )
                                .id(req.id)
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
                        let strategy = resp.context.get_demand_control_context().map(|c| c.strategy).expect("must have strategy");
                        Ok(match strategy.on_subgraph_response(req.as_ref(), &resp, &subgraph_name) {
                            Ok(_) => resp,
                            Err(err) => subgraph::Response::builder()
                                .errors(
                                    err.into_graphql_errors()
                                        .expect("must be able to convert to graphql error"),
                                )
                                .id(resp.id)
                                .subgraph_name(subgraph_name)
                                .context(resp.context.clone())
                                .extensions(Object::new())
                                .build(),
                        })
                    },
                )
                .service(service)
                .boxed_clone()
        }
    }
}

register_plugin!("apollo", "demand_control", DemandControl);

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_compiler::ast;
    use apollo_compiler::validation::Valid;
    use futures::StreamExt;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use tokio::task::JoinSet;

    use crate::Context;
    use crate::graphql;
    use crate::graphql::Response;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::demand_control::DemandControl;
    use crate::plugins::demand_control::DemandControlContext;
    use crate::plugins::demand_control::DemandControlError;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::execution;
    use crate::services::layers::query_analysis::ParsedDocument;
    use crate::services::layers::query_analysis::ParsedDocumentInner;
    use crate::services::subgraph;

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
            .await
            .expect("test harness");
        let ctx = context();
        let resp = plugin
            .execution_service(|req| async {
                Ok(execution::Response::fake_builder()
                    .context(req.context)
                    .build()
                    .unwrap())
            })
            .call(execution::Request::fake_builder().context(ctx).build())
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
            .await
            .expect("test harness");
        let strategy = plugin.strategy_factory.create();

        let ctx = context();
        ctx.insert_demand_control_context(DemandControlContext {
            strategy,
            variables: Default::default(),
        });
        let mut req = subgraph::Request::fake_builder()
            .subgraph_name("test")
            .context(ctx)
            .build();
        req.executable_document = Some(Arc::new(Valid::assume_valid(ExecutableDocument::new())));
        let resp = plugin
            .subgraph_service("test", |req| async {
                Ok(subgraph::Response::fake_builder()
                    .context(req.context)
                    .build())
            })
            .call(req)
            .await
            .unwrap();

        resp.response.into_body()
    }

    fn context() -> Context {
        let schema = Schema::parse_and_validate("type Query { f: Int }", "").unwrap();
        let ast = ast::Document::parse("{__typename}", "").unwrap();
        let doc = ast.to_executable_validate(&schema).unwrap();
        let parsed_document =
            ParsedDocumentInner::new(ast, doc.into(), None, Default::default()).unwrap();
        let ctx = Context::new();
        ctx.extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(parsed_document));
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

    // sanity check that our actuals calculation is always accumulative and based on safe data
    // structures (like its current implementation, using DashMap, which functions similarly to a
    // RwLock) -- this test looks at the same subgraph updates
    #[tokio::test]
    async fn test_concurrent_actual_cost_updates_to_same_subgraph() {
        let ctx = Arc::new(Context::new());
        let num_tasks = 100;
        let cost_per_update = 1.5;

        // multiple updates to the SAME subgraph
        let mut join_set = JoinSet::new();
        for _ in 0..num_tasks {
            let ctx = ctx.clone();
            join_set.spawn(async move {
                ctx.update_actual_cost_by_subgraph("products", cost_per_update)
                    .expect("update should succeed");
            });
        }

        while let Some(result) = join_set.join_next().await {
            result.expect("painicked waiting to join tasks");
        }

        let cost_by_subgraph = ctx
            .get_actual_cost_by_subgraph()
            .expect("should deserialize")
            .expect("should have value");

        let products_cost = cost_by_subgraph
            .get("products")
            .expect("should have products");
        let expected_cost = num_tasks as f64 * cost_per_update;

        assert_eq!(
            products_cost, expected_cost,
            "Expected products cost {expected_cost}, got {products_cost}"
        );
    }

    // sanity check that our actuals calculation is always accumulative and based on safe data
    // structures (like its current implementation, using DashMap, which functions similarly to a
    // RwLock) -- this test looks at different subgraph updates
    #[tokio::test]
    async fn test_concurrent_actual_cost_multiple_subgraphs() {
        // multiple updates to DIFFERENT subgraphs
        let ctx = Arc::new(Context::new());
        let subgraphs = ["users", "products", "reviews", "inventory", "pricing"];
        let updates_per_subgraph = 20;
        let cost_per_update = 1.5;

        let mut join_set = JoinSet::new();
        for subgraph in subgraphs.iter() {
            for _ in 0..updates_per_subgraph {
                let ctx = ctx.clone();
                let subgraph = subgraph.to_string();
                join_set.spawn(async move {
                    ctx.update_actual_cost_by_subgraph(&subgraph, cost_per_update)
                        .expect("update should succeed");
                });
            }
        }

        while let Some(result) = join_set.join_next().await {
            result.expect("task should not panic");
        }

        let cost_by_subgraph = ctx
            .get_actual_cost_by_subgraph()
            .expect("should deserialize")
            .expect("should have value");

        for subgraph in subgraphs.iter() {
            let cost = cost_by_subgraph
                .get(subgraph)
                .expect("should have subgraph");
            let expected = updates_per_subgraph as f64 * cost_per_update;

            assert_eq!(
                cost, expected,
                "Expected {subgraph} cost {expected}, got {cost}"
            );
        }

        let total = cost_by_subgraph.total();
        let expected_total = subgraphs.len() as f64 * updates_per_subgraph as f64 * cost_per_update;
        assert_eq!(
            total, expected_total,
            "Expected total cost {expected_total}, got {total}"
        );
    }
}
