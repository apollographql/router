//! Apollo metrics
// With regards to ELv2 licensing, this entire file is license key functionality
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tower::BoxError;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::apollo_exporter::ApolloExporter;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;

mod duration_histogram;
pub(crate) mod studio;

impl MetricsConfigurator for Config {
    fn apply(
        &self,
        builder: MetricsBuilder,
        _metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        tracing::debug!("configuring Apollo metrics");
        static ENABLED: AtomicBool = AtomicBool::new(false);
        Ok(match self {
            Config {
                endpoint,
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                schema_id,
                batch_processor,
                ..
            } => {
                if !ENABLED.swap(true, Ordering::Relaxed) {
                    tracing::info!("Apollo Studio usage reporting is enabled. See https://go.apollo.dev/o/data for details");
                }
                let batch_processor_config = batch_processor;
                tracing::debug!("creating metrics exporter");
                let exporter = ApolloExporter::new(
                    endpoint,
                    batch_processor_config,
                    key,
                    reference,
                    schema_id,
                )?;

                builder.with_apollo_metrics_collector(exporter.start())
            }
            _ => {
                ENABLED.swap(false, Ordering::Relaxed);
                builder
            }
        })
    }
}

#[cfg(test)]
mod test {
    use std::future::Future;
    use std::time::Duration;

    use futures::stream::StreamExt;
    use http::header::HeaderName;
    use tower::ServiceExt;
    use url::Url;

    use super::super::super::config;
    use super::studio::SingleStatsReport;
    use super::*;
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::plugins::telemetry::apollo;
    use crate::plugins::telemetry::apollo::default_buffer_size;
    use crate::plugins::telemetry::apollo::ENDPOINT_DEFAULT;
    use crate::plugins::telemetry::apollo_exporter::Sender;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::telemetry::STUDIO_EXCLUDE;
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
        let results = get_metrics_for_request(query, None, None).await?;
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
        let results = get_metrics_for_request(query, None, None).await?;
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
        let results = get_metrics_for_request(query, None, None).await?;
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
        let results = get_metrics_for_request(query, Some("UNKNOWN"), None).await?;
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.add_redaction("[].request_id", "[REDACTED]");
        settings.bind(|| insta::assert_json_snapshot!(results));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_validation_failure() -> Result<(), BoxError> {
        let query = "query {topProducts{unknown}}";
        let results = get_metrics_for_request(query, None, None).await?;
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
        let results = get_metrics_for_request(query, None, Some(context)).await?;
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
    ) -> Result<Vec<SingleStatsReport>, BoxError> {
        let _ = tracing_subscriber::fmt::try_init();
        let mut plugin = create_plugin().await?;
        // Replace the apollo metrics sender so we can test metrics collection.
        let (tx, rx) = futures::channel::mpsc::channel(100);
        plugin.apollo_metrics_sender = Sender::Apollo(tx);
        TestHarness::builder()
            .extra_plugin(plugin)
            .build_router()
            .await?
            .oneshot(
                SupergraphRequest::fake_builder()
                    .header("name_header", "test_client")
                    .header("version_header", "1.0-test")
                    .query(query)
                    .and_operation_name(operation_name)
                    .and_context(context)
                    .build()?
                    .try_into()
                    .unwrap(),
            )
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap();

        let default_latency = Duration::from_millis(100);
        let results = rx
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
        Telemetry::new(PluginInit::new(
            config::Conf {
                logging: Default::default(),
                metrics: None,
                tracing: None,
                apollo: Some(apollo_config),
            },
            Default::default(),
        ))
        .await
    }
}
