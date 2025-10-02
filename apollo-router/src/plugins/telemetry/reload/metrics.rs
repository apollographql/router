use ahash::HashMap;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::MeterProviderBuilder;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::metrics::View;
use prometheus::Registry;
use tower::BoxError;

use crate::_private::telemetry::ConfigResource;
use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::MetricsCommon;

pub(crate) trait MetricsConfigurator {
    fn config(conf: &Conf) -> &Self;
    fn is_enabled(&self) -> bool;
    fn configure<'a>(&self, builder: &mut MetricsBuilder<'a>) -> Result<(), BoxError>;
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
    pub(crate) fn build(
        self,
    ) -> (
        Option<Registry>,
        HashMap<MeterProviderType, FilterMeterProvider>,
        Sender,
    ) {
        (
            self.prometheus_registry,
            self.meter_provider_builders
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        match k {
                            MeterProviderType::Public => FilterMeterProvider::public(v.build()),
                            MeterProviderType::OtelDefault => {
                                FilterMeterProvider::public(v.build())
                            }
                            MeterProviderType::Apollo => FilterMeterProvider::apollo(v.build()),
                            MeterProviderType::ApolloRealtime => {
                                FilterMeterProvider::apollo_realtime(v.build())
                            }
                        },
                    )
                })
                .collect(),
            self.apollo_metrics_sender,
        )
    }
    pub(crate) fn configure<T: MetricsConfigurator>(&mut self, config: &T) -> Result<(), BoxError> {
        if config.is_enabled() {
            return config.configure(self);
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

    pub(crate) fn configure_views(
        &mut self,
        meter_provider_type: MeterProviderType,
    ) -> Result<(), BoxError> {
        for metric_view in self.metrics_common().views.clone() {
            self.with_view(meter_provider_type, metric_view.try_into()?);
        }
        Ok(())
    }
}
