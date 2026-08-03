use std::sync::Arc;

use attributes::CacheAttributes;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
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
pub(crate) const RESPONSE_CACHE_METRIC: &str = "apollo.router.response.cache";
const ENTITY_TYPE: Key = Key::from_static_str("graphql.type.name");
const CACHE_HIT: Key = Key::from_static_str("cache.hit");

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CacheInstrumentsConfig {
    /// A counter of times we have a cache hit or cache miss (deprecated)
    #[serde(rename = "apollo.router.operations.entity.cache")]
    pub(crate) cache: DefaultedStandardInstrument<Extendable<CacheAttributes, SubgraphSelector>>,
    /// A counter of times we have a cache hit or cache miss
    #[serde(rename = "apollo.router.response.cache")]
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

/// Cache instruments for connector services.
///
/// Unlike `CacheInstruments` which is typed on `subgraph::Request/Response` and uses the
/// `CustomCounter` generic machinery, this struct directly holds an OTel counter and reads
/// cache hit/miss data from the request context. This avoids needing `Selector`/`Selectors`
/// trait impls for connector request/response types.
pub(crate) struct ConnectorCacheInstruments {
    counter: Option<Counter<f64>>,
    source_name: String,
}

impl ConnectorCacheInstruments {
    pub(crate) fn new(counter: Option<Counter<f64>>, source_name: String) -> Self {
        Self {
            counter,
            source_name,
        }
    }

    /// Read cache hit/miss data from context and record metrics.
    /// Call this after the connector service response is available.
    pub(crate) fn on_response(&self, context: &crate::Context) {
        let Some(counter) = &self.counter else {
            return;
        };

        let cache_info: ResponseCacheSubgraph = match context
            .get(ResponseCacheMetricContextKey::new(self.source_name.clone()))
            .ok()
            .flatten()
        {
            Some(cache_info) => cache_info,
            None => {
                return;
            }
        };

        for (entity_type, ResponseCacheHitMiss { hit, miss }) in &cache_info.0 {
            if *hit > 0 {
                counter.add(
                    *hit as f64,
                    &[
                        KeyValue::new(ENTITY_TYPE, entity_type.to_string()),
                        KeyValue::new(CACHE_HIT, true),
                    ],
                );
            }
            if *miss > 0 {
                counter.add(
                    *miss as f64,
                    &[
                        KeyValue::new(ENTITY_TYPE, entity_type.to_string()),
                        KeyValue::new(CACHE_HIT, false),
                    ],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::Context;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::config_new::instruments::InstrumentsConfig;

    fn config_with_response_cache_instrument() -> InstrumentsConfig {
        serde_json::from_value(serde_json::json!({
            "cache": {
                "apollo.router.response.cache": {
                    "attributes": { "graphql.type.name": true }
                }
            }
        }))
        .expect("config should parse")
    }

    /// The connector hit/miss path of the `apollo.router.response.cache` instrument: given the
    /// per-source hit/miss context entry the caching layer writes, the configured counter must
    /// emit with `graphql.type.name` and `cache.hit` attributes. This is the config→counter→emit
    /// seam; the caching layer's context writes are covered by the response_cache integration
    /// tests, and the end-to-end pipeline wiring by black-box testing.
    #[tokio::test]
    async fn connector_response_cache_instrument_emits_hits_and_misses() {
        async {
            let config = config_with_response_cache_instrument();
            let static_instruments = Arc::new(config.new_builtin_cache_instruments());
            let source_name = "connectors.api".to_string();

            // Miss
            let context = Context::new();
            let mut hit_miss = HashMap::new();
            hit_miss.insert(
                "Query".to_string(),
                ResponseCacheHitMiss { hit: 0, miss: 1 },
            );
            let _ = context.insert(
                ResponseCacheMetricContextKey::new(source_name.clone()),
                ResponseCacheSubgraph(hit_miss),
            );
            config
                .new_connector_cache_instruments(static_instruments.clone(), source_name.clone())
                .on_response(&context);

            assert_counter!(
                "apollo.router.response.cache",
                1.0,
                "graphql.type.name" = "Query",
                "cache.hit" = false
            );

            // Hit (fresh context, as in a second request)
            let context = Context::new();
            let mut hit_miss = HashMap::new();
            hit_miss.insert(
                "Query".to_string(),
                ResponseCacheHitMiss { hit: 1, miss: 0 },
            );
            let _ = context.insert(
                ResponseCacheMetricContextKey::new(source_name.clone()),
                ResponseCacheSubgraph(hit_miss),
            );
            config
                .new_connector_cache_instruments(static_instruments.clone(), source_name.clone())
                .on_response(&context);

            assert_counter!(
                "apollo.router.response.cache",
                1.0,
                "graphql.type.name" = "Query",
                "cache.hit" = true
            );
        }
        .with_metrics()
        .await;
    }

    /// Entity-batch shape: hits and misses for the same type accumulate into the two
    /// attribute-distinguished series.
    #[tokio::test]
    async fn connector_response_cache_instrument_partial_hit() {
        async {
            let config = config_with_response_cache_instrument();
            let static_instruments = Arc::new(config.new_builtin_cache_instruments());
            let source_name = "connectors.api".to_string();

            let context = Context::new();
            let mut hit_miss = HashMap::new();
            hit_miss.insert(
                "Product".to_string(),
                ResponseCacheHitMiss { hit: 2, miss: 1 },
            );
            let _ = context.insert(
                ResponseCacheMetricContextKey::new(source_name.clone()),
                ResponseCacheSubgraph(hit_miss),
            );
            config
                .new_connector_cache_instruments(static_instruments.clone(), source_name)
                .on_response(&context);

            assert_counter!(
                "apollo.router.response.cache",
                2.0,
                "graphql.type.name" = "Product",
                "cache.hit" = true
            );
            assert_counter!(
                "apollo.router.response.cache",
                1.0,
                "graphql.type.name" = "Product",
                "cache.hit" = false
            );
        }
        .with_metrics()
        .await;
    }

    /// The instrument must NOT emit when it isn't configured (counter absent).
    #[tokio::test]
    async fn connector_response_cache_instrument_disabled_by_default() {
        async {
            let config: InstrumentsConfig =
                serde_json::from_value(serde_json::json!({})).expect("config should parse");
            let static_instruments = Arc::new(config.new_builtin_cache_instruments());
            let context = Context::new();
            let mut hit_miss = HashMap::new();
            hit_miss.insert("Query".to_string(), ResponseCacheHitMiss { hit: 1, miss: 0 });
            let _ = context.insert(
                ResponseCacheMetricContextKey::new("connectors.api".to_string()),
                ResponseCacheSubgraph(hit_miss),
            );
            config
                .new_connector_cache_instruments(static_instruments, "connectors.api".to_string())
                .on_response(&context);

            assert!(
                crate::metrics::collect_metrics()
                    .find("apollo.router.response.cache")
                    .is_none(),
                "unconfigured instrument must not emit"
            );
        }
        .with_metrics()
        .await;
    }
}
