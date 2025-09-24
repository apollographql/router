use multimap::MultiMap;
use tower::BoxError;
use tower::ServiceExt;

use crate::Endpoint;
use crate::ListenAddr;
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

/// Builder is responsible for:
/// 1. Deciding when to to reload telemetry
/// 2. Collating information from config
///
/// It will internally collect all the information using
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
        self.setup_logging();
        self.setup_public_tracing()?;
        self.setup_public_metrics()?;
        self.setup_apollo_metrics()?;
        self.setup_propagation();
        Ok((self.activation, self.endpoints, self.apollo_sender))
    }

    fn setup_public_metrics(&mut self) -> Result<(), BoxError> {
        if self.metrics_config_changed::<metrics::prometheus::Config>()
            || self.metrics_config_changed::<otlp::Config>()
        {
            ::tracing::info!("setting up metrics exporter");
            let mut builder = MetricsBuilder::new(self.config);
            builder.configure(&self.config.exporters.metrics.prometheus)?;
            builder.configure(&self.config.exporters.metrics.otlp)?;
            let (prometheus_registry, meter_providers) = builder.build();
            self.activation
                .with_prometheus_registry(prometheus_registry);
            self.activation.add_meter_providers(meter_providers);
        }
        // If we didn't change telemetry then we will get the old prom registry. Otherwise the new registry will take effect
        // The only time this will be None is if prometheus is not configured
        if let Some(prometheus_registry) = self.activation.prometheus_registry() {
            let listen = self.config.exporters.metrics.prometheus.listen.clone();
            let path = self.config.exporters.metrics.prometheus.path.clone();
            tracing::info!("Prometheus endpoint exposed at {}{}", listen, path);

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
        if self.tracing_config_changed::<otlp::Config>()
            || self.tracing_config_changed::<datadog::Config>()
            || self.tracing_config_changed::<zipkin::Config>()
            || self.tracing_config_changed::<apollo::Config>()
        {
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
        // There is no change detection for apollo metrics because we
        // have a custom sender and this MUST be populated on every reload
        let mut builder = MetricsBuilder::new(self.config);
        builder.configure(&self.config.apollo)?;
        Ok(())
    }

    /// Detects if metrics config has changed. This can be used for any implementation of `MetricsConfigurator`
    /// because they know how to get the config from the overall telemetry config.
    fn metrics_config_changed<T: MetricsConfigurator + PartialEq>(&self) -> bool {
        if let Some(previous_config) = self.previous_config {
            T::config(previous_config) != T::config(self.config)
                || previous_config.exporters.metrics.common != self.config.exporters.metrics.common
        } else {
            true
        }
    }

    /// Detects if tracing config has changed. This can be used for any implementation of `TracingConfigurator`
    /// because they know how to get the config from the overall telemetry config.
    fn tracing_config_changed<T: TracingConfigurator + PartialEq>(&self) -> bool {
        if let Some(previous_config) = self.previous_config {
            T::config(previous_config) != T::config(self.config)
                || previous_config.exporters.tracing.common != self.config.exporters.tracing.common
        } else {
            true
        }
    }

    fn setup_propagation(&mut self) {
        let propagators = create_propagator(
            &self.config.exporters.tracing.propagation,
            &self.config.exporters.tracing,
        );
        self.activation.with_tracer_propagator(propagators);
    }

    fn setup_logging(&mut self) {
        self.activation.with_logging(create_fmt_layer(self.config));
    }
}
