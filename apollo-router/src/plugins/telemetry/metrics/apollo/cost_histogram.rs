use hdrhistogram::Histogram;
use hdrhistogram::RecordError;
use serde::ser::SerializeSeq;
use serde::Serialize;

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
            .iter_linear(1)
            .map(|v| v.count_at_value() as i64)
            .collect()
    }
}

impl Serialize for CostHistogram {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.histogram.buckets() as usize))?;
        for value in self.histogram.iter_linear(1) {
            seq.serialize_element(&value.count_at_value())?;
        }
        seq.end()
    }
}
