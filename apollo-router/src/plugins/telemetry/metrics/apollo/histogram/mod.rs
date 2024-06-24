mod cost;
mod duration;
mod list_length;

use std::fmt::Debug;
use std::ops::AddAssign;

pub(crate) use cost::CostHistogram;
pub(crate) use duration::DurationHistogram;
pub(crate) use list_length::ListLengthHistogram;
use num_traits::AsPrimitive;
use serde::Serialize;
const MAXIMUM_SIZE: usize = 384;

pub(crate) trait HistogramConfig: Debug + Clone {
    type Value: Default + PartialOrd + Copy;
    type HistogramType: Debug + Copy + Default + AddAssign + PartialEq + Serialize + PartialOrd;
    fn bucket(value: Self::Value) -> usize;
    fn convert(value: Self::Value) -> Self::HistogramType;
}

/// Records a set of durations, quantized into a small number of buckets.
/// `T` values represent a number of events that fall into a given bucket.
/// It can be either `u64` for exact counts (where the `value` parameter
/// of `increment_duration` is typically `1`), or `f64` for estimations
/// that compensate for a sampling rate.
#[derive(Clone, Debug)]
pub(crate) struct Histogram<Config: HistogramConfig> {
    /// `Vec` indices represents a duration bucket.
    /// `Vec` items are the sums of values in each bucket.
    buckets: Vec<Config::HistogramType>,
    total: Config::HistogramType,
    max: Config::HistogramType,
}

impl<Config: HistogramConfig> Default for Histogram<Config> {
    fn default() -> Self {
        Histogram::new(None)
    }
}

impl<Config: HistogramConfig> AddAssign for Histogram<Config> {
    fn add_assign(&mut self, other: Histogram<Config>) {
        if self.buckets.len() < other.buckets.len() {
            self.buckets
                .resize(other.buckets.len(), Config::HistogramType::default())
        }
        self.buckets
            .iter_mut()
            .zip(other.buckets)
            .for_each(|(slot, value)| *slot += value);
        self.max = if self.max > other.max {
            self.max
        } else {
            other.max
        };
        self.total += other.total;
    }
}

// The TS implementation of DurationHistogram does Run Length Encoding (RLE)
// to replace sequences of empty buckets with negative numbers. This
// implementation doesn't because:
// Spending too much time in the export() fn exerts back-pressure into the
// telemetry framework and leads to dropped data spans. Given that the
// histogram data is ultimately gzipped for transfer, I wasn't entirely
// sure that this extra processing was worth performing.
impl<Config: HistogramConfig> Histogram<Config> {
    pub(crate) fn new(init_size: Option<usize>) -> Self {
        Self {
            buckets: vec![Config::HistogramType::default(); init_size.unwrap_or(MAXIMUM_SIZE)],
            total: Config::HistogramType::default(),
            max: Config::HistogramType::default(),
        }
    }

    pub(crate) fn record(&mut self, value: Option<Config::Value>, amount: Config::HistogramType) {
        if let Some(value) = value {
            let bucket = Config::bucket(value);
            if bucket > MAXIMUM_SIZE {
                panic!("bucket is out of bounds of the bucket array");
            }
            if bucket >= self.buckets.len() {
                self.buckets
                    .resize(bucket + 1, Config::HistogramType::default());
            }
            self.buckets[bucket] += amount;
            let value = Config::convert(value);
            if value > self.max {
                self.max = value;
            }
            self.total += amount;
        }
    }

    pub(crate) fn trim_trailing_zeroes(&mut self) {
        let zero = Config::HistogramType::default();
        let last_non_zero = self.buckets.iter().rposition(|b| *b != zero).unwrap_or(0);
        self.buckets.truncate(last_non_zero + 1);
    }
}

impl<Config: HistogramConfig> Histogram<Config>
where
    Config::HistogramType: AsPrimitive<f64>,
{
    /// Convert to the type expected by the Protobuf-generated struct for `repeated sint64`.
    ///
    /// When estimating, rounding to integer values is only done after aggregating in memory
    /// a number data points by summing floating-point values.
    #[inline]
    pub(crate) fn buckets_to_f64(mut self) -> Vec<f64> {
        self.trim_trailing_zeroes();
        self.buckets.into_iter().map(|x| x.as_()).collect()
    }

    #[inline]
    pub(crate) fn total_f64(&self) -> f64 {
        self.total.as_()
    }

    #[inline]
    pub(crate) fn max_f64(&self) -> f64 {
        self.max.as_()
    }
}

impl<Config: HistogramConfig> Histogram<Config>
where
    Config::HistogramType: AsPrimitive<i64>,
{
    /// Convert to the type expected by the Protobuf-generated struct for `repeated sint64`.
    ///
    /// When estimating, rounding to integer values is only done after aggregating in memory
    /// a number data points by summing floating-point values.
    #[inline]
    pub(crate) fn buckets_to_i64(mut self) -> Vec<i64> {
        self.trim_trailing_zeroes();
        self.buckets.into_iter().map(|x| x.as_()).collect()
    }

    #[inline]
    pub(crate) fn total_i64(&self) -> i64 {
        self.total.as_()
    }

    #[inline]
    pub(crate) fn max_i64(&self) -> i64 {
        self.max.as_()
    }
}

impl<Config: HistogramConfig> Histogram<Config>
where
    Config::HistogramType: AsPrimitive<u64>,
{
    #[inline]
    pub(crate) fn total_u64(&self) -> u64 {
        self.total.as_()
    }

    #[inline]
    pub(crate) fn max_u64(&self) -> u64 {
        self.max.as_()
    }
}
