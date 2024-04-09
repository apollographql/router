//! Demand control plugin.
mod basic_cost_calculator;
mod directives;
mod schema_aware_response;

use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use apollo_compiler::Schema;
use displaydoc::Display;
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::graphql;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::query_planner::QueryPlan;
use crate::register_plugin;
use crate::services::execution::BoxService;

/// Algorithm for calculating the cost of an incoming query.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum CostCalculationAlgorithm {
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
    Basic {
        /// The maximum cost of a query
        max: f64,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
enum Mode {
    Measure,
    Reject,
}

trait CostCalculator {
    fn estimated(
        query: &ExecutableDocument,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError>;

    fn planned(&self, query_plan: &QueryPlan) -> Result<f64, DemandControlError>;

    fn actual(
        request: &ExecutableDocument,
        response: &graphql::Response,
    ) -> Result<f64, DemandControlError>;
}

/// Demand control configuration
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DemandControlConfig {
    /// Enable demand control
    enabled: bool,
    /// The mode that the demand control plugin should operate in.
    /// - Measure: The plugin will measure the cost of incoming requests but not reject them.
    /// - Reject: The plugin will measure the cost of incoming requests and reject them if the algorithm indicates that they should be rejected.
    mode: Mode,
    /// The algorithm used to calculate the cost of an incoming request
    #[allow(dead_code)]
    algorithm: CostCalculationAlgorithm,
}

#[derive(Debug, Display, Error)]
pub(crate) enum DemandControlError {
    /// Query could not be parsed: {0}
    QueryParseFailure(String),
    /// The response body could not be properly matched with its query's structure: {0}
    ResponseTypingFailure(String),
}

impl<T> From<WithErrors<T>> for DemandControlError {
    fn from(value: WithErrors<T>) -> Self {
        DemandControlError::QueryParseFailure(format!("{}", value))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DemandControl {
    config: DemandControlConfig,
}

#[async_trait::async_trait]
impl Plugin for DemandControl {
    type Config = DemandControlConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(DemandControl {
            config: init.config,
        })
    }

    fn execution_service(&self, service: BoxService) -> BoxService {
        if !self.config.enabled {
            service
        } else {
            ServiceBuilder::new().service(service).boxed()
        }
    }
}

register_plugin!("apollo", "experimental_demand_control", DemandControl);
