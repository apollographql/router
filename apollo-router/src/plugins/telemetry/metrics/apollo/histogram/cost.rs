use crate::plugins::telemetry::metrics::apollo::histogram::{
    Histogram, HistogramConfig, MAXIMUM_SIZE,
};
use num_traits::AsPrimitive;
use serde::ser::SerializeMap;
use serde::Serialize;

pub(crate) type CostHistogram = Histogram<CostConfig>;
#[derive(Debug, Clone)]
pub(crate) struct CostConfig;
impl HistogramConfig for CostConfig {
    type Value = f64;
    type HistogramType = f64;

    fn bucket(value: Self::Value) -> usize {
        const EXPONENT_LOG: f64 = 0.09531017980432493f64; // ln(1.1) Update when ln() is a const fn (see: https://github.com/rust-lang/rust/issues/57241)
        let log_cost = f64::ln(value.as_());
        let unbounded_bucket = f64::ceil(log_cost / EXPONENT_LOG);

        if unbounded_bucket.is_nan() || unbounded_bucket <= 0f64 {
            return 0;
        } else if unbounded_bucket >= MAXIMUM_SIZE as f64 {
            return MAXIMUM_SIZE - 1;
        }

        unbounded_bucket as usize
    }

    fn convert(value: Self::Value) -> Self::HistogramType {
        value
    }
}

impl Serialize for Histogram<CostConfig> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("buckets", &self.clone().buckets_to_f64())?;
        map.serialize_entry("total", &self.total_f64())?;
        map.serialize_entry("max", &self.max_f64())?;
        map.end()
    }
}

#[cfg(test)]
mod test {
    use crate::plugins::telemetry::metrics::apollo::histogram::CostHistogram;
    use insta::assert_yaml_snapshot;

    #[test]
    fn cost_bucketing() {
        let mut hist = CostHistogram::new(None);

        // Go up to 2^20
        for i in 0..1048576 {
            hist.record(Some(i as f64), 1.0);
        }
        assert_eq!(hist.total_u64(), 1048576);
        assert_yaml_snapshot!(hist.buckets_to_i64());
    }
}
