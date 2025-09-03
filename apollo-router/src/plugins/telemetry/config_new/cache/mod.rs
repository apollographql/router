use std::sync::Arc;

use attributes::CacheAttributes;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::instruments::CustomCounter;
use super::subgraph::selectors::SubgraphSelector;
use crate::plugins::cache::entity::CacheHitMiss;
use crate::plugins::cache::entity::CacheSubgraph;
use crate::plugins::cache::metrics::CacheMetricContextKey;
use crate::plugins::response_cache::metrics::CacheMetricContextKey as ResponseCacheMetricContextKey;
use crate::plugins::response_cache::plugin::CacheHitMiss as ResponseCacheHitMiss;
use crate::plugins::response_cache::plugin::CacheSubgraph as ResponseCacheSubgraph;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::subgraph;

pub(crate) mod attributes;

pub(crate) const CACHE_METRIC: &str = "apollo.router.operations.entity.cache";
pub(crate) const RESPONSE_CACHE_METRIC: &str = "apollo.router.operations.response.cache";
const ENTITY_TYPE: Key = Key::from_static_str("graphql.type.name");
const CACHE_HIT: Key = Key::from_static_str("cache.hit");

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CacheInstrumentsConfig {
    /// A counter of times we have a cache hit or cache miss (deprecated)
    #[serde(rename = "apollo.router.operations.entity.cache")]
    pub(crate) cache: DefaultedStandardInstrument<Extendable<CacheAttributes, SubgraphSelector>>,
    /// A counter of times we have a cache hit or cache miss
    #[serde(rename = "apollo.router.operations.response.cache")]
    pub(crate) response_cache:
        DefaultedStandardInstrument<Extendable<CacheAttributes, SubgraphSelector>>,
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
        if self.response_cache.is_enabled() {
            self.response_cache
                .defaults_for_level(requirement_level, kind);
        }
    }
}

pub(crate) struct CacheInstruments {
    pub(crate) cache_hit: Option<
        CustomCounter<subgraph::Request, subgraph::Response, (), CacheAttributes, SubgraphSelector>,
    >,
    pub(crate) cache_hit_response_cache: Option<
        CustomCounter<subgraph::Request, subgraph::Response, (), CacheAttributes, SubgraphSelector>,
    >,
}

impl Instrumented for CacheInstruments {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(cache_hit) = &self.cache_hit {
            cache_hit.on_request(request);
        }
        if let Some(cache_hit) = &self.cache_hit_response_cache {
            cache_hit.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        let subgraph_name = response.subgraph_name.clone();
        // ================ DEPRECATED ENTITY CACHE ===================

        if let Some(cache_hit) = &self.cache_hit
            && let Some(cache_info) = response
                .context
                .get::<_, CacheSubgraph>(CacheMetricContextKey::new(subgraph_name.clone()))
                .ok()
                .flatten()
        {
            for (entity_type, CacheHitMiss { hit, miss }) in &cache_info.0 {
                // Cache hit
                {
                    let cloned_cache_hit = cache_hit.clone();
                    {
                        let mut inner_cache_hit = cloned_cache_hit.inner.lock();
                        inner_cache_hit.selector = Some(Arc::new(SubgraphSelector::StaticField {
                            r#static: AttributeValue::I64(*hit as i64),
                        }));
                        if let Some(key) = inner_cache_hit
                            .selectors
                            .as_ref()
                            .and_then(|s| s.attributes.entity_type.as_ref())
                            .and_then(|a| a.key(ENTITY_TYPE))
                        {
                            inner_cache_hit.attributes.push(KeyValue::new(
                                key,
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
                        if let Some(key) = inner_cache_miss
                            .selectors
                            .as_ref()
                            .and_then(|s| s.attributes.entity_type.as_ref())
                            .and_then(|a| a.key(ENTITY_TYPE))
                        {
                            inner_cache_miss.attributes.push(KeyValue::new(
                                key,
                                opentelemetry::Value::String(entity_type.to_string().into()),
                            ));
                        }
                        inner_cache_miss
                            .attributes
                            .push(KeyValue::new(CACHE_HIT, opentelemetry::Value::Bool(false)));
                    }
                    cloned_cache_miss.on_response(response);
                }
            }
            // Make sure it won't be incremented when dropped
            let _ = cache_hit.inner.lock().counter.take();
        }
        // ===================================

        let cache_info: ResponseCacheSubgraph = match response
            .context
            .get(ResponseCacheMetricContextKey::new(subgraph_name))
            .ok()
            .flatten()
        {
            Some(cache_info) => cache_info,
            None => {
                return;
            }
        };

        if let Some(cache_hit) = &self.cache_hit_response_cache {
            for (entity_type, ResponseCacheHitMiss { hit, miss }) in &cache_info.0 {
                // Cache hit
                {
                    let cloned_cache_hit = cache_hit.clone();
                    {
                        let mut inner_cache_hit = cloned_cache_hit.inner.lock();
                        inner_cache_hit.selector = Some(Arc::new(SubgraphSelector::StaticField {
                            r#static: AttributeValue::I64(*hit as i64),
                        }));
                        if let Some(key) = inner_cache_hit
                            .selectors
                            .as_ref()
                            .and_then(|s| s.attributes.entity_type.as_ref())
                            .and_then(|a| a.key(ENTITY_TYPE))
                        {
                            inner_cache_hit.attributes.push(KeyValue::new(
                                key,
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
                        if let Some(key) = inner_cache_miss
                            .selectors
                            .as_ref()
                            .and_then(|s| s.attributes.entity_type.as_ref())
                            .and_then(|a| a.key(ENTITY_TYPE))
                        {
                            inner_cache_miss.attributes.push(KeyValue::new(
                                key,
                                opentelemetry::Value::String(entity_type.to_string().into()),
                            ));
                        }
                        inner_cache_miss
                            .attributes
                            .push(KeyValue::new(CACHE_HIT, opentelemetry::Value::Bool(false)));
                    }
                    cloned_cache_miss.on_response(response);
                }
            }
            // Make sure it won't be incremented when dropped
            let _ = cache_hit.inner.lock().counter.take();
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &crate::Context) {
        if let Some(field_length) = &self.cache_hit {
            field_length.on_error(error, ctx);
        }
        if let Some(field_length) = &self.cache_hit_response_cache {
            field_length.on_error(error, ctx);
        }
    }
}
