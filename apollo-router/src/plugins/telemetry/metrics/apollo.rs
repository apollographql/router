// This entire file is license key functionality
//! Apollo metrics
use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::{MetricsBuilder, MetricsConfigurator};
use crate::stream::StreamExt;
use apollo_spaceport::{Reporter, ReporterError};
use async_trait::async_trait;
use deadpool::{managed, Runtime};
use futures::channel::mpsc;
use futures_batch::ChunksTimeoutStreamExt;
use std::time::Duration;
use studio::AggregatedMetrics;
use studio::Metrics;
use tower::BoxError;
use url::Url;

mod duration_histogram;
pub(crate) mod studio;

const DEFAULT_BATCH_SIZE: usize = 65_536;
const DEFAULT_QUEUE_SIZE: usize = 65_536;

#[derive(Clone)]
pub(crate) enum Sender {
    Noop,
    Spaceport(mpsc::Sender<Metrics>),
}

impl Sender {
    pub(crate) fn send(&self, metrics: Metrics) {
        match &self {
            Sender::Noop => {}
            Sender::Spaceport(channel) => {
                if let Err(err) = channel.to_owned().try_send(metrics) {
                    tracing::warn!(
                        "could not send metrics to spaceport, metric will be dropped: {}",
                        err
                    );
                }
            }
        }
    }
}

impl Default for Sender {
    fn default() -> Self {
        Sender::Noop
    }
}

impl MetricsConfigurator for Config {
    fn apply(
        &self,
        builder: MetricsBuilder,
        _metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError> {
        tracing::debug!("configuring Apollo metrics");

        Ok(match self {
            Config {
                endpoint: Some(endpoint),
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                schema_id: Some(schema_id),
                ..
            } => {
                let exporter = ApolloMetricsExporter::new(endpoint, key, reference, schema_id)?;

                builder
                    .with_apollo_metrics_collector(exporter.provider())
                    .with_exporter(exporter)
            }
            _ => builder,
        })
    }
}

struct ApolloMetricsExporter {
    tx: mpsc::Sender<Metrics>,
}

impl ApolloMetricsExporter {
    fn new(
        endpoint: &Url,
        apollo_key: &str,
        apollo_graph_ref: &str,
        schema_id: &str,
    ) -> Result<ApolloMetricsExporter, BoxError> {
        let apollo_key = apollo_key.to_string();
        // Desired behavior:
        // * Metrics are batched with a timeout.
        // * If we cannot connect to spaceport metrics are discarded and a warning raised.
        // * When the stream of metrics finishes we terminate the thread.
        // * If the exporter is dropped the remaining records are flushed.
        let (tx, rx) = mpsc::channel::<Metrics>(DEFAULT_QUEUE_SIZE);

        // TODO fill out this stuff
        let header = apollo_spaceport::ReportHeader {
            graph_ref: apollo_graph_ref.to_string(),
            hostname: "".to_string(),
            agent_version: "".to_string(),
            service_version: "".to_string(),
            runtime_version: "".to_string(),
            uname: "".to_string(),
            executable_schema_id: schema_id.to_string(),
        };

        // Deadpool gives us connection pooling to spaceport
        // It also significantly simplifies initialisation of the connection and gives us options in the future for configuring timeouts.
        let pool = deadpool::managed::Pool::<ReporterManager>::builder(ReporterManager {
            endpoint: endpoint.clone(),
        })
        .create_timeout(Some(Duration::from_secs(5)))
        .wait_timeout(Some(Duration::from_secs(5)))
        .runtime(Runtime::Tokio1)
        .build()
        .unwrap();

        // This is the thread that actually sends metrics
        tokio::spawn(async move {
            // We want to collect stats into batches, but also send periodically if a batch is not filled.
            // This implementation is not ideal as we do have to store all the data when really it could be folded as it is generated.
            // But in the interested of getting something over the line quickly let's go with this as it is simple to understand.
            rx.chunks_timeout(DEFAULT_BATCH_SIZE, Duration::from_secs(10))
                .for_each(|stats| async {
                    let aggregated_metrics = AggregatedMetrics::aggregate(stats);

                    match pool.get().await {
                        Ok(mut reporter) => {
                            let report =
                                AggregatedMetrics::to_report(header.clone(), aggregated_metrics);
                            match reporter
                                .submit(apollo_spaceport::ReporterRequest {
                                    apollo_key: apollo_key.clone(),
                                    report: Some(report),
                                })
                                .await
                            {
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!("failed to submit stats to spaceport: {}", e);
                                }
                            };
                        }
                        Err(err) => {
                            tracing::warn!(
                                "stats discarded as unable to get connection to spaceport: {}",
                                err
                            );
                        }
                    };
                })
                .await;
        });
        Ok(ApolloMetricsExporter { tx })
    }

