use std::ops::AddAssign;

use hdrhistogram::Histogram;
use serde::ser::SerializeSeq;
use serde::Serialize;

#[derive(Clone, Debug)]
pub(crate) struct ListLengthHistogram {
    histogram: Histogram<u64>,
}

impl ListLengthHistogram {
    pub(crate) fn new() -> Self {
        Self {
            // "If you're not sure, use 3" https://docs.rs/hdrhistogram/7.5.4/hdrhistogram/struct.Histogram.html#method.new_with_bounds
            histogram: Histogram::new_with_bounds(1, 386, 3).expect("sigfig is not greater than 5"),
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
        } else if value < 116000 {
            270 + value / 1000
        } else {
            386
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

    #[test_log::test]
    fn low_magnitude_counts() {
        let mut hist = ListLengthHistogram::new();

        for i in 0..120000 {
            hist.record(i);
        }

        let v = hist.to_vec();
        for i in 0..100 {
            assert_eq!(v[i], 1, "testing contents of bucket {}", i);
        }
        for i in 100..190 {
            assert_eq!(v[i], 10, "testing contents of bucket {}", i);
        }
        for i in 190..280 {
            assert_eq!(v[i], 100, "testing contents of bucket {}", i);
        }
        for i in 280..386 {
            assert_eq!(v[i], 1000, "testing contents of bucket {}", i);
        }
        assert_eq!(v[386], 4000, "testing contents of last bucket");
    }
}
