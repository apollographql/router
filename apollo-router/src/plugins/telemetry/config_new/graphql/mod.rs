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
        match self.field_length {
            Some(true) => Some(GraphQLFieldLengthHistogram::new()),
            _ => None,
        }
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
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::supergraph;
    use crate::Configuration;
    use crate::Context;

    #[test_log::test(tokio::test)]
    async fn basic_metric_publishing() {
        async {
            let schema_str = include_str!(
                "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
            );
            let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_named_query.graphql");


            let request = supergraph::Request::fake_builder()
                .query(query_str)
                .context(context(schema_str, query_str))
                .build()
                .unwrap();

            let harness = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("fixtures/field_length_enabled.router.yaml"))
                .schema(schema_str)
                .build()
                .await;

            harness
                .call_supergraph(request, |req| {
                    let response: serde_json::Value = serde_json::from_str(include_str!(
                        "../../../demand_control/cost_calculator/fixtures/federated_ships_named_response.json"
                    ))
                    .unwrap();
                    supergraph::Response::builder()
                        .data(response["data"].clone())
                        .context(req.context)
                        .build()
                        .unwrap()
                })
                .await
                .unwrap();

            assert_histogram_sum!("graphql.field.length", 2.0, "graphql.field.name" = "users");
        }
        .with_metrics()
        .await;
    }

    #[test_log::test(tokio::test)]
    async fn multiple_fields_metric_publishing() {
        async {
            let schema_str = include_str!(
                "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
            );
            let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_query.graphql");


            let request = supergraph::Request::fake_builder()
                .query(query_str)
                .context(context(schema_str, query_str))
                .build()
                .unwrap();

            let harness = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("fixtures/field_length_enabled.router.yaml"))
                .schema(schema_str)
                .build()
                .await;

            harness
                .call_supergraph(request, |req| {
                    let response: serde_json::Value = serde_json::from_str(include_str!(
                        "../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_response.json"
                    ))
                    .unwrap();
                    supergraph::Response::builder()
                        .data(response["data"].clone())
                        .context(req.context)
                        .build()
                        .unwrap()
                })
                .await
                .unwrap();

            assert_histogram_sum!("graphql.field.length", 2.0, "graphql.field.name" = "ships");
            assert_histogram_sum!("graphql.field.length", 2.0, "graphql.field.name" = "users");
        }
        .with_metrics()
        .await;
    }

    #[test_log::test(tokio::test)]
    async fn disabled_metric_publishing() {
        async {
            let schema_str = include_str!(
                "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
            );
            let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_named_query.graphql");


            let request = supergraph::Request::fake_builder()
                .query(query_str)
                .context(context(schema_str, query_str))
                .build()
                .unwrap();

            let harness = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("fixtures/field_length_disabled.router.yaml"))
                .schema(schema_str)
                .build()
                .await;

            harness
                .call_supergraph(request, |req| {
                    let response: serde_json::Value = serde_json::from_str(include_str!(
                        "../../../demand_control/cost_calculator/fixtures/federated_ships_named_response.json"
                    ))
                    .unwrap();
                    supergraph::Response::builder()
                        .data(response["data"].clone())
                        .context(req.context)
                        .build()
                        .unwrap()
                })
                .await
                .unwrap();

            assert_histogram_not_exists!("graphql.field.length", f64);
        }
        .with_metrics()
        .await;
    }

    fn context(schema_str: &str, query_str: &str) -> Context {
        let schema = crate::spec::Schema::parse_test(schema_str, &Default::default()).unwrap();
        let query =
            crate::spec::Query::parse_document(query_str, None, &schema, &Configuration::default())
                .unwrap();
        let context = Context::new();
        context.extensions().lock().insert(query);

        context
    }
}
