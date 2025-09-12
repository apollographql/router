//! Apollo metrics
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry_otlp::MetricsExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::PeriodicReader;
use opentelemetry_sdk::runtime;
use prometheus::exponential_buckets;
use sys_info::hostname;
use tonic::metadata::MetadataMap;
use tonic::transport::ClientTlsConfig;
use tower::BoxError;
use url::Url;

use crate::plugins::telemetry::apollo::ApolloUsageReportsExporterConfiguration;
use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::apollo::OtlpMetricsExporterConfiguration;
use crate::plugins::telemetry::apollo::router_id;
use crate::plugins::telemetry::apollo_exporter::ApolloExporter;
use crate::plugins::telemetry::apollo_exporter::get_uname;
use crate::plugins::telemetry::config::ApolloMetricsReferenceMode;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::CustomAggregationSelector;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::otlp::CustomTemporalitySelector;
use crate::plugins::telemetry::otlp::Protocol;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::otlp::process_endpoint;

pub(crate) mod histogram;
pub(crate) mod studio;

fn default_buckets() -> Vec<f64> {
    vec![
        0.001, 0.005, 0.015, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0,
    ]
}

impl MetricsConfigurator for Config {
    fn enabled(&self) -> bool {
        self.apollo_key.is_some() && self.apollo_graph_ref.is_some()
    }

    fn apply(
        &self,
        mut builder: MetricsBuilder,
        _metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        tracing::debug!("configuring Apollo metrics");
        static ENABLED: AtomicBool = AtomicBool::new(false);
        Ok(match self {
            Config {
                endpoint,
                experimental_otlp_endpoint: otlp_endpoint,
                experimental_otlp_metrics_protocol: otlp_metrics_protocol,
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                schema_id,
                metrics,
                metrics_reference_mode,
                ..
            } => {
                if !ENABLED.swap(true, Ordering::Relaxed) {
                    tracing::info!(
                        "Apollo Studio usage reporting is enabled. See https://go.apollo.dev/o/data for details"
                    );
                }

                builder = Self::configure_apollo_metrics(
                    builder,
                    endpoint,
                    key,
                    reference,
                    schema_id,
                    &metrics.usage_reports.exporter,
                    *metrics_reference_mode,
                )?;
                // env variable EXPERIMENTAL_APOLLO_OTLP_METRICS_ENABLED will disappear without warning in future
                if std::env::var("EXPERIMENTAL_APOLLO_OTLP_METRICS_ENABLED")
                    .unwrap_or_else(|_| "true".to_string())
                    == "true"
                {
                    builder = Self::configure_apollo_otlp_metrics(
                        builder,
                        otlp_endpoint,
                        otlp_metrics_protocol,
                        key,
                        reference,
                        schema_id,
                        &metrics.otlp.exporter,
                    )?;
                }
                builder
            }
            _ => {
                ENABLED.swap(false, Ordering::Relaxed);
                builder
            }
        })
    }
}

