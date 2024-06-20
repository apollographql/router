use std::ops::AddAssign;
use std::time::Duration;

use serde::Serialize;

/// Records a set of durations, quantized into a small number of buckets.
/// `T` values represent a number of events that fall into a given bucket.
/// It can be either `u64` for exact counts (where the `value` parameter
/// of `increment_duration` is typically `1`), or `f64` for estimations
/// that compensate for a sampling rate.
#[derive(Clone, Serialize, Debug)]
pub(crate) struct DurationHistogram<T = u64> {
    /// `Vec` indices represents a duration bucket.
    /// `Vec` items are the sums of values in each bucket.
    pub(crate) buckets: Vec<T>,
}

impl<T: Copy + Default + AddAssign> Default for DurationHistogram<T> {
    fn default() -> Self {
        DurationHistogram::new(None)
    }
}

impl<T: Copy + Default + AddAssign> AddAssign for DurationHistogram<T> {
    fn add_assign(&mut self, other: DurationHistogram<T>) {
        if self.buckets.len() < other.buckets.len() {
            self.buckets.resize(other.buckets.len(), T::default())
        }
        self.buckets
            .iter_mut()
            .zip(other.buckets)
            .for_each(|(slot, value)| *slot += value)
    }
}

const DEFAULT_SIZE: usize = 74; // Taken from TS implementation
const MAXIMUM_SIZE: usize = 383; // Taken from TS implementation
const EXPONENT_LOG: f64 = 0.09531017980432493f64; // ln(1.1) Update when ln() is a const fn (see: https://github.com/rust-lang/rust/issues/57241)

fn duration_to_bucket(duration: Duration) -> usize {
    // If you use as_micros() here to avoid the divide, tests will fail
    // Because, internally, as_micros() is losing remainders
    let log_duration = f64::ln(duration.as_nanos() as f64 / 1000.0);
    let unbounded_bucket = f64::ceil(log_duration / EXPONENT_LOG);

    if unbounded_bucket.is_nan() || unbounded_bucket <= 0f64 {
        return 0;
    } else if unbounded_bucket > MAXIMUM_SIZE as f64 {
        return MAXIMUM_SIZE;
    }

    unbounded_bucket as usize
}

// The TS implementation of DurationHistogram does Run Length Encoding (RLE)
// to replace sequences of empty buckets with negative numbers. This
// implementation doesn't because:
// Spending too much time in the export() fn exerts back-pressure into the
// telemetry framework and leads to dropped data spans. Given that the
// histogram data is ultimately gzipped for transfer, I wasn't entirely
// sure that this extra processing was worth performing.
impl<T: Copy + Default + AddAssign> DurationHistogram<T> {
    pub(crate) fn new(init_size: Option<usize>) -> Self {
        Self {
            buckets: vec![T::default(); init_size.unwrap_or(DEFAULT_SIZE)],
        }
    }

    pub(crate) fn increment_duration(&mut self, duration: Option<Duration>, value: T) {
        if let Some(duration) = duration {
            self.increment_bucket(duration_to_bucket(duration), value)
        }
    }

    fn increment_bucket(&mut self, bucket: usize, value: T) {
        if bucket > MAXIMUM_SIZE {
            panic!("bucket is out of bounds of the bucket array");
        }
        if bucket >= self.buckets.len() {
            self.buckets.resize(bucket + 1, T::default());
        }
        self.buckets[bucket] += value;
    }

    pub(crate) fn trim_trailing_zeroes(&mut self)
    where
        T: PartialEq,
    {
        let zero = T::default();
        let last_non_zero = self.buckets.iter().rposition(|b| *b != zero).unwrap_or(0);
        self.buckets.truncate(last_non_zero + 1);
    }

    #[inline]
    pub(crate) fn total(&self) -> T {
        self.buckets.iter().fold(T::default(), |mut sum, &x| {
            sum += x;
            sum
        })
    }
}

impl DurationHistogram<u64> {
    /// Convert to the type expected by the Protobuf-generated struct for `repeated sint64`.
    pub(crate) fn buckets_to_i64(mut self) -> Vec<i64> {
        self.trim_trailing_zeroes();
        // This optimizes to nothing: https://rust.godbolt.org/z/YMh8e55de
        self.buckets.into_iter().map(|x| x as i64).collect()
    }
}

