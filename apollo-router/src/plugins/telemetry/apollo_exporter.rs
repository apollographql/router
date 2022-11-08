//! Configuration for apollo telemetry exporter.
// This entire file is license key functionality
use bytes::BytesMut;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures::channel::mpsc;
use futures::stream::StreamExt;
use http::header::CONTENT_TYPE;
use reqwest::Client;
use std::io::Write;
use std::time::Duration;
use sys_info::hostname;
use tonic::Status;
use tower::BoxError;
use url::Url;

use super::apollo::Report;
use super::apollo::SingleReport;

const DEFAULT_QUEUE_SIZE: usize = 65_536;
// Do not set to 5 secs because it's also the default value for the BatchSpanProcesser of tracing.
// It's less error prone to set a different value to let us compute traces and metrics
pub(crate) const EXPORTER_TIMEOUT_DURATION: Duration = Duration::from_secs(6);
static DEFAULT_APOLLO_USAGE_REPORTING_INGRESS_URL: &str =
    "https://usage-reporting.api.apollographql.com/api/ingress/traces";
const BACKOFF_INCREMENT: Duration = Duration::from_millis(50);

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
        // * When the stream of metrics finishes we terminate the task.
        // * If the exporter is dropped the remaining records are flushed.
        let (tx, mut rx) = mpsc::channel::<SingleReport>(DEFAULT_QUEUE_SIZE);

        let header = crate::plugins::telemetry::apollo_exporter::proto::ReportHeader {
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

        let spaceport_endpoint = endpoint.clone();
        tracing::debug!(%spaceport_endpoint, "creating apollo exporter");

        // This is the task that actually sends metrics
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let timeout = tokio::time::interval(EXPORTER_TIMEOUT_DURATION);
            let mut report = Report::default();

            tokio::pin!(timeout);

            loop {
                tokio::select! {
                    single_report = rx.next() => {
                        if let Some(r) = single_report {
                            report += r;
                        } else {
                            tracing::debug!(%spaceport_endpoint, "terminating apollo exporter");
                            break;
                        }
                       },
                    _ = timeout.tick() => {
                        if let Err(e) = Self::submit_report(&client, &apollo_key, std::mem::take(&mut report).into_report(header.clone())).await {
                            tracing::error!("Failed to submit Apollo report: {}", e)
                        }
                    }
                };
            }

            if let Err(e) = Self::submit_report(
                &client,
                &apollo_key,
                std::mem::take(&mut report).into_report(header),
            )
            .await
            {
                tracing::error!("Failed to submit Apollo report: {}", e)
            }
        });
        Ok(ApolloExporter { tx })
    }

    pub(crate) fn provider(&self) -> Sender {
        Sender::Spaceport(self.tx.clone())
    }

    pub(crate) async fn submit_report(
        client: &Client,
        key: &str,
        report: crate::plugins::telemetry::apollo_exporter::proto::Report,
    ) -> Result<(), Status> {
        tracing::debug!("submitting report: {:?}", report);
        // Protobuf encode message
        let mut content = BytesMut::new();
        prost::Message::encode(&report, &mut content)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        // Create a gzip encoder
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        // Write our content to our encoder
        encoder
            .write_all(&content)
            .map_err(|e| Status::internal(e.to_string()))?;
        // Finish encoding and retrieve content
        let compressed_content = encoder
            .finish()
            .map_err(|e| Status::internal(e.to_string()))?;
        let mut backoff = Duration::from_millis(0);
        let ingress = match std::env::var("APOLLO_USAGE_REPORTING_INGRESS_URL") {
            Ok(v) => v,
            Err(_e) => DEFAULT_APOLLO_USAGE_REPORTING_INGRESS_URL.to_string(),
        };
        let req = client
            .post(ingress)
            .body(compressed_content)
            .header("X-Api-Key", key)
            .header("Content-Encoding", "gzip")
            .header(CONTENT_TYPE, "application/protobuf")
            .header("Accept", "application/json")
            .header(
                "User-Agent",
                format!(
                    "{} / {} usage reporting",
                    std::env!("CARGO_PKG_NAME"),
                    std::env!("CARGO_PKG_VERSION")
                ),
            )
            .build()
            .map_err(|e| Status::unavailable(e.to_string()))?;

        let mut msg = "default error message".to_string();
        for i in 0..5 {
            // We know these requests can be cloned
            let task_req = req.try_clone().expect("requests must be clone-able");
            match client.execute(task_req).await {
                Ok(v) => {
                    let status = v.status();
                    let data = v
                        .text()
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                    // Handle various kinds of status:
                    //  - if client error, terminate immediately
                    //  - if server error, it may be transient so treat as retry-able
                    //  - if ok, return ok
                    if status.is_client_error() {
                        tracing::error!("client error reported at ingress: {}", data);
                        return Err(Status::invalid_argument(data));
                    } else if status.is_server_error() {
                        tracing::warn!("attempt: {}, could not transfer: {}", i + 1, data);
                        msg = data;
                    } else {
                        tracing::debug!("ingress response text: {:?}", data);
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
        Err(Status::unavailable(msg))
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
    tonic::include_proto!("report");
}

pub(crate) use prost::*;
use serde::ser::SerializeStruct;
use std::error::Error;
use tokio::task::JoinError;
use tonic::codegen::http::uri::InvalidUri;

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
            curl -f {proto_url} > apollo-router/src/spaceport/proto/reports.proto\n\n"
    );
}
