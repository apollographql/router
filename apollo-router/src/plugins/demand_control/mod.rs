//! Demand control plugin.
mod basic_cost_calculator;
mod directives;

use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::validation::Valid;
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
use crate::register_plugin;
use crate::services::execution;
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
    Basic,
}

trait CostCalculator {
    fn estimated(
        query: &ExecutableDocument,
        schema: &Valid<Schema>,
    ) -> Result<f64, DemandControlError>;

    fn actual(response: &graphql::Response) -> Result<f64, DemandControlError>;
}

/// Demand control configuration
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DemandControlConfig {
    /// Enable demand control
    enabled: bool,
    /// The algorithm used to calculate the cost of an incoming request
    #[allow(dead_code)]
    algorithm: CostCalculationAlgorithm,
}

#[derive(Debug, Display, Error)]
pub(crate) enum DemandControlError {
    /// Query could not be parsed: {0}
    QueryParseFailure(String),
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
            ServiceBuilder::new()
                .map_response(|res: execution::Response| {
                    res.map_stream(|graphql_res: graphql::Response| graphql_res)
                })
                .service(service)
                .boxed()
        }
    }
}

register_plugin!("apollo", "experimental_demand_control", DemandControl);