    pub(crate) fn provider(&self) -> Sender {
        Sender::Spaceport(self.tx.clone())
    }
}

pub struct ReporterManager {
    endpoint: Url,
}

#[async_trait]
impl managed::Manager for ReporterManager {
    type Type = Reporter;
    type Error = ReporterError;

    async fn create(&self) -> Result<Reporter, Self::Error> {
        let url = self.endpoint.to_string();
        Ok(Reporter::try_new(url).await?)
    }

    async fn recycle(&self, _r: &mut Reporter) -> managed::RecycleResult<Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::future::Future;

    use http::header::HeaderName;

    use apollo_router_core::utils::test::IntoSchema::Canned;
    use apollo_router_core::utils::test::PluginTestHarness;
    use apollo_router_core::RouterRequest;
    use apollo_router_core::{Context, Plugin};

    use crate::plugins::telemetry::{apollo, Telemetry, STUDIO_EXCLUDE};

    use super::super::super::config;
    use super::*;

    #[tokio::test]
    async fn apollo_metrics_disabled() -> Result<(), BoxError> {
        let plugin = create_plugin_with_apollo_config(super::super::apollo::Config {
            endpoint: None,
            apollo_key: None,
            apollo_graph_ref: None,
            client_name_header: HeaderName::from_static("name_header"),
            client_version_header: HeaderName::from_static("version_header"),
            schema_id: Some("schema_sha".to_string()),
        })
        .await?;
        assert!(matches!(plugin.apollo_metrics_sender, Sender::Noop));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_enabled() -> Result<(), BoxError> {
        let plugin = create_plugin().await?;
        assert!(matches!(plugin.apollo_metrics_sender, Sender::Spaceport(_)));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_single_operation() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let results = get_metrics_for_request(query, None, None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_multiple_operations() -> Result<(), BoxError> {
        let query = "query {topProducts{name}} query {topProducts{name}}";
        let results = get_metrics_for_request(query, None, None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_parse_failure() -> Result<(), BoxError> {
        let query = "garbage";
        let results = get_metrics_for_request(query, None, None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_unknown_operation() -> Result<(), BoxError> {
        let query = "query {topProducts{name}}";
        let results = get_metrics_for_request(query, Some("UNKNOWN"), None).await?;
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_metrics_validation_failure() -> Result<(), BoxError> {
        let query = "query {topProducts{unknown}}";
        let results = get_metrics_for_request(query, None, None).await?;
        insta::with_settings!({sort_maps => true}, {
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
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!(results);
        });

        Ok(())
    }

    async fn get_metrics_for_request(
        query: &str,
        operation_name: Option<&str>,
        context: Option<Context>,
    ) -> Result<Vec<Metrics>, BoxError> {
        let _ = tracing_subscriber::fmt::try_init();
        let mut plugin = create_plugin().await?;
        // Replace the apollo metrics sender so we can test metrics collection.
        let (tx, rx) = futures::channel::mpsc::channel(100);
        plugin.apollo_metrics_sender = Sender::Spaceport(tx);
        let mut test_harness = PluginTestHarness::builder()
            .plugin(plugin)
            .schema(Canned)
            .build()
            .await?;
        let _ = test_harness
            .call(
                RouterRequest::fake_builder()
                    .header("name_header", "test_client")
                    .header("version_header", "1.0-test")
                    .query(query)
                    .and_operation_name(operation_name)
                    .and_context(context)
                    .build()?,
            )
            .await;

        drop(test_harness);
        let results = rx
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|mut m| {
                // Fix the latency counts to a known quantity so that insta tests don't fail.
                if m.query_latency_stats.latency_count != Duration::default() {
                    m.query_latency_stats.latency_count = Duration::from_millis(100);
                }
                m
            })
            .collect();
        Ok(results)
    }

    fn create_plugin() -> impl Future<Output = Result<Telemetry, BoxError>> {
        create_plugin_with_apollo_config(apollo::Config {
            endpoint: None,
            apollo_key: Some("key".to_string()),
            apollo_graph_ref: Some("ref".to_string()),
            client_name_header: HeaderName::from_static("name_header"),
            client_version_header: HeaderName::from_static("version_header"),
            schema_id: Some("schema_sha".to_string()),
        })
    }

    async fn create_plugin_with_apollo_config(
        apollo_config: apollo::Config,
    ) -> Result<Telemetry, BoxError> {
        Telemetry::new(config::Conf {
            metrics: None,
            tracing: None,
            apollo: Some(apollo_config),
        })
        .await
    }
}
