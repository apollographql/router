//! Reload support for telemetry
//!
//! Telemetry reloading is complex because it modifies global OpenTelemetry state that must remain
//! consistent across the entire application. The challenge is that plugins may fail during initialization,
//! so we cannot safely modify global state at that time without risking leaving the system in an
//! inconsistent state if a later plugin fails.
//!
//! ## Lifecycle Overview
//!
//! The reload process follows a three-phase lifecycle:
//!
//! ### 1. Preparation Phase (`prepare`)
//! - Called during plugin initialization with the current and previous configurations
//! - The [`Builder`] detects what has changed by comparing configurations
//! - For each changed component (metrics, tracing, etc.), new providers are constructed
//! - An [`Activation`] object is created containing all the prepared state
//! - This phase is fallible - errors will prevent the reload
//!
//! ### 2. Activation Phase (`activate`)
//! - Called via `PluginPrivate::activate()` after all plugins are successfully initialized
//! - The [`Activation::commit()`] method atomically updates global state
//! - Old providers are safely shut down using blocking tasks
//! - This phase is infallible - by this point we're committed to the new configuration
//!
//! ### 3. Cleanup Phase (Drop)
//! - When activation is dropped (or if preparation fails), OpenTelemetry resources are cleaned up
//! - Meter and tracer providers perform blocking I/O in their Drop implementations
//! - These are moved to blocking tasks to avoid blocking async runtime threads
//!
//! ## Module Structure
//!
//! * [`otel`] - Global state management and initialization of the tracing subscriber
//! * [`activation`] - State container for the activation phase, handles safe provider replacement
//! * [`builder`] - Configuration change detection and construction of new providers
//! * [`metrics`] - Building meter providers from configuration
//! * [`tracing`] - Building trace providers from configuration
use multimap::MultiMap;
use tower::BoxError;

use crate::Endpoint;
use crate::ListenAddr;
use crate::plugins::telemetry::apollo_exporter;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::reload::activation::Activation;
use crate::plugins::telemetry::reload::builder::Builder;

pub(crate) mod activation;
pub(crate) mod builder;
pub(crate) mod metrics;
pub(crate) mod otel;
pub(crate) mod tracing;

/// Prepares telemetry components for activation (Phase 1 of reload lifecycle).
///
/// This is the entry point for the preparation phase. It:
/// 1. Detects configuration changes by comparing `previous_config` with `config`
/// 2. Constructs new providers only for components that have changed
/// 3. Returns an `Activation` object containing the prepared state
///
/// # Returns
///
/// A tuple containing:
/// - [`Activation`] - Prepared telemetry state to be activated later
/// - Prometheus endpoints (if Prometheus is enabled)
/// - Apollo metrics sender (if Apollo is configured)
///
/// # Errors
///
/// Returns an error if any provider construction fails. This prevents
/// the reload from proceeding to the activation phase.
pub(crate) fn prepare(
    previous_config: &Option<Conf>,
    config: &Conf,
) -> Result<
    (
        Activation,
        MultiMap<ListenAddr, Endpoint>,
        apollo_exporter::Sender,
    ),
    BoxError,