impl Config {
    fn configure_apollo_otlp_metrics(
        mut builder: MetricsBuilder,
        endpoint: &Url,
        otlp_protocol: &Protocol,
        key: &str,
        reference: &str,
        schema_id: &str,
        exporter_config: &OtlpMetricsExporterConfiguration,
    ) -> Result<MetricsBuilder, BoxError> {
        tracing::info!("configuring Apollo OTLP metrics: {}", exporter_config);
        let mut metadata = MetadataMap::new();
        metadata.insert("apollo.api.key", key.parse()?);
        let exporter = match otlp_protocol {
            Protocol::Grpc => MetricsExporterBuilder::Tonic(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_tls_config(ClientTlsConfig::new().with_native_roots())
                    .with_endpoint(endpoint.as_str())
                    .with_timeout(exporter_config.max_export_timeout)
                    .with_metadata(metadata.clone())
                    .with_compression(opentelemetry_otlp::Compression::Gzip),
            ),
            // While Apollo doesn't use the HTTP protocol, we support it here for
            // use in tests to enable WireMock.
            Protocol::Http => {
                let maybe_endpoint = process_endpoint(
                    &Some(endpoint.to_string()),
                    &TelemetryDataKind::Metrics,
                    &Protocol::Http,
                )?;
                let mut otlp_exporter = opentelemetry_otlp::new_exporter()
                    .http()
                    .with_protocol(opentelemetry_otlp::Protocol::Grpc)
                    .with_timeout(exporter_config.max_export_timeout);
                if let Some(endpoint) = maybe_endpoint {
                    otlp_exporter = otlp_exporter.with_endpoint(endpoint);
                }
                MetricsExporterBuilder::Http(otlp_exporter)
            }
        }
        .build_metrics_exporter(
            Box::new(CustomTemporalitySelector(
                opentelemetry_sdk::metrics::data::Temporality::Delta,
            )),
            Box::new(
                CustomAggregationSelector::builder()
                    .boundaries(default_buckets())
                    .build(),
            ),
        )?;
        // MetricsExporterBuilder does not implement Clone, so we need to create a new builder for the realtime exporter
        let realtime_exporter = match otlp_protocol {
            Protocol::Grpc => MetricsExporterBuilder::Tonic(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_tls_config(ClientTlsConfig::new().with_native_roots())
                    .with_endpoint(endpoint.as_str())
                    .with_timeout(exporter_config.max_export_timeout)
                    .with_metadata(metadata.clone())
                    .with_compression(opentelemetry_otlp::Compression::Gzip),
            ),
            Protocol::Http => {
                let maybe_endpoint = process_endpoint(
                    &Some(endpoint.to_string()),
                    &TelemetryDataKind::Metrics,
                    &Protocol::Http,
                )?;
                let mut otlp_exporter = opentelemetry_otlp::new_exporter()
                    .http()
                    .with_protocol(opentelemetry_otlp::Protocol::Grpc)
                    .with_timeout(exporter_config.max_export_timeout);
                if let Some(endpoint) = maybe_endpoint {
                    otlp_exporter = otlp_exporter.with_endpoint(endpoint);
                }
                MetricsExporterBuilder::Http(otlp_exporter)
            }
        }
        .build_metrics_exporter(
            Box::new(CustomTemporalitySelector(
                opentelemetry_sdk::metrics::data::Temporality::Delta,
            )),
            // This aggregation uses the Apollo histogram format where a duration, x, in μs is
            // counted in the bucket of index max(0, min(ceil(ln(x)/ln(1.1)), 383)).
            Box::new(
                CustomAggregationSelector::builder()
                    .boundaries(
                        // Returns [~1.4ms ... ~5min]
                        exponential_buckets(0.001399084909, 1.1, 129).unwrap(),
                    )
                    .build(),
            ),
        )?;
        let default_reader = PeriodicReader::builder(exporter, runtime::Tokio)
            .with_interval(Duration::from_secs(60))
            .with_timeout(exporter_config.max_export_timeout)
            .build();

        let realtime_reader = PeriodicReader::builder(realtime_exporter, runtime::Tokio)
            .with_interval(exporter_config.scheduled_delay)
            .with_timeout(exporter_config.max_export_timeout)
            .build();

        let resource = Resource::new([
            KeyValue::new("apollo.router.id", router_id()),
            KeyValue::new("apollo.graph.ref", reference.to_string()),
            KeyValue::new("apollo.schema.id", schema_id.to_string()),
            KeyValue::new(
                "apollo.user.agent",
                format!(
                    "{}@{}",
                    std::env!("CARGO_PKG_NAME"),
                    std::env!("CARGO_PKG_VERSION")
                ),
            ),
            KeyValue::new("apollo.client.host", hostname()?),
            KeyValue::new("apollo.client.uname", get_uname()?),
        ]);

        builder.apollo_meter_provider_builder = builder
            .apollo_meter_provider_builder
            .with_reader(default_reader)
            .with_resource(resource.clone());

        builder.apollo_realtime_meter_provider_builder = builder
            .apollo_realtime_meter_provider_builder
            .with_reader(realtime_reader)
            .with_resource(resource.clone());
        Ok(builder)
    }

