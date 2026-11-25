use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::MeterProvider as _;

use crate::plugins::batching::config::BatchingMode;

/// Instruments emitted by the split and join layers.
pub(super) struct BatchMetrics {
    /// `apollo.router.operations.batching.size`
    size: Histogram<u64>,
    /// `apollo.router.operations.batching`
    requests: Counter<u64>,
}

impl BatchMetrics {
    /// Create the batching instruments.
    pub(super) fn new() -> Self {
        let meter = crate::metrics::meter_provider().meter("apollo/router");
        Self {
            size: meter
                .u64_histogram("apollo.router.operations.batching.size")
                .with_description("Number of queries contained within each query batch")
                .build(),
            requests: meter
                .u64_counter("apollo.router.operations.batching")
                .with_description("Total requests with batched operations")
                .build(),
        }
    }

    /// Record a batch of `batch_size` operations.
    ///
    /// Pass the subgraph name for batches sent to a subgraph, or `None` for batches received by
    /// the router.
    pub(super) fn record(&self, mode: &BatchingMode, batch_size: usize, subgraph: Option<&str>) {
        let mut attributes = Vec::with_capacity(2);
        attributes.push(KeyValue::new("mode", mode.to_string()));
        if let Some(subgraph) = subgraph {
            attributes.push(KeyValue::new("subgraph", subgraph.to_string()));
        }
        self.size.record(batch_size as u64, &attributes);
        self.requests.add(1, &attributes);
    }
}
