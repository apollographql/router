use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use failsafe::backoff::Constant;
use failsafe::failure_policy::{success_rate_over_time_window, SuccessRateOverTimeWindow};
use failsafe::{backoff, CircuitBreaker, Config, Instrument, StateMachine};
use graphql::Error as GraphQLError;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Map;
use tower::{BoxError, ServiceBuilder, ServiceExt};

use crate::layers::ServiceBuilderExt;
use crate::plugin::{Plugin, PluginInit};
use crate::services::subgraph::{BoxService, Request, Response};
use crate::{graphql, register_plugin};

const ERROR_EXTENSION_CODE: &str = "CIRCUIT_BREAKER_OPEN";

type SubgraphCircuitBreaker =
    StateMachine<SuccessRateOverTimeWindow<Constant>, SubgraphCircuitBreakerHooks>;

const fn default_success_rate() -> f64 {
    0.8
}
const fn default_minimum_requests() -> u32 {
    5
}
const fn default_success_rate_window_seconds() -> u64 {
    5
}
const fn default_constant_backoff_seconds() -> u64 {
    30
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PluginConfiguration {
    enabled: bool,
    #[serde(default)]
    circuit_breaker_configuration: CircuitBreakerConfiguration,
    #[serde(default)]
    subgraphs: Target,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CircuitBreakerConfiguration {
    #[serde(default = "default_success_rate")]
    success_rate: f64,
    #[serde(default = "default_minimum_requests")]
    minimum_requests: u32,
    #[serde(default = "default_success_rate_window_seconds")]
    success_rate_window_seconds: u64,
    #[serde(default = "default_constant_backoff_seconds")]
    constant_backoff_seconds: u64,
}

impl Default for CircuitBreakerConfiguration {
    fn default() -> Self {
        Self {
            success_rate: default_success_rate(),
            minimum_requests: default_minimum_requests(),
            success_rate_window_seconds: default_success_rate_window_seconds(),
            constant_backoff_seconds: default_constant_backoff_seconds(),
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
enum Target {
    #[default]
    All,
    AllWithOverrides(HashMap<String, CircuitBreakerConfiguration>),
    Only(Vec<String>),
    OnlyWithOverrides(HashMap<String, CircuitBreakerConfiguration>),
    Except(Vec<String>),
}

struct SubgraphCircuitBreakerHooks {
    subgraph_name: String,
}

impl Instrument for SubgraphCircuitBreakerHooks {
    fn on_call_rejected(&self) {
        tracing::error!(
            message = "Call rejected, circuit breaker open",
            service = self.subgraph_name,
        )
    }
    fn on_open(&self) {
        tracing::error!(
            message = "circuit breaker transitioned to OPEN state",
            service = self.subgraph_name,
        )
    }
    fn on_half_open(&self) {
        tracing::error!(
            message = "circuit breaker transitioned to HALF_OPEN state",
            service = self.subgraph_name,
        )
    }
    fn on_closed(&self) {
        tracing::info!(
            message = "circuit breaker transitioned to CLOSED state",
            service = self.subgraph_name,
        )
    }
}

#[derive(Debug)]
struct SubgraphCircuitBreakerPlugin {
    #[allow(dead_code)]
    configuration: PluginConfiguration,
    circuit_breakers: DashMap<String, Arc<SubgraphCircuitBreaker>>,
}

impl SubgraphCircuitBreakerPlugin {
    fn checkpoint(&self, subgraph_name: &str) -> Option<Arc<SubgraphCircuitBreaker>> {
        if !&self.configuration.enabled {
            return None;
        }

        let subgraph = subgraph_name.to_string();

        match &self.configuration.subgraphs {
            Target::All | Target::AllWithOverrides(_) => Some(subgraph),
            Target::Only(only) if only.contains(&subgraph) => Some(subgraph),
            Target::OnlyWithOverrides(only) if only.contains_key(&subgraph) => Some(subgraph),
            Target::Except(except) if !except.contains(&subgraph) => Some(subgraph),
            _ => None
        }.map(|subgraph_name| self.get_circuit_breaker(&subgraph_name))
    }

    fn get_circuit_breaker(&self, subgraph_name: &str) -> Arc<SubgraphCircuitBreaker> {
        self.circuit_breakers
            .entry(subgraph_name.to_string())
            .or_insert_with(|| {
                let configuration = match &self.configuration.subgraphs {
                    Target::All | Target::Only(_) | Target::Except(_) => {
                        &self.configuration.circuit_breaker_configuration
                    }
                    Target::AllWithOverrides(all) | Target::OnlyWithOverrides(all) => &all
                        .get(&subgraph_name.to_string())
                        .unwrap_or(&self.configuration.circuit_breaker_configuration),
                };

                Arc::new(
                    Config::new()
                        .failure_policy(success_rate_over_time_window(
                            configuration.success_rate,
                            configuration.minimum_requests,
                            Duration::from_secs(configuration.success_rate_window_seconds),
                            backoff::constant(Duration::from_secs(
                                configuration.constant_backoff_seconds,
                            )),
                        ))
                        .instrument(SubgraphCircuitBreakerHooks {
                            subgraph_name: subgraph_name.to_string(),
                        })
                        .build(),
                )
            })
            .clone()
    }
}

#[async_trait::async_trait]
impl Plugin for SubgraphCircuitBreakerPlugin {
    type Config = PluginConfiguration;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(SubgraphCircuitBreakerPlugin {
            configuration: init.config,
            circuit_breakers: DashMap::new(),
        })
    }

    fn subgraph_service(&self, subgraph_name: &str, service: BoxService) -> BoxService {
        let checkpoint = self.checkpoint(subgraph_name);
        if checkpoint.is_none() {
            return service
        }

        let circuit_breaker = checkpoint.unwrap();
        let is_call_permitted = circuit_breaker.is_call_permitted();
        let service_name = subgraph_name.to_string();

        ServiceBuilder::new()
            .checkpoint(move |req: Request| {
                if is_call_permitted {
                    return Ok(ControlFlow::Continue(req));
                }
                Ok(ControlFlow::Break(
                    Response::builder()
                        .status_code(StatusCode::SERVICE_UNAVAILABLE)
                        .error(
                            GraphQLError::builder()
                                .message("Circuit breaker open")
                                .extension_code(ERROR_EXTENSION_CODE)
                                .extensions({
                                    let mut extensions = Map::new();
                                    extensions.insert("subgraph", service_name.clone().into());
                                    extensions
                                })
                                .build()
                        )
                        .context(req.context)
                        .extensions(Map::new())
                        .build()
                ))
            })
            .map_response(move |res: Response| {
                match circuit_breaker.call(|| {
                    if res.response.status().is_success() {
                        Ok(res.response.status())
                    } else {
                        Err(res.response.status())
                    }
                }) {
                    _ => res
                }
            })
            .service(service)
            .boxed()
    }
}

register_plugin!(
    "experimental",
    "subgraph_circuit_breaker",
    SubgraphCircuitBreakerPlugin
);
