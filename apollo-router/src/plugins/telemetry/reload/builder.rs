//! Telemetry configuration change detection and provider construction
//!
//! This module provides the [`Builder`] which orchestrates the preparation phase of telemetry reloading.
//!
//! ## Purpose
//!
//! The builder is responsible for:
//! 1. **Change detection** - Comparing previous and current configurations to determine what needs reloading
//! 2. **Provider construction** - Building new OpenTelemetry providers only when necessary
//! 3. **State collection** - Gathering all prepared components into an [`Activation`] for the activation phase
//!
//! ## Change Detection
//!
//! The builder uses trait-based configuration access ([`MetricsConfigurator`], [`TracingConfigurator`])
//! to detect changes in specific exporter configurations. This allows it to:
//! - Reload only the components that have changed
//! - Preserve existing providers when configuration is unchanged
//! - Handle both common settings (service name, resource attributes) and exporter-specific settings
//!
//! ## Construction Safety
//!
//! External exporters may perform blocking I/O during construction, so the entire build process
//! runs in [`block_in_place`] to avoid blocking the async runtime.

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use multimap::MultiMap;
use tokio::task::block_in_place;
use tower::BoxError;
use tower::ServiceExt;

use crate::Endpoint;
use crate::ListenAddr;
use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::apollo;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::fmt_layer::create_fmt_layer;
use crate::plugins::telemetry::metrics;
use crate::plugins::telemetry::metrics::prometheus::PrometheusService;
use crate::plugins::telemetry::otlp;
use crate::plugins::telemetry::reload::activation::Activation;
use crate::plugins::telemetry::reload::metrics::MetricsBuilder;
use crate::plugins::telemetry::reload::metrics::MetricsConfigurator;
use crate::plugins::telemetry::reload::tracing::TracingBuilder;
use crate::plugins::telemetry::reload::tracing::TracingConfigurator;
use crate::plugins::telemetry::reload::tracing::create_propagator;
use crate::plugins::telemetry::tracing::datadog;
use crate::plugins::telemetry::tracing::zipkin;

/// Static counter for tracking Prometheus reload calls.
static PROMETHEUS_CALL_COUNT: AtomicUsize = AtomicUsize::new(1);

/// Orchestrates telemetry reload preparation by detecting configuration changes
/// and constructing new providers as needed.
pub(super) struct Builder<'a> {
    previous_config: &'a Option<Conf>,
    config: &'a Conf,
    activation: Activation,
    endpoints: MultiMap<ListenAddr, Endpoint>,
    apollo_sender: Sender,
}

impl<'a> Builder<'a> {
    pub(super) fn new(previous_config: &'a Option<Conf>, config: &'a Conf) -> Self {
        Self {
            previous_config,
            config,
            activation: Activation::new(),
            endpoints: Default::default(),
            apollo_sender: Sender::Noop,
        }
    }

    pub(super) fn build(
        mut self,
    ) -> Result<(Activation, MultiMap<ListenAddr, Endpoint>, Sender), BoxError> {
        // We can't guarantee that exporters from external libraries will not perform blocking io during or after construction.
        // Use block_in_place to avoid any chance of blocking the main rt threads
        block_in_place(|| {
            self.setup_logging();
            self.setup_public_tracing()?;
            self.setup_public_metrics()?;
            self.setup_apollo_metrics()?;
            self.setup_propagation()?;
            Ok((self.activation, self.endpoints, self.apollo_sender))
        })
    }

