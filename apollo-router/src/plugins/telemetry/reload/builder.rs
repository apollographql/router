use ahash::HashMap;
use multimap::MultiMap;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::MeterProviderBuilder;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::metrics::View;
use opentelemetry_sdk::trace::SpanProcessor;
use opentelemetry_sdk::trace::TracerProvider;
use prometheus::Registry;
use tower::BoxError;
use tower::ServiceExt;

use crate::_private::telemetry::ConfigResource;
use crate::Endpoint;
use crate::ListenAddr;
use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::plugins::telemetry::CustomTraceIdPropagator;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::fmt_layer::create_fmt_layer;
use crate::plugins::telemetry::metrics;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::metrics::prometheus::PrometheusService;
use crate::plugins::telemetry::otlp;
use crate::plugins::telemetry::reload::activation::Activation;
use crate::plugins::telemetry::tracing::TracingConfigurator;
use crate::plugins::telemetry::tracing::datadog;
use crate::plugins::telemetry::tracing::zipkin;

/// This builder is responsible for collecting
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
            self.activation
                .with_prometheus_registry(builder.prometheus_registry.clone());
            self.activation
                .add_meter_providers(builder.meter_providers());
        }
        // If we didn't change telemetry then we will get the old prom registry. Otherwise the new registry will take effect
        if let Some(prometheus_registry) = self.activation.prometheus_registry() {
            ::tracing::info!("setting up prometheus registry");

            self.endpoints.insert(
                self.config.exporters.metrics.prometheus.listen.clone(),
                Endpoint::from_router_service(
                    self.config.exporters.metrics.prometheus.path.clone(),
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
        let mut builder = MetricsBuilder::new(self.config);
        builder.configure(&self.config.apollo)?;
        Ok(())
    }

    fn metrics_config_changed<T: MetricsConfigurator + PartialEq>(&self) -> bool {
        if let Some(previous_config) = self.previous_config {
            T::config(previous_config) != T::config(self.config)
                || previous_config.exporters.metrics.common != self.config.exporters.metrics.common
        } else {
            true
        }
    }

    fn tracing_config_changed<T: TracingConfigurator + PartialEq>(&self) -> bool {
        if let Some(previous_config) = self.previous_config {
            T::config(previous_config) != T::config(self.config)
                || previous_config.exporters.tracing.common != self.config.exporters.tracing.common
        } else {
            true
        }
    }

    fn setup_propagation(&mut self) {
        let propagation = &self.config.exporters.tracing.propagation;

        let tracing = &self.config.exporters.tracing;

        let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync + 'static>> = Vec::new();
        if propagation.jaeger {
            propagators.push(Box::<opentelemetry_jaeger_propagator::Propagator>::default());
        }
        if propagation.baggage {
            propagators.push(Box::<opentelemetry_sdk::propagation::BaggagePropagator>::default());
        }
        if propagation.trace_context || tracing.otlp.enabled {
            propagators
                .push(Box::<opentelemetry_sdk::propagation::TraceContextPropagator>::default());
        }
        if propagation.zipkin || tracing.zipkin.enabled {
            propagators.push(Box::<opentelemetry_zipkin::Propagator>::default());
        }
        if propagation.datadog || tracing.datadog.enabled {
            propagators.push(Box::<
                crate::plugins::telemetry::tracing::datadog_exporter::DatadogPropagator,
            >::default());
        }
        if propagation.aws_xray {
            propagators.push(Box::<opentelemetry_aws::trace::XrayPropagator>::default());
        }

        // This propagator MUST come last because the user is trying to override the default behavior of the
        // other propagators.
        if let Some(from_request_header) = &propagation.request.header_name {
            propagators.push(Box::new(CustomTraceIdPropagator::new(
                from_request_header.to_string(),
                propagation.request.format.clone(),
            )));
        }

        self.activation
            .with_tracer_propagator(TextMapCompositePropagator::new(propagators));
    }

    fn setup_logging(&mut self) {
        self.activation.with_logging(create_fmt_layer(self.config));
    }
}

pub(crate) struct TracingBuilder<'a> {
    common: &'a TracingCommon,
    spans: &'a Spans,
    builder: opentelemetry_sdk::trace::Builder,
}

