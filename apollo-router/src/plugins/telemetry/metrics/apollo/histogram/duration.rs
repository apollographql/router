use std::fmt::Debug;
use std::ops::AddAssign;
use std::time::Duration;

use num_traits::AsPrimitive;
use num_traits::FromPrimitive;
use serde::ser::SerializeMap;
use serde::Serialize;

use crate::plugins::telemetry::metrics::apollo::histogram::Histogram;
use crate::plugins::telemetry::metrics::apollo::histogram::HistogramConfig;
use crate::plugins::telemetry::metrics::apollo::histogram::MAXIMUM_SIZE;

pub(crate) type DurationHistogram<Type = u64> = Histogram<DurationConfig<Type>>;
#[derive(Debug, Clone)]
pub(crate) struct DurationConfig<Type>
where
    Type: Default + PartialOrd + Copy + Debug + Serialize + AddAssign,
{
    _phantom: std::marker::PhantomData<Type>,
}

impl<Type> HistogramConfig for DurationConfig<Type>
where
    Type: Default
        + PartialOrd
        + Copy
        + Debug
        + Serialize
        + AddAssign
        + FromPrimitive
        + AsPrimitive<f64>,
{
    type Value = Duration;
    type HistogramType = Type;

    fn bucket(value: Self::Value) -> usize {
        const EXPONENT_LOG: f64 = 0.09531017980432493f64; // ln(1.1) Update when ln() is a const fn (see: https://github.com/rust-lang/rust/issues/57241)
                                                          // If you use as_micros() here to avoid the divide, tests will fail
                                                          // Because, internally, as_micros() is losing remainders
        let float_value = value.as_nanos() as f64 / 1000.0;
        let log_duration = f64::ln(float_value);
        let unbounded_bucket = f64::ceil(log_duration / EXPONENT_LOG);
        if unbounded_bucket.is_nan() || unbounded_bucket <= 0f64 {
            return 0;
        } else if unbounded_bucket >= MAXIMUM_SIZE as f64 {
            return MAXIMUM_SIZE - 1;
        }

        unbounded_bucket as usize
    }

    fn convert(value: Self::Value) -> Self::HistogramType {
        // If you use as_micros() here to avoid the divide, tests will fail
        // Because, internally, as_micros() is losing remainders
        Self::HistogramType::from_f64(value.as_nanos() as f64 / 1000.0).unwrap_or_default()
    }
}

impl Serialize for Histogram<DurationConfig<u64>> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("buckets", &self.clone().buckets_to_i64())?;
        map.serialize_entry("total", &self.total_i64())?;
        map.serialize_entry("max", &self.max_i64())?;
        map.end()
    }
}

impl Serialize for Histogram<DurationConfig<f64>> {
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
    use num_traits::AsPrimitive;

    use super::*;

    // DurationHistogram Tests
    impl<Config: HistogramConfig> Histogram<Config>
    where
        Config::HistogramType: AsPrimitive<i64>,
    {
        pub(crate) fn to_array(&self) -> Vec<i64> {
            let mut result = vec![];
            let mut buffered_zeroes = 0;

            for value in &self.buckets {
                if *value == Config::HistogramType::default() {
                    buffered_zeroes += 1;
                } else {
                    if buffered_zeroes == 1 {
                        result.push(0);
                    } else if buffered_zeroes != 0 {
                        result.push(0 - buffered_zeroes);
                    }
                    result.push(value.as_());
                    buffered_zeroes = 0;
                }
            }
            result
        }
    }

    #[test]
    fn it_generates_empty_histogram() {
        let histogram = DurationHistogram::<u64>::new(None);
        let expected: Vec<i64> = vec![];
        assert_eq!(histogram.to_array(), expected);
    }

    #[test]
    fn it_buckets_to_zero_and_one() {
        assert_eq!(DurationConfig::<u64>::bucket(Duration::from_nanos(0)), 0);
        assert_eq!(DurationConfig::<u64>::bucket(Duration::from_nanos(1)), 0);
        assert_eq!(DurationConfig::<u64>::bucket(Duration::from_nanos(999)), 0);
        assert_eq!(DurationConfig::<u64>::bucket(Duration::from_nanos(1000)), 0);
        assert_eq!(DurationConfig::<u64>::bucket(Duration::from_nanos(1001)), 1);
    }

    #[test]
    fn it_buckets_to_one_and_two() {
        assert_eq!(DurationConfig::<u64>::bucket(Duration::from_nanos(1100)), 1);
        assert_eq!(DurationConfig::<u64>::bucket(Duration::from_nanos(1101)), 2);
    }

    #[test]
    fn it_buckets_to_threshold() {
        assert_eq!(
            DurationConfig::<u64>::bucket(Duration::from_nanos(10000)),
            25
        );
        assert_eq!(
            DurationConfig::<u64>::bucket(Duration::from_nanos(10834)),
            25
        );
        assert_eq!(
            DurationConfig::<u64>::bucket(Duration::from_nanos(10835)),
            26
        );
    }

    #[test]
    fn it_buckets_common_times() {
        assert_eq!(
            DurationConfig::<u64>::bucket(Duration::from_nanos(1e5 as u64)),
            49
        );
        assert_eq!(
            DurationConfig::<u64>::bucket(Duration::from_nanos(1e6 as u64)),
            73
        );
        assert_eq!(
            DurationConfig::<u64>::bucket(Duration::from_nanos(1e9 as u64)),
            145
        );
    }

    #[test]
    fn it_limits_to_last_bucket() {
        assert_eq!(
            DurationConfig::<u64>::bucket(Duration::from_nanos(1e64 as u64)),
            MAXIMUM_SIZE - 1
        );
    }

    #[test]
    fn add_assign() {
        let mut h1 = DurationHistogram::new(Some(0));
        h1.record(Some(Duration::from_nanos(2000)), 1.0);
        h1.record(Some(Duration::from_nanos(2010)), 1.0);
        h1.record(Some(Duration::from_nanos(4000)), 1.0);
        assert_eq!(h1.total_i64(), 3);
        assert_eq!(
            h1.clone().buckets_to_i64(),
            [0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1]
        );

        let mut h2 = DurationHistogram::new(Some(0));
        h2.record(Some(Duration::from_nanos(1500)), 1.0);
        h2.record(Some(Duration::from_nanos(2020)), 1.0);
        assert_eq!(h2.total_i64(), 2);
        assert_eq!(h2.clone().buckets_to_i64(), [0, 0, 0, 0, 0, 1, 0, 0, 1]);

        h1 += h2;
        assert_eq!(h1.total_i64(), 5);
        assert_eq!(
            h1.buckets_to_i64(),
            [0, 0, 0, 0, 0, 1, 0, 0, 3, 0, 0, 0, 0, 0, 0, 1]
        );
    }

    #[test]
    fn trim() {
        let mut h = DurationHistogram::new(None);
        assert_eq!(h.buckets.len(), MAXIMUM_SIZE);
        h.record(Some(Duration::from_nanos(1500)), 1.0);
        h.record(Some(Duration::from_nanos(2020)), 1.0);
        h.record(Some(Duration::from_nanos(1500)), 1.0);
        assert_eq!(h.buckets.len(), MAXIMUM_SIZE);
        h.trim_trailing_zeroes();
        assert_eq!(h.total_i64(), 3);
        assert_eq!(h.buckets_to_i64(), [0, 0, 0, 0, 0, 2, 0, 0, 1]);
    }
}
