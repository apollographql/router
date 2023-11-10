use tracing_subscriber::Layer;

use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::logging::Format;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::json::Json;
use crate::plugins::telemetry::formatters::json::JsonFields;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::reload::LayeredTracer;

pub(crate) fn create_fmt_layer(
    config: &config::Conf,
) -> Box<dyn Layer<LayeredTracer> + Send + Sync> {
    let config = &config.logging.stdout;
    match &config.format {
        Format::Json(config) => {
            let format = Json::new(config.clone());
            tracing_subscriber::fmt::layer()
                .event_format(FilteringFormatter::new(format, filter_metric_events))
                .fmt_fields(JsonFields {})
                .boxed()
        }

        Format::Text(config) => {
            let format = Text::new(config.clone());
            tracing_subscriber::fmt::layer()
                .event_format(FilteringFormatter::new(format, filter_metric_events))
                // This is JSON for a reason!
                // Later on we need to be able to remove some fields during rendering, so having the fields as a structured format makes this possible.
                .fmt_fields(JsonFields {})
                .boxed()
        }
    }
}
