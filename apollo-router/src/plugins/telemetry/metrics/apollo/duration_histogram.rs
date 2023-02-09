use std::ops::AddAssign;
use std::time::Duration;

use serde::Serialize;

#[derive(Serialize, Debug)]
pub(crate) struct DurationHistogram {
    /// `Vec` indices represents a duration bucket.
    /// `Vec` items are the sums of values in each bucket.
    pub(crate) buckets: Vec<u64>,

    /// The sum of values in all buckets
    pub(crate) total: u64,
}

impl Default for DurationHistogram {
    fn default() -> Self {
        DurationHistogram::new(None)
    }
}

impl AddAssign for DurationHistogram {
    fn add_assign(&mut self, other: DurationHistogram) {
        self.total += other.total;
        if self.buckets.len() < other.buckets.len() {
            self.buckets.resize(other.buckets.len(), 0)
        }
        self.buckets
            .iter_mut()
            .zip(other.buckets)
            .for_each(|(slot, value)| *slot += value)
    }
}

// The TS implementation of DurationHistogram does Run Length Encoding (RLE)
// to replace sequences of empty buckets with negative numbers. This
// implementation doesn't because:
// Spending too much time in the export() fn exerts back-pressure into the
// telemetry framework and leads to dropped data spans. Given that the
// histogram data is ultimately gzipped for transfer, I wasn't entirely
// sure that this extra processing was worth performing.
impl DurationHistogram {
    const DEFAULT_SIZE: usize = 74; // Taken from TS implementation
    const MAXIMUM_SIZE: usize = 383; // Taken from TS implementation
    const EXPONENT_LOG: f64 = 0.09531017980432493f64; // ln(1.1) Update when ln() is a const fn (see: https://github.com/rust-lang/rust/issues/57241)
    pub(crate) fn new(init_size: Option<usize>) -> Self {
        Self {
            buckets: vec![0; init_size.unwrap_or(DurationHistogram::DEFAULT_SIZE)],
            total: 0,
        }
    }

    fn duration_to_bucket(duration: Duration) -> usize {
        // If you use as_micros() here to avoid the divide, tests will fail
        // Because, internally, as_micros() is losing remainders
        let log_duration = f64::ln(duration.as_nanos() as f64 / 1000.0);
        let unbounded_bucket = f64::ceil(log_duration / DurationHistogram::EXPONENT_LOG);

        if unbounded_bucket.is_nan() || unbounded_bucket <= 0f64 {
            return 0;
        } else if unbounded_bucket > DurationHistogram::MAXIMUM_SIZE as f64 {
            return DurationHistogram::MAXIMUM_SIZE;
        }

        unbounded_bucket as usize
    }

    pub(crate) fn increment_duration(&mut self, duration: Option<Duration>, value: u64) {
        if let Some(duration) = duration {
            self.increment_bucket(DurationHistogram::duration_to_bucket(duration), value)
        }
    }

    fn increment_bucket(&mut self, bucket: usize, value: u64) {
        if bucket > DurationHistogram::MAXIMUM_SIZE {
            panic!("bucket is out of bounds of the bucket array");
        }
        self.total += value as u64;
        if bucket >= self.buckets.len() {
            self.buckets.resize(bucket + 1, 0);
        }
        self.buckets[bucket] += value;
    }

    /// Convert to the type expected by the Protobuf-generated struct for `repeated sint64`.
    pub(crate) fn buckets_to_i64(self) -> Vec<i64> {
        // This optimizes to nothing: https://rust.godbolt.org/z/YMh8e55de
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
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(0)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(999)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1000)),
            0
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1001)),
            1
        );
    }

    #[test]
    fn it_buckets_to_one_and_two() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1100)),
            1
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1101)),
            2
        );
    }

    #[test]
    fn it_buckets_to_threshold() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10000)),
            25
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10834)),
            25
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(10835)),
            26
        );
    }

    #[test]
    fn it_buckets_common_times() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e5 as u64)),
            49
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e6 as u64)),
            73
        );
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e9 as u64)),
            145
        );
    }

    #[test]
    fn it_limits_to_last_bucket() {
        assert_eq!(
            DurationHistogram::duration_to_bucket(Duration::from_nanos(1e64 as u64)),
            DurationHistogram::MAXIMUM_SIZE
        );
    }

    #[test]
    fn add_assign() {
        let mut h1 = DurationHistogram::new(Some(0));
        h1.increment_duration(Some(Duration::from_nanos(2000)), 1);
        h1.increment_duration(Some(Duration::from_nanos(2010)), 1);
        h1.increment_duration(Some(Duration::from_nanos(4000)), 1);
        assert_eq!(h1.buckets, [0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(h1.total, 3);

        let mut h2 = DurationHistogram::new(Some(0));
        h2.increment_duration(Some(Duration::from_nanos(1500)), 1);
        h2.increment_duration(Some(Duration::from_nanos(2020)), 1);
        assert_eq!(h2.buckets, [0, 0, 0, 0, 0, 1, 0, 0, 1]);
        assert_eq!(h2.total, 2);

        h1 += h2;
        assert_eq!(h1.buckets, [0, 0, 0, 0, 0, 1, 0, 0, 3, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(h1.total, 5);
    }
}
