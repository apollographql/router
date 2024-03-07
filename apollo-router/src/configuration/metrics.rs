use std::collections::HashMap;
use std::str::FromStr;

use jsonpath_rust::JsonPathInst;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::metrics::Meter;
use opentelemetry_api::KeyValue;
use paste::paste;
use serde_json::Value;

use crate::metrics::meter_provider;
use crate::uplink::license_enforcement::LicenseState;
use crate::Configuration;

type InstrumentMap = HashMap<String, (u64, HashMap<String, opentelemetry::Value>)>;
pub(crate) struct Metrics {
    _instruments: Vec<opentelemetry::metrics::ObservableGauge<u64>>,
}

struct InstrumentData {
    data: InstrumentMap,
    meter: Meter,
}

impl Default for InstrumentData {
    fn default() -> Self {
        InstrumentData {
            meter: meter_provider().meter("apollo/router"),
            data: Default::default(),
        }
    }
}

impl Metrics {
    pub(crate) fn new(configuration: &Configuration, license_state: &LicenseState) -> Metrics {
        let mut data = InstrumentData::default();

        // Env variables and unit tests don't mix.
        data.populate_env_instrument();
        data.populate_config_instruments(
            configuration
                .validated_yaml
                .as_ref()
                .unwrap_or(&serde_json::Value::Null),
        );
        data.populate_license_instrument(license_state);

        data.into()
    }
}

impl InstrumentData {
    fn get_value_from_path(
        attributes: &mut HashMap<String, opentelemetry::Value>,
        attr_name: &str,
        path: &str,
        value: &Value,
    ) {
        let attr_name = attr_name.to_string();
        match JsonPathInst::from_str(path)
            .expect("json path must be valid")
            .find_slice(value)
            .into_iter()
            .next()
            .as_deref()
        {
            // If the value is an object we can only state that it is set, but not what it is set to.
            Some(Value::Object(_value)) => {
                attributes.insert(attr_name, true.into());
            }
            Some(Value::Array(value)) if !value.is_empty() => {
                attributes.insert(attr_name, true.into());
            }
            // Scalars can be logged as is.
            Some(Value::Number(value)) if value.is_f64() => {
                attributes.insert(attr_name, value.as_f64().expect("checked, qed").into());
            }
            Some(Value::Number(value)) if value.is_i64() => {
                attributes.insert(attr_name, value.as_i64().expect("checked, qed").into());
            }
            // Note that we convert u64 to i64 because opentelemetry does not support u64 as an attribute.
            Some(Value::Number(value)) => {
                attributes.insert(
                    attr_name,
                    (value.as_u64().expect("checked, qed") as i64).into(),
                );
            }
            Some(Value::String(value)) => {
                attributes.insert(attr_name, value.clone().into());
            }
            Some(Value::Bool(value)) => {
                attributes.insert(attr_name, (*value).into());
            }

            // If the value is not set we don't specify the attribute.
            None => {
                attributes.insert(attr_name, false.into());
            }

            _ => {}
        };
    }

