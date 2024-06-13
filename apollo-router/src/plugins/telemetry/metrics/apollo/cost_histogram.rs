use hdrhistogram::Histogram;
use hdrhistogram::RecordError;
use serde::ser::SerializeSeq;
use serde::Serialize;

/// A histogram for query costs. Since costs are calculated exponentially (ie. the cost of a list
/// field is the length multiplied by the cost of its children), they are stored in exponentially
/// increasing buckets.
#[derive(Clone, Debug)]
pub(crate) struct CostHistogram {
    histogram: Histogram<u64>,
}

impl CostHistogram {
    pub(crate) fn new() -> Self {
        Self {
            // "If you're not sure, use 3" https://docs.rs/hdrhistogram/7.5.4/hdrhistogram/struct.Histogram.html#method.new_with_bounds
            histogram: Histogram::new(3).expect("sigfig is not greater than 5"),
        }
    }

    pub(crate) fn max(&self) -> u64 {
        self.histogram.max()
    }

    pub(crate) fn record(&mut self, value: f64) -> Result<(), RecordError> {
        let rounded = value.round();
        if rounded > 0.0 {
            self.histogram.record(rounded as u64)?;
        }
        Ok(())
    }

    pub(crate) fn to_vec(&self) -> Vec<i64> {
        self.histogram
            .iter_log(1, 2.0)
            .map(|v| v.count_since_last_iteration() as i64)
            .take(383)
            .collect()
    }
}

impl Serialize for CostHistogram {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let buckets = self.to_vec();
        let mut seq = serializer.serialize_seq(Some(buckets.len()))?;
        for value in buckets {
            seq.serialize_element(&value)?;
        }
        seq.end()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn exponential_bucketing() {
        let mut hist = CostHistogram::new();

        // Go up to 2^20
        for i in 0..1048576 {
            hist.record(i as f64).unwrap();
        }
        assert_eq!(hist.histogram.len(), 1048575);

        let v = hist.to_vec();
        assert_eq!(v.len(), 21);

        for (i, item) in v.iter().enumerate().take(21).skip(1) {
            let pow_of_two = i as u32;
            assert_eq!(
                *item,
                2_i64.pow(pow_of_two - 1),
                "testing count of bucket {}",
                i
            );
        }
    }
}
