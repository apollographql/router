use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::instruments::Increment;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Selectors;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLInstrumentsConfig {
    /// A histogram of the length of a selected field in the GraphQL response
    #[serde(rename = "field.length")]
    field_length: DefaultedStandardInstrument<Extendable<GraphQLAttributes, GraphQLSelector>>,
}

impl DefaultForLevel for GraphQLInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: super::attributes::DefaultAttributeRequirementLevel,
        kind: crate::plugins::telemetry::otlp::TelemetryDataKind,
    ) {
        self.field_length
            .defaults_for_level(requirement_level, kind);
    }
}

impl GraphQLInstrumentsConfig {
    fn to_instruments(&self) -> GraphQLInstruments {
        let field_length = self.field_length.is_enabled().then(|| {
            GraphQLInstruments::histogram(
                "field.length",
                &self.field_length,
                GraphQLSelector::FieldLength,
            )
        });
        GraphQLInstruments { field_length }
    }
}

#[derive(Default)]
pub(crate) struct GraphQLInstruments {
    field_length: Option<CustomHistogram<Request, Response, GraphQLAttributes, GraphQLSelector>>,
}

impl Instrumented for GraphQLInstruments {
    type Request = Request;
    type Response = Response;
    type EventResponse = ();

    fn on_request(&self, _request: &Self::Request) {}

    fn on_response(&self, response: &Self::Response) {
        if let Some(field_length) = &self.field_length {
            field_length.on_response(response);
        }
    }

    fn on_error(&self, _error: &BoxError, _ctx: &crate::Context) {}
}

impl GraphQLInstruments {
    fn histogram(
        name: &'static str,
        config: &DefaultedStandardInstrument<Extendable<GraphQLAttributes, GraphQLSelector>>,
        selector: GraphQLSelector,
    ) -> CustomHistogram<Request, Response, GraphQLAttributes, GraphQLSelector> {
        let meter = crate::metrics::meter_provider()
            .meter(crate::plugins::telemetry::config_new::instruments::METER_NAME);
        let mut nb_attributes = 0;
        let selectors = match config {
            DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => None,
            DefaultedStandardInstrument::Extendable { attributes } => {
                nb_attributes = attributes.custom.len();
                Some(attributes.clone())
            }
        };

        CustomHistogram {
            inner: Mutex::new(CustomHistogramInner {
                increment: Increment::EventCustom(None),
                condition: Condition::True,
                histogram: Some(meter.f64_histogram(name).init()),
                attributes: Vec::with_capacity(nb_attributes),
                selector: Some(Arc::new(selector)),
                selectors,
                updated: false,
            }),
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLAttributes {
    field_name: Option<bool>,
    type_name: Option<bool>,
}

impl DefaultForLevel for GraphQLAttributes {
    fn defaults_for_level(
        &mut self,
        _requirement_level: super::attributes::DefaultAttributeRequirementLevel,
        _kind: crate::plugins::telemetry::otlp::TelemetryDataKind,
    ) {
        // No-op?
    }
}

impl Selectors for GraphQLAttributes {
    type Request = Request;
    type Response = Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Vec<KeyValue> {
        Vec::with_capacity(0)
    }

    fn on_response(&self, response: &Self::Response) -> Vec<KeyValue> {
        todo!()
    }

    fn on_error(&self, error: &BoxError) -> Vec<KeyValue> {
        Vec::with_capacity(0)
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum GraphQLSelector {
    FieldLength,
}

impl Selector for GraphQLSelector {
    type Request = Request;
    type Response = Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Option<opentelemetry::Value> {
        None
    }

    fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value> {
        todo!()
    }

    fn on_error(&self, error: &BoxError) -> Option<opentelemetry::Value> {
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;

    #[tokio::test]
    async fn valid_config() {
        PluginTestHarness::<Telemetry>::builder()
            .config(include_str!("fixtures/graphql_field_length.router.yaml"))
            .build()
            .await;
    }

    #[test]
    fn conversion_to_instruments() {
        let config = config(include_str!("fixtures/graphql_field_length.router.yaml"));
        let instruments = config.to_instruments();

        assert!(true)
    }

    fn config(config: &'static str) -> GraphQLInstrumentsConfig {
        let config: serde_json::Value = serde_yaml::from_str(config).expect("config");
        let graphql_instruments = jsonpath_lib::select(&config, "$..graphql");

        serde_json::from_value((*graphql_instruments.unwrap().first().unwrap()).clone())
            .expect("config")
    }
}