    pub(crate) fn populate_config_instruments(&mut self, yaml: &serde_json::Value) {
        // This macro will query the config json for a primary metric and optionally metric attributes.

        // The reason we use jsonpath_rust is that jsonpath_lib has correctness issues and looks abandoned.
        // We should consider converting the rest of the codebase to use jsonpath_rust.

        // Example usage:
        // populate_usage_instrument!(
        //             value.apollo.router.config.authorization, // The metric name
        //             "$.authorization", // The path into the config
        //             opt.require_authentication, // The name of the attribute
        //             "$[?(@.require_authentication == true)]" // The path for the attribute relative to the metric
        //         );

        macro_rules! populate_config_instrument {
            ($($metric:ident).+, $path:literal) => {
                let instrument_name = stringify!($($metric).+).to_string();
                self.data.entry(instrument_name.clone()).or_insert_with(|| {
                    if JsonPathInst::from_str($path).expect("json path must be valid").find_slice(yaml).first().is_some() {
                        (1, HashMap::new())
                    }
                    else {
                        (0, HashMap::new())
                    }
                });
            };
            ($($metric:ident).+, $path:literal, $($($attr:ident).+, $attr_path:literal),+) => {
                let instrument_name = stringify!($($metric).+).to_string();
                self.data.entry(instrument_name).or_insert_with(|| {
                    if let Some(value) = JsonPathInst::from_str($path).expect("json path must be valid").find_slice(yaml).first() {
                        paste!{
                            let mut attributes = HashMap::new();
                            $(
                            let attr_name = stringify!([<$($attr __ )+>]).to_string();
                            Self::get_value_from_path(&mut attributes, &attr_name, $attr_path, value);)+
                            (1, attributes)
                        }
                    }
                    else {
                        paste!{
                            let mut attributes = HashMap::new();
                            $(
                                let attr_name = stringify!([<$($attr __ )+>]).to_string();
                                attributes.insert(attr_name, false.into());
                            )+
                            (0, attributes)
                        }
                    }
                });

            };
        }

        populate_config_instrument!(
            apollo.router.config.defer,
            "$.supergraph[?(@.defer_support == true)]"
        );
        populate_config_instrument!(
            apollo.router.config.authentication.jwt,
            "$.authentication[?(@..jwt)]"
        );
        populate_config_instrument!(
            apollo.router.config.authentication.aws.sigv4,
            "$.authentication[?(@.subgraph..aws_sig_v4)]"
        );
        populate_config_instrument!(
            apollo.router.config.authorization,
            "$.authorization",
            opt.require_authentication,
            "$[?(@.require_authentication == true)]",
            opt.directives,
            "$.directives[?(@.enabled == true)]"
        );
        populate_config_instrument!(
            apollo.router.config.coprocessor,
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
        populate_config_instrument!(
            apollo.router.config.persisted_queries,
            "$.persisted_queries[?(@.enabled == true)]",
            opt.log_unknown,
            "$[?(@.log_unknown == true)]",
            opt.safelist.require_id,
            "$[?(@.safelist.require_id == true)]",
            opt.safelist.enabled,
            "$[?(@.safelist.enabled == true)]"
        );

        populate_config_instrument!(
            apollo.router.config.subscriptions,
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

        populate_config_instrument!(
            apollo.router.config.limits,
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
            "$[?(@.http_max_request_bytes)]"
        );
        populate_config_instrument!(
            apollo.router.config.apq,
            "$.apq[?(@.enabled==true)]",
            opt.router.cache.redis,
            "$.router.cache.redis",
            opt.router.cache.in_memory,
            "$.router.cache.in_memory",
            opt.subgraph,
            "$.subgraph..enabled[?(@ == true)]"
        );
        populate_config_instrument!(
            apollo.router.config.tls,
            "$.tls",
            opt.router.tls.server,
            "$.supergraph",
            opt.router.tls.subgraph.ca_override,
            "$[?(@.subgraph..certificate_authorities)]",
            opt.router.tls.subgraph.client_authentication,
            "$.subgraph..client_authentication"
        );
        populate_config_instrument!(
            apollo.router.config.traffic_shaping,
            "$.traffic_shaping",
            opt.router.timeout,
            "$$[?(@.router.timeout)]",
            opt.router.rate_limit,
            "$.router.global_rate_limit",
            opt.subgraph.timeout,
            "$[?(@.all.timeout || @.subgraphs..timeout)]",
            opt.subgraph.rate_limit,
            "$[?(@.all.global_rate_limit || @.subgraphs..global_rate_limit)]",
            opt.subgraph.http2,
            "$[?(@.all.experimental_http2 == 'enable' || @.all.experimental_http2 == 'http2only' || @.subgraphs..experimental_http2 == 'enable' || @.subgraphs..experimental_http2 == 'http2only')]",
            opt.subgraph.compression,
            "$[?(@.all.compression || @.subgraphs..compression)]",
            opt.subgraph.deduplicate_query,
            "$[?(@.all.deduplicate_query == true || @.subgraphs..deduplicate_query == true)]",
            opt.subgraph.retry,
            "$[?(@.all.experimental_retry || @.subgraphs..experimental_retry)]"
        );

        populate_config_instrument!(
            apollo.router.config.entity_cache,
            "$.preview_entity_cache",
            opt.enabled,
            "$[?(@.enabled)]",
            opt.subgraph.enabled,
            "$[?(@.subgraphs..enabled)]",
            opt.subgraph.ttl,
            "$[?(@.subgraphs..ttl)]"
        );
        populate_config_instrument!(
            apollo.router.config.telemetry,
            "$..telemetry[?(@..endpoint || @.metrics.prometheus.enabled == true)]",
            opt.metrics.otlp,
            "$..metrics.otlp[?(@.endpoint)]",
            opt.metrics.prometheus,
            "$..metrics.prometheus[?(@.enabled==true)]",
            opt.tracing.otlp,
            "$..tracing.otlp[?(@.enabled==true)]",
            opt.tracing.datadog,
            "$..tracing.datadog[?(@.enabled==true)]",
            opt.tracing.jaeger,
            "$..tracing.jaeger[?(@.enabled==true)]",
            opt.tracing.zipkin,
            "$..tracing.zipkin[?(@.enabled==true)]",
            opt.events,
            "$..events",
            opt.instruments,
            "$..instruments",
            opt.spans,
            "$..spans",
            opt.spans.mode,
            "$..spans.mode",
            opt.spans.default_attribute_requirement_level,
            "$..spans.default_attribute_requirement_level",
            opt.spans.router,
            "$..spans.router",
            opt.spans.subgraph,
            "$..spans.subgraph",
            opt.spans.supergraph,
            "$..spans.supergraph",
            opt.logging.experimental_when_header,
            "$..logging.experimental_when_header"
        );

        populate_config_instrument!(
            apollo.router.config.batching,
            "$.experimental_batching[?(@.enabled == true)]",
            opt.mode,
            "$.mode"
        );

        populate_config_instrument!(
            apollo.router.config.file_uploads.multipart,
            "$.preview_file_uploads[?(@.enabled == true)].protocols.multipart[?(@.enabled == true)]",
            opt.limits.max_file_size,
            "$.limits.max_file_size",
            opt.limits.max_files,
            "$.limits.max_files"
        );
    }

    fn populate_env_instrument(&mut self) {
        #[cfg(not(test))]
        fn env_var_exists(env_name: &str) -> opentelemetry::Value {
            std::env::var(env_name)
                .map(|_| true)
                .unwrap_or(false)
                .into()
        }
        #[cfg(test)]
        fn env_var_exists(_env_name: &str) -> opentelemetry::Value {
            true.into()
        }

        let mut attributes = HashMap::new();
        attributes.insert("opt.apollo.key".to_string(), env_var_exists("APOLLO_KEY"));
        attributes.insert(
            "opt.apollo.graph_ref".to_string(),
            env_var_exists("APOLLO_GRAPH_REF"),
        );
        attributes.insert(
            "opt.apollo.license".to_string(),
            env_var_exists("APOLLO_ROUTER_LICENSE"),
        );
        attributes.insert(
            "opt.apollo.license.path".to_string(),
            env_var_exists("APOLLO_ROUTER_LICENSE_PATH"),
        );
        attributes.insert(
            "opt.apollo.supergraph.urls".to_string(),
            env_var_exists("APOLLO_ROUTER_SUPERGRAPH_URLS"),
        );
        attributes.insert(
            "opt.apollo.supergraph.path".to_string(),
            env_var_exists("APOLLO_ROUTER_SUPERGRAPH_PATH"),
        );

        attributes.insert(
            "opt.apollo.dev".to_string(),
            env_var_exists("APOLLO_ROUTER_DEV_ENV"),
        );

        self.data
            .insert("apollo.router.config.env".to_string(), (1, attributes));
    }

    pub(crate) fn populate_license_instrument(&mut self, license_state: &LicenseState) {
        self.data.insert(
            "apollo.router.lifecycle.license".to_string(),
            (
                1,
                [(
                    "license.state".to_string(),
                    license_state.to_string().into(),
                )]
                .into(),
            ),
        );
    }
}
impl From<InstrumentData> for Metrics {
    fn from(data: InstrumentData) -> Self {
        Metrics {
            _instruments: data
                .data
                .into_iter()
                .map(|(metric_name, (value, attributes))| {
                    let attributes: Vec<_> = attributes
                        .into_iter()
                        .map(|(k, v)| KeyValue::new(k.trim_end_matches("__").replace("__", "."), v))
                        .collect();
                    data.meter
                        .u64_observable_gauge(metric_name)
                        .with_callback(move |observer| {
                            observer.observe(value, &attributes);
                        })
                        .init()
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod test {
    use rust_embed::RustEmbed;

    use crate::configuration::metrics::InstrumentData;
    use crate::configuration::metrics::Metrics;
    use crate::uplink::license_enforcement::LicenseState;

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

            let mut data = InstrumentData::default();
            data.populate_config_instruments(yaml);
            let _metrics: Metrics = data.into();
            assert_non_zero_metrics_snapshot!(file_name);
        }
    }

    #[test]
    fn test_env_metrics() {
        let mut data = InstrumentData::default();
        data.populate_env_instrument();
        let _metrics: Metrics = data.into();
        assert_non_zero_metrics_snapshot!();
    }

    #[test]
    fn test_license_warn() {
        let mut data = InstrumentData::default();
        data.populate_license_instrument(&LicenseState::LicensedWarn);
        let _metrics: Metrics = data.into();
        assert_non_zero_metrics_snapshot!();
    }

    #[test]
    fn test_license_halt() {
        let mut data = InstrumentData::default();
        data.populate_license_instrument(&LicenseState::LicensedHalt);
        let _metrics: Metrics = data.into();
        assert_non_zero_metrics_snapshot!();
    }
}
