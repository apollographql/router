//! Configuration for apollo telemetry exporter.
use std::error::Error;
use std::fmt::Debug;
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use bytes::BytesMut;
use flate2::write::GzEncoder;
use flate2::Compression;
use http::header::ACCEPT;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_TYPE;
use http::header::RETRY_AFTER;
use http::header::USER_AGENT;
use http::StatusCode;
use opentelemetry::ExportError;
pub(crate) use prost::*;
use reqwest::Client;
use serde::ser::SerializeStruct;
use serde_json::Value;
use sys_info::hostname;
use tokio::sync::mpsc;
use tokio::task::JoinError;
use tonic::codegen::http::uri::InvalidUri;
use tower::BoxError;
use url::Url;

use super::apollo::Report;
use super::apollo::SingleReport;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;

const BACKOFF_INCREMENT: Duration = Duration::from_millis(50);
const ROUTER_REPORT_TYPE_METRICS: &str = "metrics";
const ROUTER_REPORT_TYPE_TRACES: &str = "traces";

#[derive(thiserror::Error, Debug)]
pub(crate) enum ApolloExportError {
    #[error("Apollo exporter server error: {0}")]
    ServerError(String),

    #[error("Apollo exporter client error: {0}")]
    ClientError(String),

    #[error("Apollo exporter unavailable error: {0}")]
    Unavailable(String),

    #[error("Apollo Studio not accepting reports for {1} seconds")]
    StudioBackoff(Report, u64),
}

impl ExportError for ApolloExportError {
    fn exporter_name(&self) -> &'static str {
        "ApolloExporter"
    }
}

#[derive(Clone, Default)]
pub(crate) enum Sender {
    #[default]
    Noop,
    Apollo(mpsc::Sender<SingleReport>),
}

impl Sender {
    pub(crate) fn send(&self, report: SingleReport) {
        match &self {
            Sender::Noop => {}
            Sender::Apollo(channel) => {
                if let Err(err) = channel.to_owned().try_send(report) {
                    tracing::warn!(
                        "could not send metrics to telemetry, metric will be dropped: {}",
                        err
                    );
                }
            }
        }
    }
}

/// The Apollo exporter is responsible for attaching report header information for individual requests
/// Retrying when sending fails.
/// Sending periodically (in the case of metrics).
pub(crate) struct ApolloExporter {
    batch_config: BatchProcessorConfig,
    endpoint: Url,
    apollo_key: String,
    header: proto::reports::ReportHeader,
    client: Client,
    strip_traces: AtomicBool,
    studio_backoff: Mutex<Instant>,
}

