use opentelemetry::metrics::MeterProvider;
use opentelemetry::KeyValue;
use opentelemetry_api::metrics::Histogram;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::demand_control::cost_calculator::schema_aware_response;
use crate::plugins::demand_control::cost_calculator::schema_aware_response::TypedValue;
use crate::plugins::telemetry::config_new::instruments::METER_NAME;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLInstrumentationConfig {
    #[serde(rename = "field.length")]
    field_length: Option<bool>,
}

impl GraphQLInstrumentationConfig {
    pub(crate) fn get_field_length_histogram(&self) -> Option<GraphQLFieldLengthHistogram> {
        self.field_length
            .is_some()
            .then(|| GraphQLFieldLengthHistogram::new())
    }
}

pub(crate) struct GraphQLFieldLengthHistogram {
    histogram: Histogram<f64>,
}

impl GraphQLFieldLengthHistogram {
    pub(crate) fn new() -> Self {
        let meter = crate::metrics::meter_provider().meter(METER_NAME);
        Self {
            histogram: meter.f64_histogram("graphql.field.length").init(),
        }
    }

    pub(crate) fn record(&self, field_name: &str, field_length: f64) {
        self.histogram.record(
            field_length,
            &[KeyValue::new("graphql.field.name", field_name.to_string())],
        );
    }
}

impl schema_aware_response::Visitor for GraphQLFieldLengthHistogram {
    fn visit(&mut self, value: &TypedValue) {
        match value {
            TypedValue::Array(field, items) => {
                self.record(field.name.as_str(), items.len() as f64);
                self.visit_array(field, items);
            }
            TypedValue::Object(f, children) => self.visit_object(f, children),
            TypedValue::Root(children) => self.visit_root(children),
            _ => {}
        }
    }
}

#[cfg(test)]
mod test {
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;

    #[tokio::test]
    async fn valid_config() {
        PluginTestHarness::<Telemetry>::builder()
            .config(include_str!("fixtures/graphql_field_length.router.yaml"))
            .build()
            .await;
    }
}
