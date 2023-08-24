use std::collections::HashSet;
use std::time::Instant;

use tracing_core::field::Visit;
use tracing_core::span;
use tracing_core::Field;
use tracing_core::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::EXECUTION_SPAN_NAME;
use crate::plugins::telemetry::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::SUPERGRAPH_SPAN_NAME;
use crate::services::QUERY_PLANNING_SPAN_NAME;

const SUBGRAPH_ATTRIBUTE_NAME: &str = "apollo.subgraph.name";

#[derive(Debug)]
pub(crate) struct SpanMetricsLayer {
    span_names: HashSet<&'static str>,
}

impl Default for SpanMetricsLayer {
    fn default() -> Self {
        Self {
            span_names: [
                REQUEST_SPAN_NAME,
                SUPERGRAPH_SPAN_NAME,
                SUBGRAPH_SPAN_NAME,
                QUERY_PLANNING_SPAN_NAME,
                EXECUTION_SPAN_NAME,
            ]
            .into(),
        }
    }
}

impl<S> Layer<S> for SpanMetricsLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();

        let name = attrs.metadata().name();
        if self.span_names.contains(name) && extensions.get_mut::<Timings>().is_none() {
            let mut timings = Timings::new();
            if name == SUBGRAPH_SPAN_NAME {
                attrs.values().record(&mut ValueVisitor {
                    timings: &mut timings,
                });
            }
            extensions.insert(Timings::new());
        }
    }

    fn on_record(&self, _span: &span::Id, _values: &span::Record<'_>, _ctx: Context<'_, S>) {}

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();

        if let Some(timings) = extensions.get_mut::<Timings>() {
            let duration = timings.start.elapsed().as_secs_f64();

            // Convert it in seconds
            let idle: f64 = timings.idle as f64 / 1_000_000_000_f64;
            let busy: f64 = timings.busy as f64 / 1_000_000_000_f64;
            let name = span.metadata().name();
            if let Some(subgraph_name) = timings.subgraph.take() {
                ::tracing::info!(histogram.apollo_router_span = duration, kind = %"duration", span = %name, subgraph = %subgraph_name);
                ::tracing::info!(histogram.apollo_router_span = idle, kind = %"idle", span = %name, subgraph = %subgraph_name);
                ::tracing::info!(histogram.apollo_router_span = busy, kind = %"busy", span = %name, subgraph = %subgraph_name);
            } else {
                ::tracing::info!(histogram.apollo_router_span = duration, kind = %"duration", span = %name);
                ::tracing::info!(histogram.apollo_router_span = idle, kind = %"idle", span = %name);
                ::tracing::info!(histogram.apollo_router_span = busy, kind = %"busy", span = %name);
            }
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();

        if let Some(timings) = extensions.get_mut::<Timings>() {
            let now = Instant::now();
            timings.idle += (now - timings.last).as_nanos() as i64;
            timings.last = now;
        }
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();

        if let Some(timings) = extensions.get_mut::<Timings>() {
            let now = Instant::now();
            timings.busy += (now - timings.last).as_nanos() as i64;
            timings.last = now;
        }
    }
}

struct Timings {
    idle: i64,
    busy: i64,
    last: Instant,
    start: Instant,
    subgraph: Option<String>,
}

impl Timings {
    fn new() -> Self {
        Self {
            idle: 0,
            busy: 0,
            last: Instant::now(),
            start: Instant::now(),
            subgraph: None,
        }
    }
}

struct ValueVisitor<'a> {
    timings: &'a mut Timings,
}

impl<'a> Visit for ValueVisitor<'a> {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == SUBGRAPH_ATTRIBUTE_NAME {
            self.timings.subgraph = Some(value.to_string());
        }
    }
}