impl DurationHistogram<f64> {
    /// Convert to the type expected by the Protobuf-generated struct for `repeated sint64`.
    ///
    /// When estimating, rounding to integer values is only done after aggregating in memory
    /// a number data points by summing floating-point values.
    pub(crate) fn buckets_to_i64(mut self) -> Vec<i64> {
        self.trim_trailing_zeroes();
        self.buckets.into_iter().map(|x| x as i64).collect()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    // DurationHistogram Tests
    impl DurationHistogram {
        fn to_array(&self) -> Vec<i64> {
            let mut result = vec![];
            let mut buffered_zeroes = 0;

            for value in &self.buckets {
                if *value == 0 {
                    buffered_zeroes += 1;
                } else {
                    if buffered_zeroes == 1 {
                        result.push(0);
                    } else if buffered_zeroes != 0 {
                        result.push(0 - buffered_zeroes);
                    }
                    result.push(*value as i64);
                    buffered_zeroes = 0;
                }
            }
            result
        }
    }

    #[test]
    fn it_generates_empty_histogram() {
        let histogram = DurationHistogram::new(None);
        let expected: Vec<i64> = vec![];
        assert_eq!(histogram.to_array(), expected);
    }

    #[test]
    fn it_generates_populated_histogram() {
        let mut histogram = DurationHistogram::new(None);
        histogram.increment_bucket(100, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1]);
        histogram.increment_bucket(102, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1, 0, 1]);
        histogram.increment_bucket(382, 1);
        assert_eq!(histogram.to_array(), vec![-100, 1, 0, 1, -279, 1]);
    }

    #[test]
    fn it_buckets_to_zero_and_one() {
        assert_eq!(duration_to_bucket(Duration::from_nanos(0)), 0);
        assert_eq!(duration_to_bucket(Duration::from_nanos(1)), 0);
        assert_eq!(duration_to_bucket(Duration::from_nanos(999)), 0);
        assert_eq!(duration_to_bucket(Duration::from_nanos(1000)), 0);
        assert_eq!(duration_to_bucket(Duration::from_nanos(1001)), 1);
    }

    #[test]
    fn it_buckets_to_one_and_two() {
        assert_eq!(duration_to_bucket(Duration::from_nanos(1100)), 1);
        assert_eq!(duration_to_bucket(Duration::from_nanos(1101)), 2);
    }

    #[test]
    fn it_buckets_to_threshold() {
        assert_eq!(duration_to_bucket(Duration::from_nanos(10000)), 25);
        assert_eq!(duration_to_bucket(Duration::from_nanos(10834)), 25);
        assert_eq!(duration_to_bucket(Duration::from_nanos(10835)), 26);
    }

    #[test]
    fn it_buckets_common_times() {
        assert_eq!(duration_to_bucket(Duration::from_nanos(1e5 as u64)), 49);
        assert_eq!(duration_to_bucket(Duration::from_nanos(1e6 as u64)), 73);
        assert_eq!(duration_to_bucket(Duration::from_nanos(1e9 as u64)), 145);
    }

    #[test]
    fn it_limits_to_last_bucket() {
        assert_eq!(
            duration_to_bucket(Duration::from_nanos(1e64 as u64)),
            MAXIMUM_SIZE
        );
    }

    #[test]
    fn add_assign() {
        let mut h1 = DurationHistogram::new(Some(0));
        h1.increment_duration(Some(Duration::from_nanos(2000)), 1);
        h1.increment_duration(Some(Duration::from_nanos(2010)), 1);
        h1.increment_duration(Some(Duration::from_nanos(4000)), 1);
        assert_eq!(h1.buckets, [0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(h1.total(), 3);

        let mut h2 = DurationHistogram::new(Some(0));
        h2.increment_duration(Some(Duration::from_nanos(1500)), 1);
        h2.increment_duration(Some(Duration::from_nanos(2020)), 1);
        assert_eq!(h2.buckets, [0, 0, 0, 0, 0, 1, 0, 0, 1]);
        assert_eq!(h2.total(), 2);

        h1 += h2;
        assert_eq!(h1.buckets, [0, 0, 0, 0, 0, 1, 0, 0, 3, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(h1.total(), 5);
    }

    #[test]
    fn trim() {
        let mut h = DurationHistogram::new(None);
        assert_eq!(h.buckets.len(), DEFAULT_SIZE);
        h.increment_duration(Some(Duration::from_nanos(1500)), 1);
        h.increment_duration(Some(Duration::from_nanos(2020)), 1);
        h.increment_duration(Some(Duration::from_nanos(1500)), 1);
        assert_eq!(h.buckets.len(), DEFAULT_SIZE);
        h.trim_trailing_zeroes();
        assert_eq!(h.buckets, [0, 0, 0, 0, 0, 2, 0, 0, 1]);
        assert_eq!(h.total(), 3);
    }
}
