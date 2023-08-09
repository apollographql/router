use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use jsonpath_rust::JsonPathInst;
use paste::paste;
use serde_json::Value;
use tokio::sync::OwnedSemaphorePermit;

use crate::Configuration;

pub(crate) struct MetricsHandle {
    _guard: OwnedSemaphorePermit,
}

pub(crate) struct Metrics {
    yaml: Value,
    metrics: HashMap<String, (u64, HashMap<String, String>)>,
}

impl Metrics {
    /// Spawn a task that will log configuration usage metrics every second.
    /// This task has to run more frequently than that of the apollo otlp exporter otherwise the gauges will flap.
    /// Dropping the MetricsHandle stops the task.  
    pub(crate) async fn spawn(configuration: &Configuration) -> MetricsHandle {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let guard = semaphore.clone().acquire_owned().await.unwrap();
        let yaml = configuration
            .validated_yaml
            .as_ref()
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        tokio::task::spawn(async move {
            let mut metrics = Metrics {
                yaml,
                metrics: HashMap::new(),
            };
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        metrics.log_usage_metrics();
                    }
                    _ = semaphore.acquire() => {
                        // The semaphore was dropped so we can stop logging this config. The next config (if any) will take over.
                        break;
                    }

                }
            }
        });

        MetricsHandle { _guard: guard }
    }
}