    fn configure_apollo_metrics(
        mut builder: MetricsBuilder,
        endpoint: &Url,
        key: &str,
        reference: &str,
        schema_id: &str,
        exporter_config: &ApolloUsageReportsExporterConfiguration,
        metrics_reference_mode: ApolloMetricsReferenceMode,
    ) -> Result<MetricsBuilder, BoxError> {
        tracing::info!(
            "configuring Apollo usage report metrics: {}",
            exporter_config
        );
        let exporter = ApolloExporter::new(
            endpoint,
            exporter_config,
            key,
            reference,
            schema_id,
            router_id(),
            metrics_reference_mode,
        )?;

        builder.apollo_metrics_sender = exporter.start();
        Ok(builder)
    }
}

#[cfg(test)]
mod test {
    use std::future::Future;
    use std::time::Duration;

    use serde_json::Value;
    use tokio_stream::StreamExt;
    use tokio_stream::wrappers::ReceiverStream;
    use tower::ServiceExt;

    use super::studio::SingleStatsReport;
    use super::*;
    use crate::Context;
    use crate::TestHarness;
    use crate::context::OPERATION_KIND;
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::plugin::PluginPrivate;
    use crate::plugins::subscription;
    use crate::plugins::telemetry::STUDIO_EXCLUDE;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::telemetry::apollo;
    use crate::plugins::telemetry::apollo::ENDPOINT_DEFAULT;
    use crate::plugins::telemetry::apollo_exporter::Sender;
    use crate::query_planner::OperationKind;
    use crate::services::SupergraphRequest;

