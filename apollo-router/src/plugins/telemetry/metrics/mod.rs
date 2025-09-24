use opentelemetry_sdk::metrics::Aggregation;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_sdk::metrics::reader::AggregationSelector;
use tower::BoxError;

use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::reload::builder::MetricsBuilder;

pub(crate) mod apollo;
pub(crate) mod local_type_stats;
pub(crate) mod otlp;
pub(crate) mod prometheus;

pub(crate) trait MetricsConfigurator {
    fn config(conf: &Conf) -> &Self;
    fn enabled(&self) -> bool;
    fn apply<'a>(&self, builder: &mut MetricsBuilder<'a>) -> Result<(), BoxError>;
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
