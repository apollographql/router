use tracing_subscriber::fmt::format::JsonFields;
use tracing_subscriber::Layer;

use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::logging::Format;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::json::Json;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::reload::LayeredTracer;

pub(crate) fn create_fmt_layer(
    config: &config::Conf,
) -> Box<dyn Layer<LayeredTracer> + Send + Sync> {
    let config = &config.new_logging.stdout;
    match &config.format {
        Format::Json(config) => {
            let format = Json::default()
                .with_level(config.display_level)
                .with_target(config.display_target)
                .with_level(config.display_level)
                .with_span_list(config.display_span_list)
                .with_current_span(config.display_current_span)
                .with_file(config.display_filename)
                .with_line_number(config.display_line_number)
                .with_thread_ids(config.display_thread_id)
                .with_thread_names(config.display_thread_name)
                .with_timestamp(config.display_timestamp);

            tracing_subscriber::fmt::layer()
                .event_format(FilteringFormatter::new(format, filter_metric_events))
                .fmt_fields(JsonFields::new())
                .boxed()
        }

        Format::Text(config) => {
            let format = Text::default()
                .with_level(config.display_level)
                .with_target(config.display_target)
                .with_level(config.display_level)
                .with_file(config.display_filename)
                .with_line_number(config.display_line_number)
                .with_thread_ids(config.display_thread_id)
                .with_thread_names(config.display_thread_name)
                .with_timestamp(config.display_timestamp);
            tracing_subscriber::fmt::layer()
                .event_format(FilteringFormatter::new(format, filter_metric_events))
                // This is JSON for a reason!
                // Later on we need to be able to remove some fields during rendering, so having the fields as a structured format makes this possible.
                .fmt_fields(JsonFields::new())
                .boxed()
        }
    }
}