impl ApolloExporter {
    pub(crate) fn new(
        endpoint: &Url,
        batch_config: &BatchProcessorConfig,
        apollo_key: &str,
        apollo_graph_ref: &str,
        schema_id: &str,
    ) -> Result<ApolloExporter, BoxError> {
        let header = proto::reports::ReportHeader {
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

        tracing::debug!("creating apollo exporter {}", endpoint);
        Ok(ApolloExporter {
            endpoint: endpoint.clone(),
            batch_config: batch_config.clone(),
            apollo_key: apollo_key.to_string(),
            client: reqwest::Client::builder()
                .no_gzip()
                .timeout(batch_config.max_export_timeout)
                .build()
                .map_err(BoxError::from)?,
            header,
            strip_traces: Default::default(),
            studio_backoff: Mutex::new(Instant::now()),
        })
    }

    pub(crate) fn start(self) -> Sender {
        let (tx, mut rx) = mpsc::channel::<SingleReport>(self.batch_config.max_queue_size);
        tokio::spawn(async move {
            let timeout = tokio::time::interval(self.batch_config.scheduled_delay);
            let mut report = Report::default();
            let mut backoff_warn = true;

            tokio::pin!(timeout);

            loop {
                tokio::select! {
                    // If you run this example without `biased;`, the polling order is
                    // pseudo-random and may never choose the timeout tick
                    biased;
                    _ = timeout.tick() => {
                        match self.submit_report(std::mem::take(&mut report)).await {
                            Ok(_) => backoff_warn = true,
                            Err(err) => {
                                match err {
                                    ApolloExportError::StudioBackoff(unsubmitted, remaining) => {
                                        if backoff_warn {
                                            tracing::warn!("Apollo Studio not accepting reports for {remaining} seconds");
                                            backoff_warn = false;
                                        }
                                        report = unsubmitted;
                                    },
                                    _ => tracing::error!("failed to submit Apollo report: {}", err)
                                }
                            }
                        }
                    }
                    single_report = rx.recv() => {
                        if let Some(r) = single_report {
                            report += r;
                        } else {
                            tracing::debug!("terminating apollo exporter");
                            break;
                        }
                    },
                };
            }

            if let Err(e) = self.submit_report(std::mem::take(&mut report)).await {
                tracing::error!("failed to submit Apollo report: {}", e)
            }
        });
        Sender::Apollo(tx)
    }

    pub(crate) async fn submit_report(&self, report: Report) -> Result<(), ApolloExportError> {
        // We may be sending traces but with no operation count
        if report.licensed_operation_count_by_type.is_empty() && report.traces_per_query.is_empty()
        {
            return Ok(());
        }

        // If studio has previously told us not to submit reports, return for further processing
        let expires_at = *self.studio_backoff.lock().unwrap();
        let now = Instant::now();
        if expires_at > now {
            let remaining = expires_at - now;
            return Err(ApolloExportError::StudioBackoff(
                report,
                remaining.as_secs(),
            ));
        }

        tracing::debug!("submitting report: {:?}", report);
        // Protobuf encode message
        let mut content = BytesMut::new();
        let mut proto_report = report.build_proto_report(self.header.clone());
        prost::Message::encode(&proto_report, &mut content)
            .map_err(|e| ApolloExportError::ClientError(e.to_string()))?;
        // Create a gzip encoder
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        // Write our content to our encoder
        encoder
            .write_all(&content)
            .map_err(|e| ApolloExportError::ClientError(e.to_string()))?;
        // Finish encoding and retrieve content
        let compressed_content = encoder
            .finish()
            .map_err(|e| ApolloExportError::ClientError(e.to_string()))?;
        let mut backoff = Duration::from_millis(0);
        let req = self
            .client
            .post(self.endpoint.clone())
            .body(compressed_content)
            .header("X-Api-Key", self.apollo_key.clone())
            .header(CONTENT_ENCODING, "gzip")
            .header(CONTENT_TYPE, "application/protobuf")
            .header(ACCEPT, "application/json")
            .header(
                USER_AGENT,
                format!(
                    "{} / {} usage reporting",
                    std::env!("CARGO_PKG_NAME"),
                    std::env!("CARGO_PKG_VERSION")
                ),
            )
            .build()
            .map_err(|e| ApolloExportError::Unavailable(e.to_string()))?;

        let mut msg = "default error message".to_string();
        let mut has_traces = false;

        for (_, traces_and_stats) in proto_report.traces_per_query.iter_mut() {
            if !traces_and_stats.trace.is_empty() {
                has_traces = true;
                if self.strip_traces.load(Ordering::SeqCst) {
                    traces_and_stats.trace.clear();
                }
            }
        }

        // We want to retry if we have traces...
        let retries = if has_traces { 5 } else { 1 };

        for i in 0..retries {
            // We know these requests can be cloned
            let task_req = req.try_clone().expect("requests must be clone-able");
            match self.client.execute(task_req).await {
                Ok(v) => {
                    let status = v.status();
                    let opt_header_retry = v.headers().get(RETRY_AFTER).cloned();
                    let data = v
                        .text()
                        .await
                        .map_err(|e| ApolloExportError::ServerError(e.to_string()))?;
                    // Handle various kinds of status:
                    //  - if client error, terminate immediately
                    //  - if server error, it may be transient so treat as retry-able
                    //  - if ok, return ok
                    if status.is_client_error() {
                        tracing::error!("client error reported at ingress: {}", data);
                        return Err(ApolloExportError::ClientError(data));
                    } else if status.is_server_error() {
                        tracing::warn!("attempt: {}, could not transfer: {}", i + 1, data);
                        msg = data;
                        if status == StatusCode::TOO_MANY_REQUESTS {
                            // We should have a Retry-After header to go with the status code
                            // If we don't have the header, or it isn't a valid string or we can't
                            // convert it to u64, just ignore it. Otherwise, interpret it as a
                            // number of seconds for which we should not attempt to send any more
                            // reports.
                            let mut retry_after = 0;
                            if let Some(returned_retry_after) =
                                opt_header_retry.and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
                            {
                                retry_after = returned_retry_after;
                                *self.studio_backoff.lock().unwrap() =
                                    Instant::now() + Duration::from_secs(retry_after);
                            }
                            // Even if we can't update the studio_backoff, we should not continue to
                            // retry here. We'd better just return the error.
                            return Err(ApolloExportError::StudioBackoff(report, retry_after));
                        }
                    } else {
                        tracing::debug!("ingress response text: {:?}", data);
                        let report_type = if has_traces {
                            ROUTER_REPORT_TYPE_TRACES
                        } else {
                            ROUTER_REPORT_TYPE_METRICS
                        };
                        u64_counter!(
                            "apollo.router.telemetry.studio.reports",
                            "The number of reports submitted to Studio by the Router",
                            1,
                            report.type = report_type
                        );
                        if has_traces && !self.strip_traces.load(Ordering::SeqCst) {
                            // If we had traces then maybe disable sending traces from this exporter based on the response.
                            if let Ok(response) = serde_json::Value::from_str(&data) {
                                if let Some(Value::Bool(true)) = response.get("tracesIgnored") {
                                    tracing::warn!("traces will not be sent to Apollo as this account is on a free plan");
                                    self.strip_traces.store(true, Ordering::SeqCst);
                                }
                            }
                        }
                        return Ok(());
                    }
                }
                Err(e) => {
                    // TODO: Ultimately need more sophisticated handling here. For example
                    // a redirect should not be treated the same way as a connect or a
                    // type builder error...
                    tracing::warn!("attempt: {}, could not transfer: {}", i + 1, e);
                    msg = e.to_string();
                }
            }
            backoff += BACKOFF_INCREMENT;
            tokio::time::sleep(backoff).await;
        }
        Err(ApolloExportError::Unavailable(msg))
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

#[allow(unreachable_pub)]
pub(crate) mod proto {
    pub(crate) mod reports {
        #![allow(clippy::derive_partial_eq_without_eq)]
        tonic::include_proto!("reports");
    }
}

/// Reporting Error type
#[derive(Debug)]
pub(crate) struct ReporterError {
    source: Box<dyn Error + Send + Sync + 'static>,
    msg: String,
}

impl std::error::Error for ReporterError {}

impl From<InvalidUri> for ReporterError {
    fn from(error: InvalidUri) -> Self {
        ReporterError {
            msg: error.to_string(),
            source: Box::new(error),
        }
    }
}

impl From<tonic::transport::Error> for ReporterError {
    fn from(error: tonic::transport::Error) -> Self {
        ReporterError {
            msg: error.to_string(),
            source: Box::new(error),
        }
    }
}

impl From<std::io::Error> for ReporterError {
    fn from(error: std::io::Error) -> Self {
        ReporterError {
            msg: error.to_string(),
            source: Box::new(error),
        }
    }
}

impl From<sys_info::Error> for ReporterError {
    fn from(error: sys_info::Error) -> Self {
        ReporterError {
            msg: error.to_string(),
            source: Box::new(error),
        }
    }
}

impl From<JoinError> for ReporterError {
    fn from(error: JoinError) -> Self {
        ReporterError {
            msg: error.to_string(),
            source: Box::new(error),
        }
    }
}

impl std::fmt::Display for ReporterError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "ReporterError: source: {}, message: {}",
            self.source, self.msg
        )
    }
}

pub(crate) fn serialize_timestamp<S>(
    timestamp: &Option<prost_types::Timestamp>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match timestamp {
        Some(ts) => {
            let mut ts_strukt = serializer.serialize_struct("Timestamp", 2)?;
            ts_strukt.serialize_field("seconds", &ts.seconds)?;
            ts_strukt.serialize_field("nanos", &ts.nanos)?;
            ts_strukt.end()
        }
        None => serializer.serialize_none(),
    }
}

#[cfg(not(windows))] // git checkout converts \n to \r\n, making == below fail
#[test]
fn check_reports_proto_is_up_to_date() {
    let proto_url = "https://usage-reporting.api.apollographql.com/proto/reports.proto";
    let response = reqwest::blocking::get(proto_url).unwrap();
    let content = response.text().unwrap();
    // Not using assert_eq! as printing the entire file would be too verbose
    assert!(
        content == include_str!("proto/reports.proto"),
        "Protobuf file is out of date. Run this command to update it:\n\n    \
            curl -f {proto_url} > apollo-router/src/plugins/telemetry/proto/reports.proto\n\n"
    );
}
