use std::collections::HashMap;
use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;

use super::RouterOverheadAttributes;
use crate::metrics;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::HistogramInstrument;
use crate::plugins::telemetry::config_new::instruments::IncrementKind;
use crate::plugins::telemetry::config_new::instruments::METER_NAME;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::router::selectors::RouterSelector;

pub(crate) const ROUTER_OVERHEAD_METRIC: &str = "apollo.router.overhead";

/// Create the static histogram instrument for router overhead if enabled
pub(crate) fn create_static_instrument(enabled: bool) -> Option<(String, StaticInstrument)> {
    if !enabled {
        return None;
    }

    let meter = metrics::meter_provider().meter(METER_NAME);
    Some((
        ROUTER_OVERHEAD_METRIC.to_string(),
        StaticInstrument::Histogram(
            meter
                .f64_histogram(ROUTER_OVERHEAD_METRIC)
                .with_unit("s")
                .with_description(
                    "Router processing overhead (time not spent waiting for subgraphs or connectors to respond).",
                )
                .build(),
        ),
    ))
}

/// The router overhead histogram `config` configures, built once for a pipeline over its
/// `static_instruments`: its value the overhead its selector reads, where it is enabled.
pub(crate) fn router_overhead_instrument(
    config: &DefaultedStandardInstrument<Extendable<RouterOverheadAttributes, RouterSelector>>,
    static_instruments: &HashMap<String, StaticInstrument>,
) -> Option<HistogramInstrument<RouterOverheadAttributes, RouterSelector>> {
    if !config.is_enabled() {
        return None;
    }

    let selectors = match config {
        DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => None,
        DefaultedStandardInstrument::Extendable { attributes } => Some(attributes.clone()),
    };

    Some(HistogramInstrument::new(
        IncrementKind::Custom,
        static_instruments
            .get(ROUTER_OVERHEAD_METRIC)
            .expect("cannot get static instrument for router overhead; this should not happen")
            .as_histogram()
            .cloned()
            .expect(
                "cannot convert instrument to histogram for router overhead; this should not happen",
            ),
        Some(Arc::new(RouterSelector::RouterOverhead {
            router_overhead: true,
        })),
        selectors,
    ))
}
