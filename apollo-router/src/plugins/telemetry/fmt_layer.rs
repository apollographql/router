use tracing_subscriber::Layer;

use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::logging::Format;
use crate::plugins::telemetry::config_new::logging::StdOut;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::json::Json;
use crate::plugins::telemetry::formatters::json::JsonFields;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::reload::LayeredTracer;
use crate::plugins::telemetry::resource::ConfigResource;

pub(crate) fn create_fmt_layer(
    config: &config::Conf,
) -> Box<dyn Layer<LayeredTracer> + Send + Sync> {
    match &config.exporters.logging.stdout {
        StdOut { enabled, format } if *enabled => {
            match format {
                Format::Json(format_config) => {
                    let format = Json::new(
                        config.exporters.logging.common.to_resource(),
                        format_config.clone(),
                    );
                    tracing_subscriber::fmt::layer()
                        .event_format(FilteringFormatter::new(format, filter_metric_events))
                        .fmt_fields(JsonFields {})
                        .boxed()
                }

                Format::Text(format_config) => {
                    let format = Text::new(
                        config.exporters.logging.common.to_resource(),
                        format_config.clone(),
                    );
                    tracing_subscriber::fmt::layer()
                        .event_format(FilteringFormatter::new(format, filter_metric_events))
                        // This is JSON for a reason!
                        // Later on we need to be able to remove some fields during rendering, so having the fields as a structured format makes this possible.
                        .fmt_fields(JsonFields {})
                        .boxed()
                }
            }
        }
        _ => NoOpLayer.boxed(),
    }
}

struct NoOpLayer;

impl Layer<LayeredTracer> for NoOpLayer {}