impl<'a> TracingBuilder<'a> {
    fn new(config: &'a Conf) -> Self {
        Self {
            common: &config.exporters.tracing.common,
            spans: &config.instrumentation.spans,
            builder: opentelemetry_sdk::trace::TracerProvider::builder()
                .with_config((&config.exporters.tracing.common).into()),
        }
    }
    fn configure<T: TracingConfigurator>(&mut self, config: &T) -> Result<(), BoxError> {
        if config.enabled() {
            return config.apply(self);
        }
        Ok(())
    }

    pub(crate) fn tracing_common(&self) -> &TracingCommon {
        self.common
    }
    pub(crate) fn spans(&self) -> &Spans {
        self.spans
    }

    pub(crate) fn with_span_processor<T: SpanProcessor + 'static>(&mut self, span_processor: T) {
        let builder = std::mem::take(&mut self.builder);
        self.builder = builder.with_span_processor(span_processor);
    }
    pub(crate) fn build(self) -> TracerProvider {
        self.builder.build()
    }
}

pub(crate) struct MetricsBuilder<'a> {
    meter_provider_builders:
        HashMap<MeterProviderType, opentelemetry_sdk::metrics::MeterProviderBuilder>,
    apollo_metrics_sender: Sender,
    prometheus_registry: Option<Registry>,
    metrics_common: &'a MetricsCommon,
    resource: Resource,
}

impl<'a> MetricsBuilder<'a> {
    pub(crate) fn meter_providers(
        self,
    ) -> impl Iterator<Item = (MeterProviderType, FilterMeterProvider)> {
        self.meter_provider_builders.into_iter().map(|(k, v)| {
            (
                k,
                match k {
                    MeterProviderType::Public => FilterMeterProvider::public(v.build()),
                    MeterProviderType::OtelDefault => FilterMeterProvider::public(v.build()),
                    MeterProviderType::Apollo => FilterMeterProvider::apollo(v.build()),
                    MeterProviderType::ApolloRealtime => {
                        FilterMeterProvider::apollo_realtime(v.build())
                    }
                },
            )
        })
    }
    fn configure<T: MetricsConfigurator>(&mut self, config: &T) -> Result<(), BoxError> {
        if config.enabled() {
            return config.apply(self);
        }
        Ok(())
    }
    pub(crate) fn new(config: &'a Conf) -> Self {
        let resource = config.exporters.metrics.common.to_resource();

        Self {
            meter_provider_builders: HashMap::default(),
            resource,
            apollo_metrics_sender: Sender::default(),
            prometheus_registry: None,
            metrics_common: &config.exporters.metrics.common,
        }
    }
    pub(crate) fn metrics_common(&self) -> &MetricsCommon {
        self.metrics_common
    }
    pub(crate) fn with_prometheus_registry(&mut self, prometheus_registry: Registry) -> &mut Self {
        self.prometheus_registry = Some(prometheus_registry);
        self
    }
    pub(crate) fn with_apollo_metrics_sender(
        &mut self,
        apollo_metrics_sender: Sender,
    ) -> &mut Self {
        self.apollo_metrics_sender = apollo_metrics_sender;
        self
    }

    pub(crate) fn with_reader<T: opentelemetry_sdk::metrics::reader::MetricReader>(
        &mut self,
        meter_provider_type: MeterProviderType,
        reader: T,
    ) -> &mut Self {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_reader(reader);
        self
    }

    pub(crate) fn with_view(
        &mut self,
        meter_provider_type: MeterProviderType,
        view: Box<dyn View>,
    ) -> &mut Self {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_view(view);
        self
    }

    pub(crate) fn with_resource(
        &mut self,
        meter_provider_type: MeterProviderType,
        resource: Resource,
    ) -> &mut Self {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_resource(resource);
        self
    }

    fn meter_provider(
        &mut self,
        meter_provider_type: MeterProviderType,
    ) -> &mut MeterProviderBuilder {
        self.meter_provider_builders
            .entry(meter_provider_type)
            .or_insert_with(|| match meter_provider_type {
                //Only public and default should have the resource attached as we don't send resource into fo apollo
                MeterProviderType::Public => {
                    SdkMeterProvider::builder().with_resource(self.resource.clone())
                }
                MeterProviderType::OtelDefault => {
                    SdkMeterProvider::builder().with_resource(self.resource.clone())
                }
                MeterProviderType::Apollo => SdkMeterProvider::builder(),
                MeterProviderType::ApolloRealtime => SdkMeterProvider::builder(),
            })
    }
}
