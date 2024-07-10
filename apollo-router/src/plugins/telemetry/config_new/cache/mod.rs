use std::sync::Arc;

use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::ExecutableDocument;
use opentelemetry::metrics::MeterProvider;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use tower::BoxError;

use super::instruments::CustomCounter;
use super::instruments::CustomCounterInner;
use super::instruments::CustomInstruments;
use super::instruments::Increment;
use super::instruments::InstrumentsConfig;
use super::instruments::METER_NAME;
use super::selectors::SubgraphSelector;
use super::selectors::SubgraphValue;
use crate::graphql::ResponseVisitor;
use crate::metrics;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLValue;
use crate::plugins::telemetry::config_new::graphql::selectors::ListLength;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::Context;

pub(crate) mod attributes;
pub(crate) mod selectors;

static CACHE_HIT_METRIC: &str = "apollo.router.operations.entity.cache.hit";
static CACHE_MISS_METRIC: &str = "apollo.router.operations.entity.cache.miss";

#[derive(Default)]
struct CacheAttributes;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CacheInstrumentsConfig {
    /// A counter of times we have a cache hit
    #[serde(rename = "apollo.router.operations.entity.cache.hit")]
    pub(crate) cache_hit:
        DefaultedStandardInstrument<Extendable<CacheAttributes, SubgraphSelector>>,

    /// A counter of the number of times we have a cache miss
    #[serde(rename = "apollo.router.operations.entity.cache.miss")]
    pub(crate) cache_miss:
        DefaultedStandardInstrument<Extendable<CacheAttributes, SubgraphSelector>>,
}

impl DefaultForLevel for CacheInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        if self.cache_hit.is_enabled() {
            self.cache_hit.defaults_for_level(requirement_level, kind);
        }
        if self.cache_miss.is_enabled() {
            self.cache_miss.defaults_for_level(requirement_level, kind);
        }
    }
}

pub(crate) type CacheCustomInstruments = CustomInstruments<
    subgraph::Request,
    subgraph::Response,
    CacheAttributes,
    SubgraphSelector,
    SubgraphValue,
>;

pub(crate) struct CacheInstruments {
    pub(crate) cache_hit: Option<
        CustomCounter<subgraph::Request, subgraph::Response, CacheAttributes, SubgraphSelector>,
    >,
    pub(crate) cache_miss: Option<
        CustomCounter<subgraph::Request, subgraph::Response, CacheAttributes, SubgraphSelector>,
    >,
    // pub(crate) custom: CacheCustomInstruments,
}

impl From<&InstrumentsConfig> for CacheInstruments {
    fn from(value: &InstrumentsConfig) -> Self {
        let meter = metrics::meter_provider().meter(METER_NAME);
        CacheInstruments {
            cache_hit: value.graphql.attributes.list_length.is_enabled().then(|| {
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
                    inner: Mutex::new(CustomCounterInner {
                        increment: Increment::FieldCustom(None),
                        condition: Condition::True,
                        counter: Some(
                            meter
                                .f64_counter(CACHE_HIT_METRIC)
                                .with_description(
                                    "Entity cache hit operations at the subgraph level",
                                )
                                .init(),
                        ),
                        attributes: Vec::with_capacity(nb_attributes),
                        selector: Some(Arc::new(SubgraphSelector::ListLength {
                            list_length: ListLength::Value,
                        })),
                        selectors,
                        incremented: false,
                    }),
                }
            }),
            cache_miss: value
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
                            counter: Some(
                                meter
                                    .f64_counter(CACHE_MISS_METRIC)
                                    .with_description(
                                        "Entity cache miss operations at the subgraph level",
                                    )
                                    .init(),
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: None,
                            selectors,
                            incremented: false,
                        }),
                    }
                }),
            // custom: CustomInstruments::new(&value.graphql.custom),
        }
    }
}

impl Instrumented for CacheInstruments {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &Self::Request) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_request(request);
        }
        if let Some(field_execution) = &self.cache_miss {
            field_execution.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_response(response);
        }
        if let Some(field_execution) = &self.cache_miss {
            field_execution.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &crate::Context) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_error(error, ctx);
        }
        if let Some(field_execution) = &self.cache_miss {
            field_execution.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_response_event(response, ctx);
        }
        if let Some(field_execution) = &self.cache_miss {
            field_execution.on_response_event(response, ctx);
        }
        self.custom.on_response_event(response, ctx);

        if !self.custom.is_empty() || self.cache_hit.is_some() || self.cache_miss.is_some() {
            if let Some(executable_document) = ctx.unsupported_executable_document() {
                CacheInstrumentsVisitor {
                    ctx,
                    instruments: self,
                }
                .visit(&executable_document, response);
            }
        }
    }

    fn on_response_field(&self, ty: &NamedType, field: &Field, value: &Value, ctx: &Context) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_response_field(ty, field, value, ctx);
        }
        if let Some(field_execution) = &self.cache_miss {
            field_execution.on_response_field(ty, field, value, ctx);
        }
        self.custom.on_response_field(ty, field, value, ctx);
    }
}

struct CacheInstrumentsVisitor<'a> {
    ctx: &'a Context,
    instruments: &'a CacheInstruments,
}

impl<'a> ResponseVisitor for CacheInstrumentsVisitor<'a> {
    fn visit_field(
        &mut self,
        request: &ExecutableDocument,
        ty: &NamedType,
        field: &Field,
        value: &Value,
    ) {
        self.instruments
            .on_response_field(ty, field, value, self.ctx);

        match value {
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(request, field.ty().inner_named_type(), field, item);
                }
            }
            Value::Object(children) => {
                self.visit_selections(request, &field.selection_set, children);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod test {

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;
    use crate::Configuration;

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

            assert_histogram_sum!(
                "graphql.field.list.length",
                2.0,
                "graphql.field.name" = "users",
                "graphql.field.type" = "User",
                "graphql.type.name" = "Query"
            );
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

            assert_histogram_sum!(
                "graphql.field.list.length",
                2.0,
                "graphql.field.name" = "ships",
                "graphql.field.type" = "Ship",
                "graphql.type.name" = "Query"
            );
            assert_histogram_sum!(
                "graphql.field.list.length",
                2.0,
                "graphql.field.name" = "users",
                "graphql.field.type" = "User",
                "graphql.type.name" = "Query"
            );
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

            assert_histogram_not_exists!("graphql.field.list.length", f64);
        }
        .with_metrics()
        .await;
    }

    #[test_log::test(tokio::test)]
    async fn filtered_metric_publishing() {
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
                .config(include_str!("fixtures/filtered_field_length.router.yaml"))
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

            assert_histogram_sum!("ships.list.length", 2.0);
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
        context
            .extensions()
            .with_lock(|mut lock| lock.insert(query));

        context
    }
}
