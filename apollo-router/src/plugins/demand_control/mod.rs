//! Demand control plugin.
//! This plugin will use the cost calculation algorithm to determine if a query should be allowed to execute.
//! On the request path it will use estimated
use std::future;
use std::ops::ControlFlow;
use std::sync::Arc;

use apollo_compiler::validation::{Valid, WithErrors};
use apollo_compiler::ExecutableDocument;
use displaydoc::Display;
use futures::{stream, StreamExt};
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::graphql::IntoGraphQLErrors;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::demand_control::strategy::Strategy;
use crate::plugins::demand_control::strategy::StrategyFactory;
use crate::services::execution;
use crate::services::execution::BoxService;
use crate::services::subgraph;
use crate::{graphql, register_plugin};

pub(crate) mod cost_calculator;
pub(crate) mod strategy;

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
        /// The maximum cost of a query
        max: f64,
    },
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Mode {
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
    /// Query estimated cost exceeded configured maximum
    EstimatedCostTooExpensive,
    /// Query actual cost exceeded configured maximum
    #[allow(dead_code)]
    ActualCostTooExpensive,
    /// Query could not be parsed: {0}
    QueryParseFailure(String),
    /// The response body could not be properly matched with its query's structure: {0}
    ResponseTypingFailure(String),
}

impl IntoGraphQLErrors for DemandControlError {
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self> {
        todo!()
    }
}

impl<T> From<WithErrors<T>> for DemandControlError {
    fn from(value: WithErrors<T>) -> Self {
        DemandControlError::QueryParseFailure(format!("{}", value))
    }
}

pub(crate) struct DemandControl {
    config: DemandControlConfig,
    strategy_factory: StrategyFactory,
}

#[async_trait::async_trait]
impl Plugin for DemandControl {
    type Config = DemandControlConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(DemandControl {
            strategy_factory: StrategyFactory::new(
                init.config.clone(),
                init.supergraph_schema.clone(),
                init.subgraph_schemas.clone(),
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
                    req.context.extensions().lock().insert(strategy.clone());
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
                    let strategy = resp
                        .context
                        .extensions()
                        .lock()
                        .get::<Strategy>()
                        .expect("must have strategy")
                        .clone();
                    resp.response = resp.response.map(move |resp| {
                        // Here we are going to abort the stream if the cost is too high
                        // First we map based on cost, then we use take while
                        resp.flat_map(move |resp| {
                            match strategy.on_execution_response(req.as_ref(), &resp) {
                                Ok(_) => stream::iter(vec![Ok(resp)]),
                                Err(err) => stream::iter(vec![
                                    Ok(graphql::Response::builder()
                                        .errors(
                                            err.into_graphql_errors()
                                                .expect("must be able to convert to graphql error"),
                                        )
                                        .extensions(crate::json_ext::Object::new())
                                        .build()),
                                    Err(()),
                                ]),
                            }
                        })
                        .take_while(|resp| future::ready(resp.is_ok()))
                        .map(|i| i.expect("error used to terminate stream"))
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
        _subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        if !self.config.enabled {
            service
        } else {
            ServiceBuilder::new()
                .checkpoint(move |req: subgraph::Request| {
                    let strategy = req
                        .context
                        .extensions()
                        .lock()
                        .get::<Strategy>()
                        .expect("must have strategy")
                        .clone();

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
                                .build(),
                        ),
                    })
                })
                .map_future_with_request_data(
                    |req: &subgraph::Request| {
                        req.subgraph_request_document
                            .clone()
                            .expect("must have document")
                    },
                    |req: Arc<Valid<ExecutableDocument>>, fut| async move {
                        let resp: subgraph::Response = fut.await?;
                        let strategy = resp
                            .context
                            .extensions()
                            .lock()
                            .get::<Strategy>()
                            .expect("must have strategy")
                            .clone();
                        Ok(match strategy.on_subgraph_response(req.as_ref(), &resp) {
                            Ok(_) => resp,
                            Err(err) => subgraph::Response::builder()
                                .errors(
                                    err.into_graphql_errors()
                                        .expect("must be able to convert to graphql error"),
                                )
                                .context(resp.context.clone())
                                .extensions(crate::json_ext::Object::new())
                                .build(),
                        })
                    },
                )
                .service(service)
                .boxed()
        }
    }
}

register_plugin!("apollo", "experimental_demand_control", DemandControl);

#[cfg(test)]
mod test {
    use crate::plugins::demand_control::DemandControl;
    use crate::plugins::test::PluginTestHarness;

    #[test]
    fn test_measure() {
        let _plugin = PluginTestHarness::<DemandControl>::builder()
            .config(include_str!("fixtures/measure.router.yaml"))
            .build();
    }

    #[test]
    fn test_enforce() {
        let _plugin = PluginTestHarness::<DemandControl>::builder()
            .config(include_str!("fixtures/enforce.router.yaml"))
            .build();
    }
}
