use std::sync::Arc;

use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::ExecutableDocument;
use attributes::CacheAttributes;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::Key;
use opentelemetry::KeyValue;
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
use super::selectors::CacheKind;
use super::selectors::EntityType;
use super::selectors::SubgraphSelector;
use super::selectors::SubgraphValue;
use super::Selectors;
use crate::graphql::ResponseVisitor;
use crate::metrics;
use crate::plugins::cache::entity::CacheHitMiss;
use crate::plugins::cache::entity::CacheSubgraph;
use crate::plugins::telemetry::config::AttributeValue;
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
            cache_hit: value.cache.attributes.cache_hit.is_enabled().then(|| {
                let mut nb_attributes = 0;
                let selectors = match &value.cache.attributes.cache_hit {
                    DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => {
                        None
                    }
                    DefaultedStandardInstrument::Extendable { attributes } => {
                        nb_attributes = attributes.custom.len();
                        Some(attributes.clone())
                    }
                };
                CustomCounter {
                    inner: Mutex::new(CustomCounterInner {
                        increment: Increment::Custom(None),
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
                        selector: Some(Arc::new(SubgraphSelector::Cache {
                            cache: CacheKind::Hit,
                            entity_type: EntityType::default(),
                        })),
                        selectors,
                        incremented: false,
                    }),
                }
            }),
            cache_miss: value.cache.attributes.cache_miss.is_enabled().then(|| {
                let mut nb_attributes = 0;
                let selectors = match &value.cache.attributes.cache_miss {
                    DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => {
                        None
                    }
                    DefaultedStandardInstrument::Extendable { attributes } => {
                        nb_attributes = attributes.custom.len();
                        Some(attributes.clone())
                    }
                };
                CustomCounter {
                    inner: Mutex::new(CustomCounterInner {
                        increment: Increment::Unit,
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
                        selector: Some(Arc::new(SubgraphSelector::Cache {
                            cache: CacheKind::Miss,
                            entity_type: EntityType::default(),
                        })),
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
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_request(request);
        }
        if let Some(field_execution) = &self.cache_miss {
            field_execution.on_request(request);
        }
        // self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        let subgraph_name = match &response.subgraph_name {
            Some(subgraph_name) => subgraph_name,
            None => {
                return;
            }
        };
        let cache_info: CacheSubgraph = match response.context.get(subgraph_name).ok().flatten() {
            Some(cache_info) => cache_info,
            None => {
                return;
            }
        };

        dbg!(&cache_info);

        if let Some(cache_hit) = &self.cache_hit {
            // Clone but set incremented to true on the current one to make sure it's not incremented again
            for (entity_type, CacheHitMiss { hit, .. }) in &cache_info.0 {
                let cache_hit = cache_hit.clone();
                {
                    let mut inner_cache_hit = cache_hit.inner.lock();
                    inner_cache_hit.selector = Some(Arc::new(SubgraphSelector::StaticField {
                        r#static: AttributeValue::I64(*hit as i64),
                    }));
                    if inner_cache_hit
                        .selectors
                        .as_ref()
                        .map(|s| s.attributes.entity_type == Some(true))
                        .unwrap_or_default()
                    {
                        inner_cache_hit.attributes.push(KeyValue::new(
                            Key::from_static_str("entity.type"),
                            opentelemetry::Value::String(entity_type.to_string().into()),
                        ));
                    }
                    if inner_cache_hit
                        .selectors
                        .as_ref()
                        .map(|s| s.attributes.hit == Some(true))
                        .unwrap_or_default()
                    {
                        inner_cache_hit.attributes.push(KeyValue::new(
                            Key::from_static_str("cache.hit"),
                            opentelemetry::Value::Bool(true),
                        ));
                    }
                }
                cache_hit.on_response(response);
            }
        }
        if let Some(field_execution) = &self.cache_miss {
            field_execution.on_response(response);
        }
        // self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &crate::Context) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_error(error, ctx);
        }
        if let Some(field_execution) = &self.cache_miss {
            field_execution.on_error(error, ctx);
        }
        // self.custom.on_error(error, ctx);
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

// #[cfg(test)]
// pub(crate) mod test {

//     use super::*;
//     use crate::metrics::FutureMetricsExt;
//     use crate::plugins::telemetry::Telemetry;
//     use crate::plugins::test::PluginTestHarness;
//     use crate::Configuration;

//     #[test_log::test(tokio::test)]
//     async fn basic_metric_publishing() {
//         async {
//             let schema_str = include_str!(
//                 "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
//             );
//             let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_named_query.graphql");

//             let request = supergraph::Request::fake_builder()
//                 .query(query_str)
//                 .context(context(schema_str, query_str))
//                 .build()
//                 .unwrap();

//             let harness = PluginTestHarness::<Telemetry>::builder()
//                 .config(include_str!("fixtures/field_length_enabled.router.yaml"))
//                 .schema(schema_str)
//                 .build()
//                 .await;

//             harness
//                 .call_supergraph(request, |req| {
//                     let response: serde_json::Value = serde_json::from_str(include_str!(
//                         "../../../demand_control/cost_calculator/fixtures/federated_ships_named_response.json"
//                     ))
//                     .unwrap();
//                     supergraph::Response::builder()
//                         .data(response["data"].clone())
//                         .context(req.context)
//                         .build()
//                         .unwrap()
//                 })
//                 .await
//                 .unwrap();

//             assert_histogram_sum!(
//                 "graphql.field.list.length",
//                 2.0,
//                 "graphql.field.name" = "users",
//                 "graphql.field.type" = "User",
//                 "graphql.type.name" = "Query"
//             );
//         }
//         .with_metrics()
//         .await;
//     }

//     #[test_log::test(tokio::test)]
//     async fn multiple_fields_metric_publishing() {
//         async {
//             let schema_str = include_str!(
//                 "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
//             );
//             let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_query.graphql");

//             let request = supergraph::Request::fake_builder()
//                 .query(query_str)
//                 .context(context(schema_str, query_str))
//                 .build()
//                 .unwrap();

//             let harness = PluginTestHarness::<Telemetry>::builder()
//                 .config(include_str!("fixtures/field_length_enabled.router.yaml"))
//                 .schema(schema_str)
//                 .build()
//                 .await;

//             harness
//                 .call_supergraph(request, |req| {
//                     let response: serde_json::Value = serde_json::from_str(include_str!(
//                         "../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_response.json"
//                     ))
//                     .unwrap();
//                     supergraph::Response::builder()
//                         .data(response["data"].clone())
//                         .context(req.context)
//                         .build()
//                         .unwrap()
//                 })
//                 .await
//                 .unwrap();

//             assert_histogram_sum!(
//                 "graphql.field.list.length",
//                 2.0,
//                 "graphql.field.name" = "ships",
//                 "graphql.field.type" = "Ship",
//                 "graphql.type.name" = "Query"
//             );
//             assert_histogram_sum!(
//                 "graphql.field.list.length",
//                 2.0,
//                 "graphql.field.name" = "users",
//                 "graphql.field.type" = "User",
//                 "graphql.type.name" = "Query"
//             );
//         }
//         .with_metrics()
//         .await;
//     }

//     #[test_log::test(tokio::test)]
//     async fn disabled_metric_publishing() {
//         async {
//             let schema_str = include_str!(
//                 "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
//             );
//             let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_named_query.graphql");

//             let request = supergraph::Request::fake_builder()
//                 .query(query_str)
//                 .context(context(schema_str, query_str))
//                 .build()
//                 .unwrap();

//             let harness = PluginTestHarness::<Telemetry>::builder()
//                 .config(include_str!("fixtures/field_length_disabled.router.yaml"))
//                 .schema(schema_str)
//                 .build()
//                 .await;

//             harness
//                 .call_supergraph(request, |req| {
//                     let response: serde_json::Value = serde_json::from_str(include_str!(
//                         "../../../demand_control/cost_calculator/fixtures/federated_ships_named_response.json"
//                     ))
//                     .unwrap();
//                     supergraph::Response::builder()
//                         .data(response["data"].clone())
//                         .context(req.context)
//                         .build()
//                         .unwrap()
//                 })
//                 .await
//                 .unwrap();

//             assert_histogram_not_exists!("graphql.field.list.length", f64);
//         }
//         .with_metrics()
//         .await;
//     }

//     #[test_log::test(tokio::test)]
//     async fn filtered_metric_publishing() {
//         async {
//             let schema_str = include_str!(
//                 "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
//             );
//             let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_query.graphql");

//             let request = supergraph::Request::fake_builder()
//                 .query(query_str)
//                 .context(context(schema_str, query_str))
//                 .build()
//                 .unwrap();

//             let harness = PluginTestHarness::<Telemetry>::builder()
//                 .config(include_str!("fixtures/filtered_field_length.router.yaml"))
//                 .schema(schema_str)
//                 .build()
//                 .await;

//             harness
//                 .call_supergraph(request, |req| {
//                     let response: serde_json::Value = serde_json::from_str(include_str!(
//                         "../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_response.json"
//                     ))
//                     .unwrap();
//                     supergraph::Response::builder()
//                         .data(response["data"].clone())
//                         .context(req.context)
//                         .build()
//                         .unwrap()
//                 })
//                 .await
//                 .unwrap();

//             assert_histogram_sum!("ships.list.length", 2.0);
//         }
//         .with_metrics()
//         .await;
//     }

//     fn context(schema_str: &str, query_str: &str) -> Context {
//         let schema = crate::spec::Schema::parse_test(schema_str, &Default::default()).unwrap();
//         let query =
//             crate::spec::Query::parse_document(query_str, None, &schema, &Configuration::default())
//                 .unwrap();
//         let context = Context::new();
//         context
//             .extensions()
//             .with_lock(|mut lock| lock.insert(query));

//         context
//     }
// }
