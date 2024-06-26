use serde::ser::SerializeMap;
use serde::Serialize;

use crate::plugins::telemetry::metrics::apollo::histogram::Histogram;
use crate::plugins::telemetry::metrics::apollo::histogram::HistogramConfig;
use crate::plugins::telemetry::metrics::apollo::histogram::MAXIMUM_SIZE;

pub(crate) type ListLengthHistogram = Histogram<ListLengthConfig>;
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListLengthConfig;
impl HistogramConfig for ListLengthConfig {
    type Value = u64;
    type HistogramType = u64;

    fn bucket(value: Self::Value) -> usize {
        (if value < 100 {
            value
        } else if value < 1000 {
            90 + value / 10
        } else if value < 10000 {
            180 + value / 100
        } else if value < 114000 {
            270 + value / 1000
        } else {
            (MAXIMUM_SIZE - 1) as u64
        }) as usize
    }

    fn convert(value: Self::Value) -> Self::HistogramType {
        value
    }
}

impl Serialize for Histogram<ListLengthConfig> {
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
    use crate::plugins::telemetry::metrics::apollo::histogram::ListLengthHistogram;
    use crate::plugins::telemetry::metrics::apollo::histogram::MAXIMUM_SIZE;

    #[test]
    fn list_length_bucketing() {
        let mut hist = ListLengthHistogram::new(None);

        for i in 0..120000 {
            hist.record(Some(i), 1);
        }

        let v = hist.buckets_to_i64();
        assert_eq!(v.len(), MAXIMUM_SIZE);

        for (i, item) in v.iter().enumerate().take(100) {
            assert_eq!(*item, 1, "testing contents of bucket {}", i);
        }
        for (i, item) in v.iter().enumerate().take(190).skip(100) {
            assert_eq!(*item, 10, "testing contents of bucket {}", i);
        }
        for (i, item) in v.iter().enumerate().take(280).skip(190) {
            assert_eq!(*item, 100, "testing contents of bucket {}", i);
        }
        for (i, item) in v.iter().enumerate().take(382).skip(280) {
            assert_eq!(*item, 1000, "testing contents of bucket {}", i);
        }
        assert_eq!(v[MAXIMUM_SIZE - 1], 7000, "testing contents of last bucket");
    }

    #[test]
    fn it_generates_populated_histogram() {
        let mut histogram = ListLengthHistogram::new(None);
        histogram.record(Some(1), 1);
        assert_eq!(histogram.to_array(), vec![0, 1]);
        histogram.record(Some(2), 1);
        assert_eq!(histogram.to_array(), vec![0, 1, 1]);
        histogram.record(Some(2), 3);
        assert_eq!(histogram.to_array(), vec![0, 1, 4]);
    }
}
