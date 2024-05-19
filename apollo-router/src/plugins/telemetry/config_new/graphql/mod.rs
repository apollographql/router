use std::sync::Arc;

use crate::{metrics, Context};
use opentelemetry::metrics::MeterProvider;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::instruments::{CustomInstruments, Increment, InstrumentsConfig, METER_NAME};
use crate::plugins::demand_control::cost_calculator::schema_aware_response;
use crate::plugins::demand_control::cost_calculator::schema_aware_response::{TypedValue, Visitor};
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::graphql::attributes::GraphQLAttributes;
use crate::plugins::telemetry::config_new::graphql::selectors::{FieldLength, GraphQLSelector};
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::supergraph;

pub(crate) mod attributes;
pub(crate) mod selectors;

static FIELD_LENGTH: &str = "graphql.field.length";

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLInstrumentsConfig {
    /// A histogram of the length of a selected field in the GraphQL response
    #[serde(rename = "field.length")]
    pub(crate) field_length:
        DefaultedStandardInstrument<Extendable<GraphQLAttributes, GraphQLSelector>>,
}

impl DefaultForLevel for GraphQLInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.field_length
            .defaults_for_level(requirement_level, kind);
    }
}

impl GraphQLInstrumentsConfig {
    fn histogram(
        name: &'static str,
        config: &DefaultedStandardInstrument<Extendable<GraphQLAttributes, GraphQLSelector>>,
        selector: GraphQLSelector,
    ) -> CustomHistogram<
        supergraph::Request,
        supergraph::Response,
        GraphQLAttributes,
        GraphQLSelector,
    > {
        let meter = crate::metrics::meter_provider().meter(METER_NAME);
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
                increment: Increment::FieldCustom(None),
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

pub(crate) type GraphQLCustomInstruments = CustomInstruments<
    supergraph::Request,
    supergraph::Response,
    GraphQLAttributes,
    GraphQLSelector,
>;

pub(crate) struct GraphQLInstruments {
    pub(crate) field_length: Option<
        CustomHistogram<
            supergraph::Request,
            supergraph::Response,
            GraphQLAttributes,
            GraphQLSelector,
        >,
    >,
    pub(crate) custom: GraphQLCustomInstruments,
}

impl From<&InstrumentsConfig> for GraphQLInstruments {
    fn from(value: &InstrumentsConfig) -> Self {
        let meter = metrics::meter_provider().meter(METER_NAME);
        GraphQLInstruments {
            field_length: value.graphql.attributes.field_length.is_enabled().then(|| {
                let mut nb_attributes = 0;
                let selectors = match &value.graphql.attributes.field_length {
                    DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => {
                        None
                    }
                    DefaultedStandardInstrument::Extendable { attributes } => {
                        nb_attributes = attributes.custom.len();
                        Some(attributes.clone())
                    }
                };
                CustomHistogram {
                    inner: Mutex::new(CustomHistogramInner {
                        increment: Increment::FieldCustom(None),
                        condition: Condition::True,
                        histogram: Some(meter.f64_histogram(FIELD_LENGTH).init()),
                        attributes: Vec::with_capacity(nb_attributes),
                        selector: Some(Arc::new(GraphQLSelector::FieldLength {
                            field_length: FieldLength::Value,
                        })),
                        selectors,
                        updated: false,
                    }),
                }
            }),
            custom: CustomInstruments::new(&value.graphql.custom),
        }
    }
}

impl Instrumented for GraphQLInstruments {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &Self::Request) {
        if let Some(field_length) = &self.field_length {
            field_length.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(field_length) = &self.field_length {
            field_length.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &crate::Context) {
        if let Some(field_length) = &self.field_length {
            field_length.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        if !self.custom.is_empty() || self.field_length.is_some() {
            if let Some(executable_document) = ctx.unsupported_executable_document() {
                if let Ok(schema) =
                    schema_aware_response::SchemaAwareResponse::new(&executable_document, response)
                {
                    GraphQLInstrumentsVisitor {
                        ctx,
                        instruments: self,
                    }
                    .visit(&schema.value)
                }
            }
        }
    }

    fn on_response_field(&self, typed_value: &TypedValue, ctx: &Context) {
        if let Some(field_length) = &self.field_length {
            field_length.on_response_field(typed_value, ctx);
        }
        self.custom.on_response_field(typed_value, ctx);
    }
}

struct GraphQLInstrumentsVisitor<'a> {
    ctx: &'a Context,
    instruments: &'a GraphQLInstruments,
}

impl<'a> Visitor for GraphQLInstrumentsVisitor<'a> {
    fn visit(&self, value: &TypedValue) {
        self.instruments.on_response_field(value, self.ctx);
        self.visit_value(value);
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
        //let instruments: GraphQLInstruments = config.into();

        //      assert!(true)
    }

    fn config(config: &'static str) -> GraphQLInstrumentsConfig {
        let config: serde_json::Value = serde_yaml::from_str(config).expect("config");
        let graphql_instruments = jsonpath_lib::select(&config, "$..graphql");

        serde_json::from_value((*graphql_instruments.unwrap().first().unwrap()).clone())
            .expect("config")
    }
}
