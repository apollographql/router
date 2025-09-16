use multimap::MultiMap;
use opentelemetry_sdk::Resource;
use tower::BoxError;

use crate::ListenAddr;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::resource::ConfigResource;
use crate::router_factory::Endpoint;

pub(crate) mod apollo;
pub(crate) mod local_type_stats;
pub(crate) mod otlp;
pub(crate) mod prometheus;

pub(crate) struct MetricsBuilder {
    pub(crate) public_meter_provider_builder: opentelemetry_sdk::metrics::MeterProviderBuilder,
    pub(crate) apollo_meter_provider_builder: opentelemetry_sdk::metrics::MeterProviderBuilder,
    pub(crate) apollo_realtime_meter_provider_builder:
        opentelemetry_sdk::metrics::MeterProviderBuilder,
    pub(crate) prometheus_meter_provider: Option<opentelemetry_sdk::metrics::SdkMeterProvider>,
    pub(crate) custom_endpoints: MultiMap<ListenAddr, Endpoint>,
    pub(crate) apollo_metrics_sender: Sender,
    pub(crate) resource: Resource,
}

impl MetricsBuilder {
    pub(crate) fn new(config: &Conf) -> Self {
        let resource = config.exporters.metrics.common.to_resource();

        Self {
            resource: resource.clone(),
            public_meter_provider_builder: opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                .with_resource(resource.clone()),
            apollo_meter_provider_builder: opentelemetry_sdk::metrics::SdkMeterProvider::builder(),
            apollo_realtime_meter_provider_builder:
                opentelemetry_sdk::metrics::SdkMeterProvider::builder(),
            prometheus_meter_provider: None,
            custom_endpoints: MultiMap::new(),
            apollo_metrics_sender: Sender::default(),
        }
    }
}

pub(crate) trait MetricsConfigurator {
    fn enabled(&self) -> bool;

    fn apply(
        &self,
        builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError>;
}

#[derive(Clone, Default, Debug)]
pub(crate) struct CustomAggregationSelector {
    boundaries: Vec<f64>,
    record_min_max: bool,
}

#[buildstructor::buildstructor]
impl CustomAggregationSelector {
    #[builder]
    pub(crate) fn new(
        boundaries: Vec<f64>,
        record_min_max: Option<bool>,
    ) -> CustomAggregationSelector {
        Self {
            boundaries,
            record_min_max: record_min_max.unwrap_or(true),
        }
    }
}