    fn setup_public_metrics(&mut self) -> Result<(), BoxError> {
        if self.is_metrics_config_changed::<metrics::prometheus::Config>()
            || self.is_metrics_config_changed::<otlp::Config>()
            || self.prometheus_force_change()
        {
            ::tracing::debug!("configuring metrics");
            let mut builder = MetricsBuilder::new(self.config);
            builder.configure(&self.config.exporters.metrics.prometheus)?;
            builder.configure(&self.config.exporters.metrics.otlp)?;
            // Register memory allocation views with custom buckets
            crate::plugins::telemetry::metrics::allocation::register_memory_allocation_views(
                &mut builder,
            );
            builder.configure_views(MeterProviderType::Public)?;

            let (prometheus_registry, meter_providers, _) = builder.build();
            self.activation
                .with_prometheus_registry(prometheus_registry);
            self.activation.add_meter_providers(meter_providers);
        }
        // Always create Prometheus endpoint if we have a registry (either new or existing).
        // The activation holds either the newly created registry or the previous one if unchanged.
        // This will be None only if Prometheus has never been configured.
        if let Some(prometheus_registry) = self.activation.prometheus_registry() {
            let listen = self.config.exporters.metrics.prometheus.listen.clone();
            let path = self.config.exporters.metrics.prometheus.path.clone();
            tracing::info!("Prometheus endpoint exposed at {}{}", &listen, &path);
            self.endpoints.insert(
                listen,
                Endpoint::from_router_service(
                    path,
                    PrometheusService {
                        registry: prometheus_registry.clone(),
                    }
                    .boxed(),
                ),
            );
        }
        Ok(())
    }

    fn setup_public_tracing(&mut self) -> Result<(), BoxError> {
        if self.is_tracing_config_changed::<otlp::Config>()
            || self.is_tracing_config_changed::<datadog::Config>()
            || self.is_tracing_config_changed::<zipkin::Config>()
            || self.is_tracing_config_changed::<apollo::Config>()
        {
            ::tracing::debug!("configuring tracing");
            let mut builder = TracingBuilder::new(self.config);
            builder.configure(&self.config.exporters.tracing.otlp)?;
            builder.configure(&self.config.exporters.tracing.zipkin)?;
            builder.configure(&self.config.exporters.tracing.datadog)?;
            builder.configure(&self.config.apollo)?;

            self.activation.with_tracer_provider(builder.build())
        }
        Ok(())
    }

    fn setup_apollo_metrics(&mut self) -> Result<(), BoxError> {
        ::tracing::debug!("configuring Apollo metrics");
        // Apollo metrics are always rebuilt (no change detection) because the sender
        // needs to be populated on every reload. The sender cannot be stored globally
        // and must be returned from the prepare phase.
        let mut builder = MetricsBuilder::new(self.config);
        builder.configure(&self.config.apollo)?;
        let (_, meter_providers, sender) = builder.build();
        self.activation.add_meter_providers(meter_providers);
        self.apollo_sender = sender;
        Ok(())
    }

    /// Detects if metrics config has changed for a specific exporter.
    ///
    /// Returns `true` if:
    /// - This is the first run (no previous config)
    /// - The exporter-specific config has changed
    /// - Common metrics settings (service name, resource attributes, etc.) have changed
    fn is_metrics_config_changed<T: MetricsConfigurator + PartialEq>(&self) -> bool {
        let Some(previous_config) = self.previous_config else {
            return true;
        };
        T::config(previous_config) != T::config(self.config)
            || previous_config.exporters.metrics.common != self.config.exporters.metrics.common
    }

    /// Detects if tracing config has changed for a specific exporter.
    ///
    /// Returns `true` if:
    /// - This is the first run (no previous config)
    /// - The exporter-specific config has changed
    /// - Common tracing settings (service name, sampler, span limits, etc.) have changed
    fn is_tracing_config_changed<T: TracingConfigurator + PartialEq>(&self) -> bool {
        let Some(previous_config) = self.previous_config else {
            return true;
        };
        T::config(previous_config) != T::config(self.config)
            || previous_config.exporters.tracing.common != self.config.exporters.tracing.common
    }

    fn setup_propagation(&mut self) -> Result<(), BoxError> {
        let propagators = create_propagator(
            &self.config.exporters.tracing.propagation,
            &self.config.exporters.tracing,
        )?;
        self.activation.with_tracer_propagator(propagators);
        Ok(())
    }

