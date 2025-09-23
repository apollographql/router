use ::prometheus::Registry;
use opentelemetry_sdk::metrics::Aggregation;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_sdk::metrics::reader::AggregationSelector;
use tower::BoxError;

use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::resource::ConfigResource;

pub(crate) mod apollo;
pub(crate) mod local_type_stats;
pub(crate) mod otlp;
pub(crate) mod prometheus;

pub(crate) struct MetricsBuilder {
    pub(crate) public_meter_provider_builder: opentelemetry_sdk::metrics::MeterProviderBuilder,
    pub(crate) apollo_meter_provider_builder: opentelemetry_sdk::metrics::MeterProviderBuilder,
    pub(crate) apollo_realtime_meter_provider_builder:
        opentelemetry_sdk::metrics::MeterProviderBuilder,
    pub(crate) apollo_metrics_sender: Sender,
    pub(crate) prometheus_registry: Option<Registry>,
}

impl MetricsBuilder {
    pub(crate) fn new(config: &Conf) -> Self {
        let resource = config.exporters.metrics.common.to_resource();

        Self {
            public_meter_provider_builder: opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                .with_resource(resource.clone()),
            apollo_meter_provider_builder: opentelemetry_sdk::metrics::SdkMeterProvider::builder(),
            apollo_realtime_meter_provider_builder:
                opentelemetry_sdk::metrics::SdkMeterProvider::builder(),
            apollo_metrics_sender: Sender::default(),
            prometheus_registry: None,
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

impl AggregationSelector for CustomAggregationSelector {
    fn aggregation(&self, kind: InstrumentKind) -> Aggregation {
        match kind {
            InstrumentKind::Counter
            | InstrumentKind::UpDownCounter
            | InstrumentKind::ObservableCounter
            | InstrumentKind::ObservableUpDownCounter => Aggregation::Sum,
            InstrumentKind::Gauge | InstrumentKind::ObservableGauge => Aggregation::LastValue,
            InstrumentKind::Histogram => Aggregation::ExplicitBucketHistogram {
                boundaries: self.boundaries.clone(),
                record_min_max: self.record_min_max,
            },
        }
    }
}
