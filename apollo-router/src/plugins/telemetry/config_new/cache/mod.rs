use std::sync::Arc;

use attributes::CacheAttributes;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::Unit;
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
use crate::plugins::cache::metrics::CacheMetricContextKey;
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
const ENTITY_TYPE: Key = Key::from_static_str("entity.type");
const CACHE_HIT: Key = Key::from_static_str("cache.hit");

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
                                .with_unit(Unit::new("ops"))
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
        let cache_info: CacheSubgraph = match response
            .context
            .get(CacheMetricContextKey::new(subgraph_name.clone()))
            .ok()
            .flatten()
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
                    let cloned_cache_hit = cache_hit.clone();
                    {
                        let mut inner_cache_hit = cloned_cache_hit.inner.lock();
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
                                ENTITY_TYPE,
                                opentelemetry::Value::String(entity_type.to_string().into()),
                            ));
                        }
                        inner_cache_hit
                            .attributes
                            .push(KeyValue::new(CACHE_HIT, opentelemetry::Value::Bool(true)));
                    }
                    cloned_cache_hit.on_response(response);
                }
                // Cache miss
                {
                    let cloned_cache_miss = cache_hit.clone();
                    {
                        let mut inner_cache_miss = cloned_cache_miss.inner.lock();
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
                                ENTITY_TYPE,
                                opentelemetry::Value::String(entity_type.to_string().into()),
                            ));
                        }
                        inner_cache_miss
                            .attributes
                            .push(KeyValue::new(CACHE_HIT, opentelemetry::Value::Bool(false)));
                    }
                    cloned_cache_miss.on_response(response);
                }
                // Make sure it won't be incremented when dropped
            }
            let _ = cache_hit.inner.lock().counter.take();
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &crate::Context) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_error(error, ctx);
        }
    }
}
