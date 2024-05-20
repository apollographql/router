use std::sync::Arc;

use crate::{metrics, Context};
use opentelemetry::metrics::MeterProvider;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::instruments::{
    CustomCounter, CustomCounterInner, CustomInstruments, Increment, InstrumentsConfig, METER_NAME,
};
use crate::plugins::demand_control::cost_calculator::schema_aware_response;
use crate::plugins::demand_control::cost_calculator::schema_aware_response::{TypedValue, Visitor};
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::graphql::attributes::GraphQLAttributes;
use crate::plugins::telemetry::config_new::graphql::selectors::{GraphQLSelector, ListLength};
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::supergraph;

pub(crate) mod attributes;
pub(crate) mod selectors;

static FIELD_LENGTH: &str = "graphql.field.list.length";
static FIELD_EXECUTION: &str = "graphql.field.execution";

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLInstrumentsConfig {
    /// A histogram of the length of a selected field in the GraphQL response
    #[serde(rename = "list.length")]
    pub(crate) list_length:
        DefaultedStandardInstrument<Extendable<GraphQLAttributes, GraphQLSelector>>,

    /// A counter of the number of times a field is used.
    #[serde(rename = "field.execution")]
    pub(crate) field_execution:
        DefaultedStandardInstrument<Extendable<GraphQLAttributes, GraphQLSelector>>,
}

impl DefaultForLevel for GraphQLInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        if self.list_length.is_enabled() {
            self.list_length.defaults_for_level(requirement_level, kind);
        }
        if self.field_execution.is_enabled() {
            self.field_execution
                .defaults_for_level(requirement_level, kind);
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
    pub(crate) list_length: Option<
        CustomHistogram<
            supergraph::Request,
            supergraph::Response,
            GraphQLAttributes,
            GraphQLSelector,
        >,
    >,
    pub(crate) field_execution: Option<
        CustomCounter<
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
            list_length: value.graphql.attributes.list_length.is_enabled().then(|| {
                let mut nb_attributes = 0;
                let selectors = match &value.graphql.attributes.list_length {
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
                        selector: Some(Arc::new(GraphQLSelector::ListLength {
                            list_length: ListLength::Value,
                        })),
                        selectors,
                        updated: false,
                    }),
                }
            }),
            field_execution: value
                .graphql
                .attributes
                .field_execution
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &value.graphql.attributes.field_execution {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };
                    CustomCounter {
                        inner: Mutex::new(CustomCounterInner {
                            increment: Increment::FieldUnit,
                            condition: Condition::True,
                            counter: Some(meter.f64_counter(FIELD_EXECUTION).init()),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: None,
                            selectors,
                            incremented: false,
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
        if let Some(field_length) = &self.list_length {
            field_length.on_request(request);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(field_length) = &self.list_length {
            field_length.on_response(response);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &crate::Context) {
        if let Some(field_length) = &self.list_length {
            field_length.on_error(error, ctx);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        if !self.custom.is_empty() || self.list_length.is_some() || self.field_execution.is_some() {
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
        if let Some(field_length) = &self.list_length {
            field_length.on_response_field(typed_value, ctx);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_response_field(typed_value, ctx);
        }
        self.custom.on_response_field(typed_value, ctx);
    }
}

struct GraphQLInstrumentsVisitor<'a> {
    ctx: &'a Context,
    instruments: &'a GraphQLInstruments,
}

impl<'a> Visitor for GraphQLInstrumentsVisitor<'a> {
    fn visit_field(&self, value: &TypedValue) {
        self.instruments.on_response_field(value, self.ctx);
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