    #[tokio::test]
    async fn apollo_metrics_disabled() -> Result<(), BoxError> {
        let config = r#"
            telemetry:
              apollo:
                endpoint: "http://example.com"
                client_name_header: "name_header"
                client_version_header: "version_header"
                buffer_size: 10000
                schema_id: "schema_sha"
            "#;
        let plugin = create_telemetry_plugin(config).await?;
        assert!(matches!(plugin.apollo_metrics_sender, Sender::Noop));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_enabled() -> Result<(), BoxError> {
        let plugin = create_default_telemetry_plugin().await?;
        assert!(matches!(plugin.apollo_metrics_sender, Sender::Apollo(_)));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_single_operation() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let results = get_metrics_for_request(query, None, None, false, None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_for_subscription() -> Result<(), BoxError> {
        let query = "subscription {userWasCreated{name}}";
        let context = Context::new();
        let _ = context
            .insert(OPERATION_KIND, OperationKind::Subscription)
            .unwrap();
        let results = get_metrics_for_request(query, None, Some(context), true, None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_for_subscription_error() -> Result<(), BoxError> {
        let query = "subscription{reviewAdded{body}}";
        let context = Context::new();
        let _ = context
            .insert(OPERATION_KIND, OperationKind::Subscription)
            .unwrap();
        let results = get_metrics_for_request(query, None, Some(context), true, None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_multiple_operations() -> Result<(), BoxError> {
        let query = "query {topProducts{name}} query {topProducts{name}}";
        let results = get_metrics_for_request(query, None, None, false, None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_parse_failure() -> Result<(), BoxError> {
        let query = "garbage";
        let results = get_metrics_for_request(query, None, None, false, None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_unknown_operation() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let results = get_metrics_for_request(query, Some("UNKNOWN"), None, false, None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| insta::assert_json_snapshot!(results));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_validation_failure() -> Result<(), BoxError> {
        let query = "query {topProducts(minStarRating: 4.7){name}}";
        let results = get_metrics_for_request(query, None, None, false, None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_exclude() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let context = Context::new();
        context.insert(STUDIO_EXCLUDE, true)?;
        let results = get_metrics_for_request(query, None, Some(context), false, None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_features_explicitly_enabled() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let plugin = create_telemetry_plugin(include_str!(
            "../../testdata/full_config_all_features_enabled.router.yaml"
        ))
        .await?;
        let results = get_metrics_for_request(query, None, None, false, Some(plugin)).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_features_explicitly_disabled() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let plugin = create_telemetry_plugin(include_str!(
            "../../testdata/full_config_all_features_explicitly_disabled.router.yaml"
        ))
        .await?;
        let results = get_metrics_for_request(query, None, None, false, Some(plugin)).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_features_disabled_when_defaulted() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let plugin = create_telemetry_plugin(include_str!(
            "../../testdata/full_config_all_features_defaults.router.yaml"
        ))
        .await?;
        let results = get_metrics_for_request(query, None, None, false, Some(plugin)).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_distributed_apq_cache_feature_enabled_with_partial_defaults()
    -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let plugin = create_telemetry_plugin(include_str!(
            "../../testdata/full_config_apq_enabled_partial_defaults.router.yaml"
        ))
        .await?;
        let results = get_metrics_for_request(query, None, None, false, Some(plugin)).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_distributed_apq_cache_feature_disabled_with_partial_defaults()
    -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let plugin = create_telemetry_plugin(include_str!(
            "../../testdata/full_config_apq_disabled_partial_defaults.router.yaml"
        ))
        .await?;
        let results = get_metrics_for_request(query, None, None, false, Some(plugin)).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    async fn get_metrics_for_request(
        query: &str,
        operation_name: Option<&str>,
        context: Option<Context>,
        is_subscription: bool,
        telemetry_plugin: Option<Telemetry>,
    ) -> Result<Vec<SingleStatsReport>, BoxError> {
        let _ = tracing_subscriber::fmt::try_init();

        let mut plugin = if let Some(p) = telemetry_plugin {
            p
        } else {
            create_default_telemetry_plugin().await?
        };
        // Replace the apollo metrics sender so we can test metrics collection.
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        plugin.apollo_metrics_sender = Sender::Apollo(tx);
        let mut request_builder = SupergraphRequest::fake_builder()
            .header("name_header", "test_client")
            .header("version_header", "1.0-test")
            .query(query)
            .and_operation_name(operation_name)
            .and_context(context);
        if is_subscription {
            request_builder =
                request_builder.header("accept", "multipart/mixed;subscriptionSpec=1.0");
        }
        TestHarness::builder()
            .extra_private_plugin(plugin)
            .extra_plugin(create_subscription_plugin().await?)
            .build_router()
            .await?
            .oneshot(request_builder.build()?.try_into().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap();

        let default_latency = Duration::from_millis(100);
        let results = ReceiverStream::new(rx)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|m| match m {
                apollo::SingleReport::Stats(mut m) => {
                    m.stats.iter_mut().for_each(|(_k, v)| {
                        v.stats_with_context.query_latency_stats.latency = default_latency
                    });
                    Some(m)
                }
                apollo::SingleReport::Traces(_) => None,
            })
            .collect();
        Ok(results)
    }

    fn create_default_telemetry_plugin() -> impl Future<Output = Result<Telemetry, BoxError>> {
        let config = format!(
            r#"
            telemetry:
              apollo:
                endpoint: "{ENDPOINT_DEFAULT}"
                apollo_key: "key"
                apollo_graph_ref: "ref"
                client_name_header: "name_header"
                client_version_header: "version_header"
                buffer_size: 10000
                schema_id: "schema_sha"
            "#
        );

        async move { create_telemetry_plugin(&config).await }
    }

    async fn create_telemetry_plugin(full_config: &str) -> Result<Telemetry, BoxError> {
        let full_config = serde_yaml::from_str::<Value>(full_config).expect("yaml must be valid");
        let telemetry_config = full_config
            .as_object()
            .expect("must be an object")
            .get("telemetry")
            .expect("telemetry must be a root key");
        let init = PluginInit::fake_builder()
            .config(telemetry_config.clone())
            .full_config(full_config)
            .build()
            .with_deserialized_config()
            .expect("unable to deserialize telemetry config");

        Telemetry::new(init).await
    }

    async fn create_subscription_plugin() -> Result<subscription::Subscription, BoxError> {
        <subscription::Subscription as Plugin>::new(PluginInit::fake_new(
            subscription::SubscriptionConfig::default(),
            Default::default(),
        ))
        .await
    }
}