> {
    Builder::new(previous_config, config).build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::aggregation::MeterProviderType;
    use crate::plugins::telemetry::apollo;
    use crate::plugins::telemetry::config::Exporters;
    use crate::plugins::telemetry::config::Instrumentation;
    use crate::plugins::telemetry::config::Metrics;
    use crate::plugins::telemetry::config::Tracing;

    fn create_default_config() -> Conf {
        Conf {
            apollo: apollo::Config::default(),
            exporters: Exporters {
                metrics: Metrics {
                    common: Default::default(),
                    otlp: Default::default(),
                    prometheus: Default::default(),
                },
                tracing: Tracing::default(),
                logging: Default::default(),
            },
            instrumentation: Instrumentation::default(),
        }
    }

    fn create_config_with_prometheus() -> Conf {
        let mut config = create_default_config();
        config.exporters.metrics.prometheus.enabled = true;
        config.exporters.metrics.prometheus.listen =
            crate::ListenAddr::SocketAddr("127.0.0.1:9090".parse().unwrap());
        config.exporters.metrics.prometheus.path = "/metrics".to_string();
        config
    }

    fn create_config_with_apollo() -> Conf {
        let mut config = create_default_config();
        config.apollo.apollo_key = Some("test-key".to_string());
        config.apollo.apollo_graph_ref = Some("test@current".to_string());
        config
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_prepare_with_no_previous_config() {
        let config = create_default_config();

        let result = prepare(&None, &config);
        assert!(result.is_ok(), "prepare should succeed with default config");

        let (activation, endpoints, _sender) = result.unwrap();
        let instr = activation.test_instrumentation();

        // First run should set up basic telemetry
        assert!(
            instr.tracer_provider_set,
            "First run should set tracer provider"
        );
        assert!(
            instr.tracer_propagator_set,
            "First run should set propagator"
        );
        assert!(instr.logging_layer_set, "First run should set logging");

        // No endpoints should be created with default config
        assert!(
            endpoints.is_empty(),
            "No endpoints should be created with default config"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_prepare_with_prometheus_creates_endpoint() {
        let config = create_config_with_prometheus();

        let result = prepare(&None, &config);
        assert!(
            result.is_ok(),
            "prepare should succeed with prometheus config"
        );

        let (activation, endpoints, _sender) = result.unwrap();
        let instr = activation.test_instrumentation();

        // Should set up prometheus
        assert!(
            instr.prometheus_registry_set,
            "Should set prometheus registry"
        );
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Should add public meter provider"
        );

        // Should create prometheus endpoint
        assert!(!endpoints.is_empty(), "Should create prometheus endpoint");
        let listen_addr = crate::ListenAddr::SocketAddr("127.0.0.1:9090".parse().unwrap());
        assert!(
            endpoints.contains_key(&listen_addr),
            "Should create endpoint on correct address"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_prepare_with_apollo_creates_apollo_metrics() {
        let config = create_config_with_apollo();

        let result = prepare(&None, &config);
        assert!(result.is_ok(), "prepare should succeed with apollo config");

        let (activation, _endpoints, sender) = result.unwrap();
        let instr = activation.test_instrumentation();

        // Should set up apollo metrics and tracing
        assert!(
            instr.tracer_provider_set,
            "Should set tracer provider for apollo"
        );
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Apollo)
                || instr
                    .meter_providers_added
                    .contains(&MeterProviderType::ApolloRealtime),
            "Should add apollo meter providers"
        );

        // Should have apollo sender (not noop)
        if let apollo_exporter::Sender::Noop = sender {
            panic!("Should not be noop sender when apollo configured")
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_prepare_detects_config_changes() {
        let previous_config = create_default_config();
        let config = create_config_with_prometheus();

        let result = prepare(&Some(previous_config), &config);
        assert!(result.is_ok(), "prepare should succeed with config change");

        let (activation, endpoints, _sender) = result.unwrap();
        let instr = activation.test_instrumentation();

        // Should detect prometheus config change and reload metrics
        assert!(
            instr.prometheus_registry_set,
            "Should reload prometheus when config changes"
        );
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Should reload public meter provider"
        );

        // Should create new endpoint
        assert!(
            !endpoints.is_empty(),
            "Should create prometheus endpoint when enabled"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_prepare_no_reload_when_configs_identical() {
        let config = create_default_config();
        let previous_config = Some(config.clone());

        let result = prepare(&previous_config, &config);
        assert!(
            result.is_ok(),
            "prepare should succeed with identical configs"
        );

        let (activation, endpoints, _sender) = result.unwrap();
        let instr = activation.test_instrumentation();

        // Should not reload anything when configs are identical
        assert!(
            !instr.tracer_provider_set,
            "Should not reload tracer when configs identical"
        );
        assert!(
            !instr.prometheus_registry_set,
            "Should not reload prometheus when configs identical"
        );
        assert!(
            instr.meter_providers_added.is_empty(),
            "Should not add meter providers when configs identical"
        );

        // But always set logging and propagation
        assert!(instr.logging_layer_set, "Should always set logging");
        assert!(instr.tracer_propagator_set, "Should always set propagation");

        // No endpoints with default config
        assert!(endpoints.is_empty(), "No endpoints with default config");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_prepare_multiple_config_changes() {
        let previous_config = create_default_config();
        let mut config = create_config_with_prometheus();
        config.apollo.apollo_key = Some("test-key".to_string());
        config.apollo.apollo_graph_ref = Some("test@current".to_string());

        let result = prepare(&Some(previous_config), &config);
        assert!(
            result.is_ok(),
            "prepare should succeed with multiple changes"
        );

        let (activation, endpoints, sender) = result.unwrap();
        let instr = activation.test_instrumentation();

        // Should set up both prometheus and apollo
        assert!(instr.tracer_provider_set, "Should set tracer provider");
        assert!(
            instr.prometheus_registry_set,
            "Should set prometheus registry"
        );
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Should add public meter provider"
        );
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Apollo)
                || instr
                    .meter_providers_added
                    .contains(&MeterProviderType::ApolloRealtime),
            "Should add apollo meter providers"
        );

        // Should create prometheus endpoint and apollo sender
        assert!(!endpoints.is_empty(), "Should create prometheus endpoint");
        if let apollo_exporter::Sender::Noop = sender {
            panic!("Should not be noop sender when apollo configured")
        }
    }
}
