use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;
use parking_lot::Mutex;

use super::RouterOverheadAttributes;
use crate::metrics;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Increment;
use crate::plugins::telemetry::config_new::instruments::METER_NAME;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::router::selectors::RouterSelector;
use crate::services::router;

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
                    "Router processing overhead (time not spent waiting for subgraphs).",
                )
                .init(),
        ),
    ))
}

/// Initialize the router overhead custom histogram from config
pub(crate) fn initialize_custom_histogram(
    config: &DefaultedStandardInstrument<Extendable<RouterOverheadAttributes, RouterSelector>>,
    static_instruments: &HashMap<String, StaticInstrument>,
) -> Option<
    CustomHistogram<
        router::Request,
        router::Response,
        (),
        RouterOverheadAttributes,
        RouterSelector,
    >,
> {
    if !config.is_enabled() {
        return None;
    }

    let mut nb_attributes = 0;
    let selectors = match config {
        DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => None,
        DefaultedStandardInstrument::Extendable { attributes } => {
            nb_attributes = attributes.custom.len();
            Some(attributes.clone())
        }
    };

    Some(CustomHistogram {
        inner: Mutex::new(CustomHistogramInner {
            increment: Increment::Custom(None),
            condition: Condition::True,
            histogram: Some(
                static_instruments
                    .get(ROUTER_OVERHEAD_METRIC)
                    .expect(
                        "cannot get static instrument for router overhead; this should not happen",
                    )
                    .as_histogram()
                    .cloned()
                    .expect("cannot convert instrument to histogram for router overhead; this should not happen"),
            ),
            attributes: Vec::with_capacity(nb_attributes),
            selector: Some(Arc::new(
                RouterSelector::RouterOverhead {
                    router_overhead: true,
                },
            )),
            selectors,
            updated: false,
            _phantom: PhantomData,
        }),
    })
}
