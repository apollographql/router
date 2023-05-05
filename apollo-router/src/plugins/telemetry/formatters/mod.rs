//! Our formatters and visitors used for logging
pub(crate) mod json;
pub(crate) mod text;

use std::cell::RefCell;
use std::fmt;
use std::io;
use std::marker::PhantomData;

use tracing::Event;
use tracing::Subscriber;
use tracing_subscriber::fmt::format;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::FormattedFields;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use super::metrics::METRIC_PREFIX_COUNTER;
use super::metrics::METRIC_PREFIX_HISTOGRAM;
use super::metrics::METRIC_PREFIX_MONOTONIC_COUNTER;
use super::metrics::METRIC_PREFIX_VALUE;

pub(crate) const TRACE_ID_FIELD_NAME: &str = "trace_id";

/// `FilteringFormatter` is useful if you want to not filter the entire event but only want to not display it
/// ```ignore
/// use tracing_core::Event;
/// use tracing_subscriber::fmt::format::{Format};
/// tracing_subscriber::fmt::fmt()
/// .event_format(FilteringFormatter::new(
///     Format::default().pretty(),
///     // Do not display the event if an attribute name starts with "counter"
///     |event: &Event| !event.metadata().fields().iter().any(|f| f.name().starts_with("counter")),
/// ))
/// .finish();
/// ```
pub(crate) struct FilteringFormatter<T, F> {
    inner: T,
    filter_fn: F,
}

impl<T, F> FilteringFormatter<T, F>
where
    F: Fn(&tracing::Event<'_>) -> bool,
{
    pub(crate) fn new(inner: T, filter_fn: F) -> Self {
        Self { inner, filter_fn }
    }
}

impl<T, F, S, N> FormatEvent<S, N> for FilteringFormatter<T, F>
where
    T: FormatEvent<S, N>,
    F: Fn(&tracing::Event<'_>) -> bool,
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        if (self.filter_fn)(event) {
            self.inner.format_event(ctx, writer, event)
        } else {
            Ok(())
        }
    }
}

// Function to filter metric event for the filter formatter
pub(crate) fn filter_metric_events(event: &tracing::Event<'_>) -> bool {
    !event.metadata().fields().iter().any(|f| {
        f.name().starts_with(METRIC_PREFIX_COUNTER)
            || f.name().starts_with(METRIC_PREFIX_HISTOGRAM)
            || f.name().starts_with(METRIC_PREFIX_MONOTONIC_COUNTER)
            || f.name().starts_with(METRIC_PREFIX_VALUE)
    })
}

pub(crate) struct FormattingLayer<
    S,
    N = format::DefaultFields,
    E = format::Format<format::Full>,
    W = fn() -> io::Stdout,
> {
    make_writer: W,
    fmt_fields: N,
    fmt_event: E,
    //fmt_span: format::FmtSpanConfig,
    is_ansi: bool,
    log_internal_errors: bool,
    _inner: PhantomData<fn(S)>,
}

impl<S, N, E, W> FormattingLayer<S, N, E, W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
    E: FormatEvent<S, N> + 'static,
    W: for<'writer> MakeWriter<'writer> + 'static,
{
    #[inline]
    fn make_ctx<'a>(&'a self, ctx: Context<'a, S>, event: &'a Event<'a>) -> FmtContext<'a, S, N> {
        FmtContext {
            ctx,
            fmt_fields: &self.fmt_fields,
            event,
        }
    }
}

impl<S, N, E, W> Layer<S> for FormattingLayer<S, N, E, W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
    E: FormatEvent<S, N> + 'static,
    W: for<'writer> MakeWriter<'writer> + 'static,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        thread_local! {
            static BUF: RefCell<String> = RefCell::new(String::new());
        }

        BUF.with(|buf| {
            let borrow = buf.try_borrow_mut();
            let mut a;
            let mut b;
            let mut buf = match borrow {
                Ok(buf) => {
                    a = buf;
                    &mut *a
                }
                _ => {
                    b = String::new();
                    &mut b
                }
            };

let formatted_fields = FormattedFields::new(String::new());
let writer = formatted_fields.as_writer();
            let ctx = self.make_ctx(ctx, event);
            if self
                .fmt_event
                .format_event(
                    &ctx,
                    //format::Writer::new(&mut buf).with_ansi(self.is_ansi),
                    writer,
                    event,
                )
                .is_ok()
            {
                let mut writer = self.make_writer.make_writer_for(event.metadata());
                let res = io::Write::write_all(&mut writer, buf.as_bytes());
                if self.log_internal_errors {
                    if let Err(e) = res {
                        eprintln!("[tracing-subscriber] Unable to write an event to the Writer for this Subscriber! Error: {}\n", e);
                    }
                }
            }

            buf.clear();
        });
    }
}