impl Metrics {
    pub(crate) fn log_usage_metrics(&mut self) {
        // We have to have a macro here because tracing requires it. However, we also need to cache the metrics as json path is slow.
        // This macro will query the config json for a primary metric and optionally metric attributes.
        // The results will be cached for the next iteration.

        // The reason we use jsonpath_rust is that jsonpath_lib has correctness issues and looks abandoned.
        // We should consider converting the rest of the codebase to use jsonpath_rust.
        // The only issue is that jsonpath_rust's API takes ownership of the json Value. It has lower level APIs that don't but for some reason they don't get exposed.

        // Example usage:
        // log_usage_metrics!(
        //             value.apollo.router.config.authorization, // The metric name
        //             "$.authorization", // The path into the config
        //             opt.require_authentication, // The name of the attribute
        //             "$[?(@.require_authentication == true)]" // The path for the attribute relative to the metric
        //         );

        macro_rules! log_usage_metrics {
            ($($metric:ident).+, $path:literal) => {
                let metric_name = stringify!($($metric).+).to_string();
                let metric = self.metrics.entry(metric_name.clone()).or_insert_with(|| {
                    if JsonPathInst::from_str($path).expect("json path must be valid").find_slice(&self.yaml).first().is_some() {
                        (1, HashMap::new())
                    }
                    else {
                        (0, HashMap::new())
                    }
                });

                // Now log the metric
                tracing::info!($($metric).+ = metric.0);

            };
            ($($metric:ident).+, $path:literal, $($($attr:ident).+, $attr_path:literal),+) => {
                let metric_name = stringify!($($metric).+).to_string();
                let metric = self.metrics.entry(metric_name.clone()).or_insert_with(|| {
                    if let Some(value) = JsonPathInst::from_str($path).expect("json path must be valid").find_slice(&self.yaml).first() {
                        paste!{
                            let mut attributes = HashMap::new();
                            $(
                            let attr_name = stringify!([<$($attr __ )+>]).to_string();
                            match JsonPathInst::from_str($attr_path).expect("json path must be valid").find_slice(value).into_iter().next().as_deref() {
                                // If the value is an object we can only state that it is set, but not what it is set to.
                                Some(Value::Object(_value)) => {attributes.insert(attr_name, "true".to_string());},
                                Some(Value::Array(value)) if !value.is_empty() => {attributes.insert(attr_name, "true".to_string());},
                                // Scalars can be logged as is.
                                Some(value) => {attributes.insert(attr_name, value.to_string());},
                                // If the value is not set we don't specify the attribute.
                                None => {attributes.insert(attr_name, "false".to_string());},
                            };)+
                            (1, attributes)
                        }
                    }
                    else {
                        paste!{
                            let mut attributes = HashMap::new();
                            $(
                                let attr_name = stringify!([<$($attr __ )+>]).to_string();
                                attributes.insert(attr_name, "false".to_string());
                            )+
                            (0, attributes)
                        }
                    }
                });

                // Now log the metric
                paste!{
                    tracing::info!($($metric).+ = metric.0, $($($attr).+ = metric.1.get(stringify!([<$($attr __ )+>])).expect("attribute must be in map")),+);
                }
            };
        }

        log_usage_metrics!(
            value.apollo.router.config.defer,
            "$.supergraph[?(@.defer_support == true)]"
        );
        log_usage_metrics!(
            value.apollo.router.config.authentication.jwt,
            "$.authentication[?(@..jwt)]"
        );
        log_usage_metrics!(
            value.apollo.router.config.authentication.aws.sigv4,
            "$.authentication[?(@.subgraph..aws_sig_v4)]"
        );
        log_usage_metrics!(
            value.apollo.router.config.authorization,
            "$.authorization",
            opt.require_authentication,
            "$[?(@.require_authentication == true)]"
        );
        log_usage_metrics!(
            value.apollo.router.config.coprocessor,
            "$.coprocessor",
            opt.router.request,
            "$.router.request",
            opt.router.response,
            "$.router.response",
            // Note that supergraph is not supported yet so these will always be empty
            opt.supergraph.request,
            "$.supergraph.response",
            opt.supergraph.response,
            "$.supergraph.request",
            opt.subgraph.request,
            "$.subgraph..request",
            opt.subgraph.response,
            "$.subgraph..response"
        );
        log_usage_metrics!(
            value.apollo.router.config.persisted_queries,
            "$.preview_persisted_queries[?(@.enabled == true)]",
            opt.log_unknown,
            "$[?(@.log_unknown == true)]",
            opt.safelist.require_id,
            "$[?(@.safelist.require_id == true)]",
            opt.safelist.enabled,
            "$[?(@.safelist.enabled == true)]"
        );

        log_usage_metrics!(
            value.apollo.router.config.subscriptions,
            "$.subscription[?(@.enabled == true)]",
            opt.mode.passthrough,
            "$.mode.passthrough",
            opt.mode.callback,
            "$.mode.callback",
            opt.deduplication,
            "$[?(@.enable_deduplication == true)]",
            opt.max_opened,
            "$[?(@.max_opened_subscriptions)]",
            opt.queue_capacity,
            "$[?(@.queue_capacity)]"
        );

        log_usage_metrics!(
            value.apollo.router.config.limits,
            "$.limits",
            opt.operation.max_depth,
            "$[?(@.max_depth)]",
            opt.operation.max_aliases,
            "$[?(@.max_aliases)]",
            opt.operation.max_height,
            "$[?(@.max_height)]",
            opt.operation.max_root_fields,
            "$[?(@.max_root_fields)]",
            opt.operation.warn_only,
            "$[?(@.warn_only)]",
            opt.parser.max_recursion,
            "$[?(@.parser_max_recursion)]",
            opt.parser.max_tokens,
            "$[?(@.parser_max_tokens)]",
            opt.request.max_size,
            "$[?(@.experimental_http_max_request_bytes)]"
        );
        log_usage_metrics!(
            value.apollo.router.config.apq,
            "$.apq[?(@.enabled==true)]",
            opt.router.cache.redis,
            "$.router.cache.redis",
            opt.router.cache.in_memory,
            "$.router.cache.in_memory",
            opt.subgraph,
            "$.subgraph..enabled[?(@ == true)]"
        );
        log_usage_metrics!(
            value.apollo.router.config.traffic_shaping,
            "$.traffic_shaping",
            opt.router.timout,
            "$$[?(@.router.timeout)]",
            opt.router.rate_limit,
            "$.router.global_rate_limit",
            opt.subgraph.timeout,
            "$[?(@.all.timeout || @.subgraphs..timeout)]",
            opt.subgraph.rate_limit,
            "$[?(@.all.global_rate_limit || @.subgraphs..global_rate_limit)]",
            opt.subgraph.http2,
            "$[?(@.all.experimental_enable_http2 == true || @.subgraphs..experimental_enable_http2 == true)]",
            opt.subgraph.compression,
            "$[?(@.all.compression || @.subgraphs..compression)]",
            opt.subgraph.deduplicate_query,
            "$[?(@.all.deduplicate_query == true || @.subgraphs..deduplicate_query == true)]",
            opt.subgraph.retry,
            "$[?(@.all.experimental_retry || @.subgraphs..experimental_retry)]"
        );

        log_usage_metrics!(
            value.apollo.router.config.entities,
            "$[?(@.traffic_shaping..experimental_entity_caching)]",
            opt.cache,
            "$[?(@.traffic_shaping..experimental_entity_caching)]"
        );
        log_usage_metrics!(
            value.apollo.router.config.telemetry,
            "$.telemetry[?(@..endpoint || @.metrics.prometheus.enabled == true)]",
            opt.metrics.otlp,
            "$.metrics.otlp[?(@.endpoint)]",
            opt.metrics.prometheus,
            "$.metrics.prometheus[?(@.enabled==true)]",
            opt.tracing.otlp,
            "$.tracing.otlp[?(@.endpoint)]",
            opt.tracing.datadog,
            "$.tracing.datadog[?(@.endpoint)]",
            opt.tracing.jaeger,
            "$.tracing.jaeger[?(@..endpoint)]",
            opt.tracing.zipkin,
            "$.tracing.zipkin[?(@.endpoint)]"
        );
    }
}

#[cfg(test)]
mod test {
    use insta::assert_yaml_snapshot;
    use rust_embed::RustEmbed;

    use crate::configuration::metrics::Metrics;

    #[derive(RustEmbed)]
    #[folder = "src/configuration/testdata/metrics"]
    struct Asset;

    #[test]
    fn test_metrics() {
        for file_name in Asset::iter() {
            let source = Asset::get(&file_name).expect("test file must exist");
            let input = std::str::from_utf8(&source.data)
                .expect("expected utf8")
                .to_string();
            let yaml = &serde_yaml::from_str::<serde_json::Value>(&input)
                .expect("config must be valid yaml");

            let mut metrics = Metrics {
                yaml: yaml.clone(),
                metrics: Default::default(),
            };
            metrics.log_usage_metrics();
            metrics.metrics.retain(|_, v| v.0 > 0);
            insta::with_settings!({sort_maps => true, snapshot_suffix => file_name}, {
                assert_yaml_snapshot!(&metrics.metrics);
            });
        }
    }
}
