use std::sync::Arc;

use attributes::CacheAttributes;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::instruments::CustomCounter;
use super::instruments::CustomCounterInner;
use super::instruments::Increment;
use super::instruments::InstrumentsConfig;
use super::instruments::METER_NAME;
use super::selectors::CacheKind;
use super::selectors::SubgraphSelector;
use crate::metrics;
use crate::plugins::cache::entity::CacheHitMiss;
use crate::plugins::cache::entity::CacheSubgraph;
use crate::plugins::cache::entity::CACHE_INFO_SUBGRAPH_CONTEXT_KEY;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::subgraph;

pub(crate) mod attributes;

static CACHE_METRIC: &str = "apollo.router.operations.entity.cache";

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CacheInstrumentsConfig {
    /// A counter of times we have a cache hit or cache miss
    #[serde(rename = "apollo.router.operations.entity.cache")]
    pub(crate) cache: DefaultedStandardInstrument<Extendable<CacheAttributes, SubgraphSelector>>,
}

impl DefaultForLevel for CacheInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        if self.cache.is_enabled() {
            self.cache.defaults_for_level(requirement_level, kind);
        }
    }
}

pub(crate) struct CacheInstruments {
    pub(crate) cache_hit: Option<
        CustomCounter<subgraph::Request, subgraph::Response, CacheAttributes, SubgraphSelector>,
    >,
}

impl From<&InstrumentsConfig> for CacheInstruments {
    fn from(value: &InstrumentsConfig) -> Self {
        let meter = metrics::meter_provider().meter(METER_NAME);
        CacheInstruments {
            cache_hit: value.cache.attributes.cache.is_enabled().then(|| {
                let mut nb_attributes = 0;
                let selectors = match &value.cache.attributes.cache {
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
                                .f64_counter(CACHE_METRIC)
                                .with_description(
                                    "Entity cache hit/miss operations at the subgraph level",
                                )
                                .init(),
                        ),
                        attributes: Vec::with_capacity(nb_attributes),
                        selector: Some(Arc::new(SubgraphSelector::Cache {
                            cache: CacheKind::Hit,
                            entity_type: None,
                        })),
                        selectors,
                        incremented: false,
                    }),
                }
            }),
        }
    }
}

impl Instrumented for CacheInstruments {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(cache_hit) = &self.cache_hit {
            cache_hit.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        let subgraph_name = match &response.subgraph_name {
            Some(subgraph_name) => subgraph_name,
            None => {
                return;
            }
        };
        let cache_info: CacheSubgraph = match dbg!(response
            .context
            .get(&format!(
                "{CACHE_INFO_SUBGRAPH_CONTEXT_KEY}_{subgraph_name}"
            ))
            .ok()
            .flatten())
        {
            Some(cache_info) => cache_info,
            None => {
                return;
            }
        };

        if let Some(cache_hit) = &self.cache_hit {
            for (entity_type, CacheHitMiss { hit, miss }) in &cache_info.0 {
                // Cache hit
                {
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
                        inner_cache_hit.attributes.push(KeyValue::new(
                            Key::from_static_str("cache.hit"),
                            opentelemetry::Value::Bool(true),
                        ));
                    }
                    cache_hit.on_response(response);
                }
                // Cache miss
                {
                    let cache_miss = cache_hit.clone();
                    {
                        let mut inner_cache_miss = cache_miss.inner.lock();
                        inner_cache_miss.selector = Some(Arc::new(SubgraphSelector::StaticField {
                            r#static: AttributeValue::I64(*miss as i64),
                        }));
                        if inner_cache_miss
                            .selectors
                            .as_ref()
                            .map(|s| s.attributes.entity_type == Some(true))
                            .unwrap_or_default()
                        {
                            inner_cache_miss.attributes.push(KeyValue::new(
                                Key::from_static_str("entity.type"),
                                opentelemetry::Value::String(entity_type.to_string().into()),
                            ));
                        }
                        inner_cache_miss.attributes.push(KeyValue::new(
                            Key::from_static_str("cache.hit"),
                            opentelemetry::Value::Bool(false),
                        ));
                    }
                    cache_miss.on_response(response);
                }
            }
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &crate::Context) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_error(error, ctx);
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
