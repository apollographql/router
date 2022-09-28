use std::fmt;

use ansi_term::Color;
use ansi_term::Style;
use tracing_core::Event;
use tracing_core::Level;
use tracing_core::Subscriber;
use tracing_subscriber::fmt::format::FormatEvent;
use tracing_subscriber::fmt::format::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::registry::LookupSpan;

#[derive(Debug, Clone)]
pub(crate) struct TextFormatter {
    pub(crate) timer: SystemTime,
}

impl Default for TextFormatter {
    fn default() -> Self {
        Self::new()
    }
}

impl TextFormatter {
    const TRACE_STR: &'static str = "TRACE";
    const DEBUG_STR: &'static str = "DEBUG";
    const INFO_STR: &'static str = " INFO";
    const WARN_STR: &'static str = " WARN";
    const ERROR_STR: &'static str = "ERROR";

    pub(crate) fn new() -> Self {
        Self {
            timer: Default::default(),
        }
    }

    #[inline]
    fn format_level(&self, level: &Level, writer: &mut Writer<'_>) -> fmt::Result {
        if writer.has_ansi_escapes() {
            match *level {
                Level::TRACE => write!(writer, "{}", Color::Purple.paint(TextFormatter::TRACE_STR)),
                Level::DEBUG => write!(writer, "{}", Color::Blue.paint(TextFormatter::DEBUG_STR)),
                Level::INFO => write!(writer, "{}", Color::Green.paint(TextFormatter::INFO_STR)),
                Level::WARN => write!(writer, "{}", Color::Yellow.paint(TextFormatter::WARN_STR)),
                Level::ERROR => write!(writer, "{}", Color::Red.paint(TextFormatter::ERROR_STR)),
            }?;
        } else {
            match *level {
                Level::TRACE => write!(writer, "{}", TextFormatter::TRACE_STR),
                Level::DEBUG => write!(writer, "{}", TextFormatter::DEBUG_STR),
                Level::INFO => write!(writer, "{}", TextFormatter::INFO_STR),
                Level::WARN => write!(writer, "{}", TextFormatter::WARN_STR),
                Level::ERROR => write!(writer, "{}", TextFormatter::ERROR_STR),
            }?;
        }
        writer.write_char(' ')
    }

    #[inline]
    fn format_timestamp(&self, writer: &mut Writer<'_>) -> fmt::Result {
        if writer.has_ansi_escapes() {
            let style = Style::new().dimmed();
            write!(writer, "{}", style.prefix())?;

            // If getting the timestamp failed, don't bail --- only bail on
            // formatting errors.
            if self.timer.format_time(writer).is_err() {
                writer.write_str("<unknown time>")?;
            }

            write!(writer, "{}", style.suffix())?;
        } else {
            // If getting the timestamp failed, don't bail --- only bail on
            // formatting errors.
            if self.timer.format_time(writer).is_err() {
                writer.write_str("<unknown time>")?;
            }
        }
        writer.write_char(' ')
    }

    #[inline]
    fn format_target(&self, target: &str, writer: &mut Writer<'_>) -> fmt::Result {
        if writer.has_ansi_escapes() {
            let style = Style::new().dimmed();
            write!(writer, "{}", style.prefix())?;
            write!(writer, "{}:", target)?;
            write!(writer, "{}", style.suffix())?;
        } else {
            write!(writer, "{}:", target)?;
        }
        writer.write_char(' ')
    }
}

impl<S, N> FormatEvent<S, N> for TextFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();

        self.format_timestamp(&mut writer)?;

        self.format_level(meta.level(), &mut writer)?;

        self.format_target(meta.target(), &mut writer)?;

        ctx.format_fields(writer.by_ref(), event)?;

        writeln!(writer)
    }
}