    fn setup_logging(&mut self) {
        ::tracing::debug!("configuring logging");
        self.activation.with_logging(create_fmt_layer(self.config));
    }

    /// Returns true every 20 calls when Prometheus is enabled.
    ///
    /// This helps prevent accumulation of stale metrics in the Prometheus registry.
    /// Once a series enters the Prometheus registry it is never dropped, so metrics
    /// with labels that are unique (like launch_id) can accumulate as zeroed
    /// out series that will never be incremented again.
    ///
    /// The true solution for this is to drop Prometheus support as this has been dropped in upstream OTEL.
    /// Note that this doesn't actually hurt Prometheus metrics as these are OK to be recreated at any time,
    /// but things like connections won't be visible across reloads.
    fn prometheus_force_change(&self) -> bool {
        if !metrics::prometheus::Config::config(self.config).enabled {
            return false;
        }
        let count = PROMETHEUS_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
        count.is_multiple_of(20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::telemetry::apollo;
    use crate::plugins::telemetry::config::Exporters;
    use crate::plugins::telemetry::config::Instrumentation;
    use crate::plugins::telemetry::config::Metrics;
    use crate::plugins::telemetry::config::Propagation;
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

    fn create_config_with_prometheus_enabled() -> Conf {
        let mut config = create_default_config();
        config.exporters.metrics.prometheus.enabled = true;
        config
    }

    fn create_config_with_otlp_metrics_enabled() -> Conf {
        let mut config = create_default_config();
        config.exporters.metrics.otlp.enabled = true;
        config
    }

    fn create_config_with_otlp_tracing_enabled() -> Conf {
        let mut config = create_default_config();
        config.exporters.tracing.otlp.enabled = true;
        config
    }

    fn create_config_with_apollo_enabled() -> Conf {
        let mut config = create_default_config();
        config.apollo.apollo_key = Some("test-key".to_string());
        config.apollo.apollo_graph_ref = Some("test@current".to_string());
        config
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_no_reload_when_configs_identical() {
        let config = create_default_config();
        let previous_config = Some(config.clone());

        let builder = Builder::new(&previous_config, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        // When configs are identical, only certain things should be set
        let instr = activation.test_instrumentation();
        assert!(
            !instr.tracer_provider_set,
            "Tracer provider should not reload when configs identical"
        );
        assert!(
            !instr.prometheus_registry_set,
            "Prometheus registry should not reload when configs identical"
        );
        // Apollo metrics should not be added when not configured (no apollo key/graph ref)
        assert!(
            instr.meter_providers_added.is_empty(),
            "No meter providers should be added when configs identical and apollo not configured"
        );
        // Logging and propagation always get set
        assert!(instr.logging_layer_set, "Logging should always be set");
        assert!(
            instr.tracer_propagator_set,
            "Propagator should always be set"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_metrics_reload_on_prometheus_change() {
        let previous_config = Some(create_default_config());
        let config = create_config_with_prometheus_enabled();

        let builder = Builder::new(&previous_config, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        // Prometheus config changed, so metrics should reload
        assert!(
            instr.prometheus_registry_set,
            "Prometheus registry should be set when config changes"
        );
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Public meter provider should be added"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_metrics_reload_on_otlp_change() {
        let previous_config = Some(create_default_config());
        let config = create_config_with_otlp_metrics_enabled();

        let builder = Builder::new(&previous_config, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        // OTLP metrics config changed, so metrics should reload
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Public meter provider should be added when OTLP metrics changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing_reload_on_otlp_change() {
        let previous_config = Some(create_default_config());
        let config = create_config_with_otlp_tracing_enabled();

        let builder = Builder::new(&previous_config, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        // OTLP tracing config changed, so tracing should reload
        assert!(
            instr.tracer_provider_set,
            "Tracer provider should be set when OTLP tracing changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_apollo_metrics_always_rebuild_when_enabled() {
        let config = create_config_with_apollo_enabled();
        let previous_config = Some(config.clone());

        let builder = Builder::new(&previous_config, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        // Apollo metrics should always rebuild when apollo is configured
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Apollo)
                || instr
                    .meter_providers_added
                    .contains(&MeterProviderType::ApolloRealtime),
            "Apollo metrics should always rebuild when apollo is configured"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_first_run_builds_everything() {
        let config = create_default_config();
        let previous_config = None; // First run, no previous config

        let builder = Builder::new(&previous_config, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        // First run should build everything
        assert!(
            instr.tracer_provider_set,
            "First run should build tracer provider"
        );
        assert!(
            instr.tracer_propagator_set,
            "First run should set tracer propagator"
        );
        assert!(
            instr.logging_layer_set,
            "First run should set logging layer"
        );
        // But no meter providers get added if nothing is configured
        assert!(
            instr.meter_providers_added.is_empty(),
            "No meter providers added on first run when nothing enabled"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_first_run_with_apollo_enabled() {
        let config = create_config_with_apollo_enabled();
        let previous_config = None; // First run, no previous config

        let builder = Builder::new(&previous_config, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        // First run with apollo enabled should build apollo meters
        assert!(
            instr.tracer_provider_set,
            "First run should build tracer provider"
        );
        assert!(
            instr.tracer_propagator_set,
            "First run should set tracer propagator"
        );
        assert!(
            instr.logging_layer_set,
            "First run should set logging layer"
        );
        // Apollo meter providers should be added on first run when apollo is enabled
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Apollo)
                || instr
                    .meter_providers_added
                    .contains(&MeterProviderType::ApolloRealtime),
            "First run should add apollo meter providers when apollo enabled"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_metrics_common_change_triggers_reload() {
        let previous_config = create_config_with_prometheus_enabled();
        let mut config = create_config_with_prometheus_enabled();
        config.exporters.metrics.common.service_name = Some("new-service".to_string());

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        // Common config changed, so metrics should reload even when only common settings change
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Public meter provider should reload when common config changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing_common_change_triggers_reload() {
        let previous_config = create_config_with_otlp_tracing_enabled();
        let mut config = create_config_with_otlp_tracing_enabled();
        config.exporters.tracing.common.service_name = Some("new-service".to_string());

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        // Common config changed, so tracing should reload even when only common settings change
        assert!(
            instr.tracer_provider_set,
            "Tracer provider should reload when common config changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_metrics_common_service_namespace_change() {
        let previous_config = create_config_with_prometheus_enabled();
        let mut config = create_config_with_prometheus_enabled();
        config.exporters.metrics.common.service_namespace = Some("new-namespace".to_string());

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Public meter provider should reload when service_namespace changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_metrics_common_resource_change() {
        let previous_config = create_config_with_prometheus_enabled();
        let mut config = create_config_with_prometheus_enabled();
        config.exporters.metrics.common.resource.insert(
            "deployment.environment".to_string(),
            crate::plugins::telemetry::config::AttributeValue::String("staging".to_string()),
        );

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Public meter provider should reload when resource attributes change"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_metrics_common_buckets_change() {
        let previous_config = create_config_with_prometheus_enabled();
        let mut config = create_config_with_prometheus_enabled();
        config.exporters.metrics.common.buckets = vec![0.1, 0.5, 1.0, 2.5, 5.0, 10.0];

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        assert!(
            instr
                .meter_providers_added
                .contains(&MeterProviderType::Public),
            "Public meter provider should reload when histogram buckets change"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing_common_service_namespace_change() {
        let previous_config = create_config_with_otlp_tracing_enabled();
        let mut config = create_config_with_otlp_tracing_enabled();
        config.exporters.tracing.common.service_namespace = Some("new-namespace".to_string());

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        assert!(
            instr.tracer_provider_set,
            "Tracer provider should reload when service_namespace changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing_common_sampler_change() {
        let previous_config = create_config_with_otlp_tracing_enabled();
        let mut config = create_config_with_otlp_tracing_enabled();
        config.exporters.tracing.common.sampler =
            crate::plugins::telemetry::config::SamplerOption::TraceIdRatioBased(0.5);

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        assert!(
            instr.tracer_provider_set,
            "Tracer provider should reload when sampler changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing_common_parent_based_sampler_change() {
        let previous_config = create_config_with_otlp_tracing_enabled();
        let mut config = create_config_with_otlp_tracing_enabled();
        config.exporters.tracing.common.parent_based_sampler = false;

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        assert!(
            instr.tracer_provider_set,
            "Tracer provider should reload when parent_based_sampler changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing_common_span_limits_change() {
        let previous_config = create_config_with_otlp_tracing_enabled();
        let mut config = create_config_with_otlp_tracing_enabled();
        config.exporters.tracing.common.max_events_per_span = 256;
        config.exporters.tracing.common.max_attributes_per_span = 64;

        let previous_config_opt = Some(previous_config);
        let builder = Builder::new(&previous_config_opt, &config);
        let (activation, _endpoints, _sender) = builder.build().unwrap();

        let instr = activation.test_instrumentation();
        assert!(
            instr.tracer_provider_set,
            "Tracer provider should reload when span limits change"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_propagation_only_passes() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert!(builder.build().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_with_baggage_propagation_passes() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            baggage: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert!(builder.build().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_jaeger_propagation_only_passes() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.propagation = Propagation {
            jaeger: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert!(builder.build().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_with_jaeger_propagation_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            jaeger: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "datadog propagation cannot be used with any other propagator except for baggage",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_with_trace_context_propagation_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            trace_context: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "datadog propagation cannot be used with any other propagator except for baggage",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_with_zipkin_propagation_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            zipkin: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "datadog propagation cannot be used with any other propagator except for baggage",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_with_aws_xray_propagation_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            aws_xray: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "datadog propagation cannot be used with any other propagator except for baggage",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_datadog_propagation_only_passes() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert!(builder.build().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_datadog_baggage_propagation_passes() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            baggage: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert!(builder.build().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_jaeger_propagation_only_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.propagation = Propagation {
            jaeger: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "datadog propagation must be explicitly disabled if the datadog exporter is enabled and any propagator other than baggage is enabled",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_datadog_jaeger_propagation_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            jaeger: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "if the datadog exporter is enabled and any other propagator is enabled, the datadog propagator must be disabled",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_datadog_trace_context_propagation_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            trace_context: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "if the datadog exporter is enabled and any other propagator is enabled, the datadog propagator must be disabled",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_datadog_zipkin_propagation_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            zipkin: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "if the datadog exporter is enabled and any other propagator is enabled, the datadog propagator must be disabled",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_datadog_aws_xray_propagation_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.propagation = Propagation {
            datadog: Some(true),
            aws_xray: true,
            ..Default::default()
        };

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "if the datadog exporter is enabled and any other propagator is enabled, the datadog propagator must be disabled",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_otlp_exporter_enabled_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.otlp.enabled = true;

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "datadog propagation must be explicitly disabled if the datadog exporter is enabled and any propagator other than baggage is enabled",
            builder.build().err().unwrap().to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_datadog_exporter_enabled_with_zipkin_exporter_enabled_fails() {
        let mut config = create_config_with_apollo_enabled();
        config.exporters.tracing.datadog.enabled = true;
        config.exporters.tracing.zipkin.enabled = true;

        let builder = Builder::new(&None, &config);
        assert_eq!(
            "datadog propagation must be explicitly disabled if the datadog exporter is enabled and any propagator other than baggage is enabled",
            builder.build().err().unwrap().to_string()
        );
    }
}
