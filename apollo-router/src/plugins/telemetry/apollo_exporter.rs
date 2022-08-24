//! Configuration for apollo telemetry exporter.
// This entire file is license key functionality
use std::time::Duration;

use apollo_spaceport::ReportHeader;
use apollo_spaceport::Reporter;
use apollo_spaceport::ReporterError;
use async_trait::async_trait;
use deadpool::managed;
use deadpool::managed::Pool;
use deadpool::Runtime;
use futures::channel::mpsc;
use futures::stream::StreamExt;
use sys_info::hostname;
use tower::BoxError;
use url::Url;

use super::apollo::Report;
use super::apollo::SingleReport;
// use crate::plugins::telemetry::apollo::ReportBuilder;

const DEFAULT_QUEUE_SIZE: usize = 65_536;
// Do not set to 5 secs because it's also the default value for the BatchSpanProcesseur of tracing.
// It's less error prone to set a different value to let us compute traces and metrics
pub(crate) const EXPORTER_TIMEOUT_DURATION: Duration = Duration::from_secs(6);

#[derive(Clone)]
pub(crate) enum Sender {
    Noop,
    Spaceport(mpsc::Sender<SingleReport>),
}

impl Sender {
    pub(crate) fn send(&self, metrics: SingleReport) {
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

pub(crate) struct ApolloExporter {
    tx: mpsc::Sender<SingleReport>,
}

impl ApolloExporter {
    pub(crate) fn new(
        endpoint: &Url,
        apollo_key: &str,
        apollo_graph_ref: &str,
        schema_id: &str,
    ) -> Result<ApolloExporter, BoxError> {
        let apollo_key = apollo_key.to_string();
        // Desired behavior:
        // * Metrics are batched with a timeout.
        // * If we cannot connect to spaceport metrics are discarded and a warning raised.
        // * When the stream of metrics finishes we terminate the thread.
        // * If the exporter is dropped the remaining records are flushed.
        let (tx, mut rx) = mpsc::channel::<SingleReport>(DEFAULT_QUEUE_SIZE);

        let header = apollo_spaceport::ReportHeader {
            graph_ref: apollo_graph_ref.to_string(),
            hostname: hostname()?,
            agent_version: format!(
                "{}@{}",
                std::env!("CARGO_PKG_NAME"),
                std::env!("CARGO_PKG_VERSION")
            ),
            runtime_version: "rust".to_string(),
            uname: get_uname()?,
            executable_schema_id: schema_id.to_string(),
            ..Default::default()
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
            let timeout = tokio::time::interval(EXPORTER_TIMEOUT_DURATION);
            let mut report = Report::default();

            tokio::pin!(timeout);

            loop {
                tokio::select! {
                    single_report = rx.next() => {
                        if let Some(r) = single_report {
                            report += r;
                        } else {
                            break;
                        }
                       },
                    _ = timeout.tick() => {
                        Self::send_report(&pool, &apollo_key, &header, std::mem::take(&mut report)).await;
                    }
                };
            }

            Self::send_report(&pool, &apollo_key, &header, report).await;
        });
        Ok(ApolloExporter { tx })
    }

    pub(crate) fn provider(&self) -> Sender {
        Sender::Spaceport(self.tx.clone())
    }

    async fn send_report(
        pool: &Pool<ReporterManager>,
        apollo_key: &str,
        header: &ReportHeader,
        report: Report,
    ) {
        if report.operation_count == 0 && report.traces_per_query.is_empty() {
            return;
        }

        match pool.get().await {
            Ok(mut reporter) => {
                let report = report.into_report(header.clone());
                match reporter
                    .submit(apollo_spaceport::ReporterRequest {
                        apollo_key: apollo_key.to_string(),
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
    }
}

pub(crate) struct ReporterManager {
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

#[cfg(not(target_os = "windows"))]
pub(crate) fn get_uname() -> Result<String, std::io::Error> {
    let u = uname::uname()?;
    Ok(format!(
        "{}, {}, {}, {}, {},",
        u.sysname, u.nodename, u.release, u.version, u.machine
    ))
}

#[cfg(target_os = "windows")]
pub(crate) fn get_uname() -> Result<String, std::io::Error> {
    // Best we can do on windows right now
    let sysname = sys_info::os_type().unwrap_or_else(|_| "Windows".to_owned());
    let nodename = sys_info::hostname().unwrap_or_else(|_| "unknown".to_owned());
    let release = sys_info::os_release().unwrap_or_else(|_| "unknown".to_owned());
    let version = "unknown";
    let machine = "unknown";
    Ok(format!(
        "{}, {}, {}, {}, {}",
        sysname, nodename, release, version, machine
    ))
}
