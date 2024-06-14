use std::ops::AddAssign;

use hdrhistogram::Histogram;
use serde::ser::SerializeSeq;
use serde::Serialize;

use super::studio::MAX_HISTOGRAM_BUCKETS;

/// A histogram for recording lengths of list fields in GraphQL responses. This implementation is clamped to a maximum
/// of 386 buckets, a restriction imposed by Studio. The buckets roughly follow order of magnitude of the recorded value,
/// so lower values are recorded with higher levels of granularity. Each value under 100 has its own integer bucket, values
/// between 100 and 1000 have buckets of width 10, and so on up to 116000. Anything over 116000 ends up in the final bucket.
#[derive(Clone, Debug)]
pub(crate) struct ListLengthHistogram {
    histogram: Histogram<u64>,
}

impl ListLengthHistogram {
    pub(crate) fn new() -> Self {
        Self {
            // "If you're not sure, use 3" https://docs.rs/hdrhistogram/7.5.4/hdrhistogram/struct.Histogram.html#method.new_with_bounds
            histogram: Histogram::new_with_bounds(1, MAX_HISTOGRAM_BUCKETS as u64, 3)
                .expect("sigfig is not greater than 5"),
        }
    }

    pub(crate) fn record(&mut self, value: usize) {
        let value = value as u64;
        let bucket = if value < 100 {
            value
        } else if value < 1000 {
            90 + value / 10
        } else if value < 10000 {
            180 + value / 100
        } else if value < 114000 {
            270 + value / 1000
        } else {
            MAX_HISTOGRAM_BUCKETS as u64 - 1
        };
        self.histogram.saturating_record(bucket);
    }

    pub(crate) fn to_vec(&self) -> Vec<i64> {
        self.histogram
            .iter_linear(1)
            .map(|x| x.count_at_value() as i64)
            .collect()
    }
}

impl Default for ListLengthHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Serialize for ListLengthHistogram {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let buckets = self.to_vec();
        let mut seq = serializer.serialize_seq(Some(buckets.len()))?;
        for value in buckets {
            seq.serialize_element(&value)?;
        }
        seq.end()
    }
}

impl AddAssign<ListLengthHistogram> for ListLengthHistogram {
    fn add_assign(&mut self, rhs: ListLengthHistogram) {
        self.histogram += rhs.histogram;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn magnitude_based_bucketing() {
        let mut hist = ListLengthHistogram::new();

        for i in 0..120000 {
            hist.record(i);
        }

        let v = hist.to_vec();
        assert_eq!(v.len(), 384);

        for (i, item) in v.iter().enumerate().take(100) {
            assert_eq!(*item, 1, "testing contents of bucket {}", i);
        }
        for (i, item) in v.iter().enumerate().take(190).skip(100) {
            assert_eq!(*item, 10, "testing contents of bucket {}", i);
        }
        for (i, item) in v.iter().enumerate().take(280).skip(190) {
            assert_eq!(*item, 100, "testing contents of bucket {}", i);
        }
        for (i, item) in v.iter().enumerate().take(383).skip(280) {
            assert_eq!(*item, 1000, "testing contents of bucket {}", i);
        }
        assert_eq!(v[383], 7000, "testing contents of last bucket");
    }
}
