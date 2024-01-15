//! Apollo metrics
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::time::Duration;

use opentelemetry::runtime;
use opentelemetry::sdk::metrics::PeriodicReader;
use opentelemetry::sdk::Resource;
use opentelemetry_api::KeyValue;
use opentelemetry_otlp::MetricsExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use sys_info::hostname;
use tonic::metadata::MetadataMap;
use tower::BoxError;
use url::Url;
use uuid::Uuid;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::apollo_exporter::get_uname;
use crate::plugins::telemetry::apollo_exporter::ApolloExporter;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::CustomAggregationSelector;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::otlp::CustomTemporalitySelector;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;

mod duration_histogram;
pub(crate) mod studio;

fn default_buckets() -> Vec<f64> {
    vec![
        0.001, 0.005, 0.015, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0,
    ]
}

// Random unique UUID for the Router. This doesn't actually identify the router, it just allows disambiguation between multiple routers with the same metadata.
static ROUTER_ID: OnceLock<Uuid> = OnceLock::new();

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
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                schema_id,
                batch_processor,
                ..
            } => {
                if !ENABLED.swap(true, Ordering::Relaxed) {
                    tracing::info!("Apollo Studio usage reporting is enabled. See https://go.apollo.dev/o/data for details");
                }

                builder = Self::configure_apollo_metrics(
                    builder,
                    endpoint,
                    key,
                    reference,
                    schema_id,
                    batch_processor,
                )?;
                // env variable EXPERIMENTAL_APOLLO_OTLP_METRICS_ENABLED will disappear without warning in future
                if std::env::var("EXPERIMENTAL_APOLLO_OTLP_METRICS_ENABLED")
                    .unwrap_or_else(|_| "true".to_string())
                    == "true"
                {
                    builder = Self::configure_apollo_otlp_metrics(
                        builder,
                        otlp_endpoint,
                        key,
                        reference,
                        schema_id,
                        batch_processor,
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
        key: &str,
        reference: &str,
        schema_id: &str,
        batch_processor: &BatchProcessorConfig,
    ) -> Result<MetricsBuilder, BoxError> {
        tracing::debug!(endpoint = %endpoint, "creating Apollo OTLP metrics exporter");
        let mut metadata = MetadataMap::new();
        metadata.insert("apollo.api.key", key.parse()?);
        let exporter = MetricsExporterBuilder::Tonic(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(endpoint.as_str())
                .with_timeout(batch_processor.max_export_timeout)
                .with_metadata(metadata)
                .with_compression(opentelemetry_otlp::Compression::Gzip),
        )
        .build_metrics_exporter(
            Box::new(CustomTemporalitySelector(
                opentelemetry::sdk::metrics::data::Temporality::Delta,
            )),
            Box::new(
                CustomAggregationSelector::builder()
                    .boundaries(default_buckets())
                    .build(),
            ),
        )?;
        let reader = PeriodicReader::builder(exporter, runtime::Tokio)
            .with_interval(Duration::from_secs(60))
            .build();

        builder.apollo_meter_provider_builder = builder
            .apollo_meter_provider_builder
            .with_reader(reader)
            .with_resource(Resource::new([
                KeyValue::new(
                    "apollo.router.id",
                    ROUTER_ID.get_or_init(Uuid::new_v4).to_string(),
                ),
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
            ]));
        Ok(builder)
    }

    fn configure_apollo_metrics(
        mut builder: MetricsBuilder,
        endpoint: &Url,
        key: &str,
        reference: &str,
        schema_id: &str,
        batch_processor: &BatchProcessorConfig,
    ) -> Result<MetricsBuilder, BoxError> {
        let batch_processor_config = batch_processor;
        tracing::debug!(endpoint = %endpoint, "creating Apollo metrics exporter");
        let exporter =
            ApolloExporter::new(endpoint, batch_processor_config, key, reference, schema_id)?;

        builder.apollo_metrics_sender = exporter.start();
        Ok(builder)
    }
}

#[cfg(test)]
mod test {
    use std::future::Future;
    use std::time::Duration;

    use http::header::HeaderName;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_stream::StreamExt;
    use tower::ServiceExt;
    use url::Url;

    use super::super::super::config;
    use super::studio::SingleStatsReport;
    use super::*;
    use crate::context::OPERATION_KIND;
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::plugins::subscription;
    use crate::plugins::telemetry::apollo;
    use crate::plugins::telemetry::apollo::default_buffer_size;
    use crate::plugins::telemetry::apollo::ENDPOINT_DEFAULT;
    use crate::plugins::telemetry::apollo_exporter::Sender;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::telemetry::STUDIO_EXCLUDE;
    use crate::query_planner::OperationKind;
    use crate::services::SupergraphRequest;
    use crate::Context;
    use crate::TestHarness;

    #[tokio::test]
    async fn apollo_metrics_disabled() -> Result<(), BoxError> {
        let plugin = create_plugin_with_apollo_config(super::super::apollo::Config {
            endpoint: Url::parse("http://example.com")?,
            apollo_key: None,
            apollo_graph_ref: None,
            client_name_header: HeaderName::from_static("name_header"),
            client_version_header: HeaderName::from_static("version_header"),
            buffer_size: default_buffer_size(),
            schema_id: "schema_sha".to_string(),
            ..Default::default()
        })
        .await?;
        assert!(matches!(plugin.apollo_metrics_sender, Sender::Noop));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_enabled() -> Result<(), BoxError> {
        let plugin = create_plugin().await?;
        assert!(matches!(plugin.apollo_metrics_sender, Sender::Apollo(_)));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_single_operation() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let results = get_metrics_for_request(query, None, None, false).await?;
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
        let results = get_metrics_for_request(query, None, Some(context), true).await?;
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
        let results = get_metrics_for_request(query, None, Some(context), true).await?;
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
        let results = get_metrics_for_request(query, None, None, false).await?;
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
        let results = get_metrics_for_request(query, None, None, false).await?;
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
        let results = get_metrics_for_request(query, Some("UNKNOWN"), None, false).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| insta::assert_json_snapshot!(results));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_validation_failure() -> Result<(), BoxError> {
        let query = "query {topProducts(minStarRating: 4.7){name}}";
        let results = get_metrics_for_request(query, None, None, false).await?;
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
        let results = get_metrics_for_request(query, None, Some(context), false).await?;
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
    ) -> Result<Vec<SingleStatsReport>, BoxError> {
        let _ = tracing_subscriber::fmt::try_init();
        let mut plugin = create_plugin().await?;
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
            .extra_plugin(plugin)
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

    fn create_plugin() -> impl Future<Output = Result<Telemetry, BoxError>> {
        create_plugin_with_apollo_config(apollo::Config {
            endpoint: Url::parse(ENDPOINT_DEFAULT).expect("default endpoint must be parseable"),
            apollo_key: Some("key".to_string()),
            apollo_graph_ref: Some("ref".to_string()),
            client_name_header: HeaderName::from_static("name_header"),
            client_version_header: HeaderName::from_static("version_header"),
            buffer_size: default_buffer_size(),
            schema_id: "schema_sha".to_string(),
            ..Default::default()
        })
    }

    async fn create_plugin_with_apollo_config(
        apollo_config: apollo::Config,
    ) -> Result<Telemetry, BoxError> {
        Telemetry::new(PluginInit::fake_new(
            config::Conf {
                apollo: apollo_config,
                ..Default::default()
            },
            Default::default(),
        ))
        .await
    }

    async fn create_subscription_plugin() -> Result<subscription::Subscription, BoxError> {
        subscription::Subscription::new(PluginInit::fake_new(
            subscription::SubscriptionConfig::default(),
            Default::default(),
        ))
        .await
    }
}
