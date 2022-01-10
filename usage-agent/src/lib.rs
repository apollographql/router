pub mod report {
    tonic::include_proto!("report");
}

mod agent {
    tonic::include_proto!("agent");
}

use agent::reporter_client::ReporterClient;
use agent::{ReporterResponse, ReporterStats, ReporterTrace};
pub use report::*;
use std::error::Error;
use sys_info::hostname;
use tonic::codegen::http::uri::InvalidUri;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};

#[derive(Debug)]
pub struct ReporterError {
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

// Implement std::fmt::Display for ReporterError
impl std::fmt::Display for ReporterError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "ReporterError: source: {}, message: {}",
            self.source, self.msg
        )
    }
}

impl Report {
    pub fn try_new(graph: &str) -> Result<Self, ReporterError> {
        let header = Some(ReportHeader::try_new(graph)?);

        Ok(Report {
            header,
            ..Default::default()
        })
    }
}

#[cfg(target_os = "windows")]
fn get_uname() -> Result<String, std::io::Error> {
    // XXX Figure out at some point.
    Ok(format!(
        "{} {} {} {} {}",
        "sysname", "nodename", "release", "version", "machine"
    ))
}

#[cfg(not(target_os = "windows"))]
fn get_uname() -> Result<String, std::io::Error> {
    let u = uname::uname()?;
    Ok(format!(
        "{} {} {} {} {}",
        u.sysname, u.nodename, u.release, u.version, u.machine
    ))
}

impl ReportHeader {
    fn try_new(graph: &str) -> Result<Self, ReporterError> {
        let mut header = ReportHeader {
            ..Default::default()
        };

        header.add_hostname()?;
        header.agent_version = std::env!("CARGO_PKG_VERSION").to_string();
        header.runtime_version = "N/A".to_string();
        header.uname = get_uname()?;
        header.graph_ref = graph.to_string();
        Ok(header)
    }

    fn add_hostname(&mut self) -> Result<(), ReporterError> {
        self.hostname = hostname()?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct Reporter {
    client: ReporterClient<Channel>,
}

impl Reporter {
    pub async fn try_new<T: AsRef<str>>(addr: T) -> Result<Self, ReporterError>
    where
        prost::bytes::Bytes: From<T>,
    {
        let ep = Endpoint::from_shared(addr)?;
        let client = ReporterClient::connect(ep).await?;
        Ok(Self { client })
    }

    pub async fn try_new_with_static(addr: &'static str) -> Result<Self, ReporterError> {
        let ep = Endpoint::from_static(addr);
        let client = ReporterClient::connect(ep).await?;
        Ok(Self { client })
    }

    /// Relay stats onto the collector
    pub async fn submit_stats(
        &mut self,
        q: String,
        stats: ContextualizedStats,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client
            .add_stats(Request::new(ReporterStats {
                key: q,
                stats: Some(stats),
            }))
            .await
    }

    /// Relay trace onto the collector
    pub async fn submit_trace(
        &mut self,
        q: String,
        trace: Trace,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client
            .add_trace(Request::new(ReporterTrace {
                key: q,
                trace: Some(trace),
            }))
            .await
    }
}

pub mod server {
    use super::report;
    use crate::{ReporterStats, ReporterTrace, TracesAndStats};
    use bytes::BytesMut;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use prost::Message;
    use prost_types::Timestamp;
    use reqwest::Client;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::io::Write;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::Mutex;
    use tokio::time::{interval, Duration, MissedTickBehavior};
    use tonic::transport::{Error, Server};
    use tonic::{Request, Response, Status};

    pub use crate::agent::reporter_server::{Reporter, ReporterServer};
    use crate::agent::ReporterResponse;

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ApolloResponse {
        traces_ignored: bool,
    }

    pub struct ReportServer {
        addr: SocketAddr,
        tpq: Arc<Mutex<HashMap<String, report::TracesAndStats>>>,
    }

    impl ReportServer {
        pub fn new(addr: SocketAddr) -> Self {
            // Spawn a task which will check if there are reports to
            // submit every interval.
            let tpq = Arc::new(Mutex::new(HashMap::new()));
            let task_tpq = tpq.clone();
            tokio::task::spawn(async move {
                let client = Client::new();
                let mut interval = interval(Duration::from_secs(5));
                interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                interval.tick().await;
                loop {
                    let mut tpq = task_tpq.lock().await;
                    let current_tpq = std::mem::take(&mut *tpq);
                    drop(tpq);
                    if !current_tpq.is_empty() {
                        tracing::info!("submitting: {} records", current_tpq.len());
                        tracing::debug!("containing: {:?}", current_tpq);
                        let mut report =
                            crate::Report::try_new("Usage-Agent-uc0sri@current").expect("XXX");

                        report.traces_per_query = current_tpq;
                        let time = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .expect("Time went backwards");
                        let seconds = time.as_secs();
                        let nanos = time.as_nanos() - (seconds as u128 * 1_000_000_000);
                        let ts_end = Timestamp {
                            seconds: seconds as i64,
                            nanos: nanos as i32,
                        };
                        report.end_time = Some(ts_end);

                        match ReportServer::submit_report(&client, report).await {
                            Ok(v) => tracing::debug!("Report submission succeeded: {:?}", v),
                            Err(e) => tracing::error!("Report submission failed: {}", e),
                        }
                    }
                    interval.tick().await;
                }
            });
            Self { addr, tpq }
        }

        pub async fn serve(self) -> Result<(), Error> {
            let addr = self.addr;
            Server::builder()
                .add_service(ReporterServer::new(self))
                .serve(addr)
                .await
        }

        async fn submit_report(
            client: &Client,
            report: report::Report,
        ) -> Result<Response<ReporterResponse>, Status> {
            tracing::debug!("submitting report: {:?}", report);
            // Protobuf encode message
            let mut content = BytesMut::new();
            report
                .encode(&mut content)
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
            let res = client
                .post("https://usage-reporting.api.apollographql.com/api/ingress/traces")
                .body(compressed_content)
                .header(
                    "X-Api-Key",
                    std::env::var("X_API_KEY")
                        .map_err(|e| Status::unauthenticated(e.to_string()))?,
                )
                .header("Content-Encoding", "gzip")
                .header("Content-Type", "application/protobuf")
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(|e| Status::failed_precondition(e.to_string()))?;
            println!("result: {:?}", res);
            let data = res
                .text()
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
            println!("text: {:?}", data);
            /*
            let ar: ApolloResponse = res
                .json()
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
            println!("json: {:?}", ar);
            */
            let response = ReporterResponse {
                message: "Report accepted".to_string(),
            };
            Ok(Response::new(response))
        }
    }

    #[tonic::async_trait]
    impl Reporter for ReportServer {
        async fn add_stats(
            &self,
            request: Request<ReporterStats>,
        ) -> Result<Response<ReporterResponse>, Status> {
            println!("received request: {:?}", request);
            let msg = request.into_inner();
            let response = ReporterResponse {
                message: "Report accepted".to_string(),
            };
            let mut tpq = self.tpq.lock().await;
            let entry = tpq.entry(msg.key).or_insert(TracesAndStats {
                stats_with_context: vec![],
                ..Default::default()
            });
            entry.stats_with_context.push(msg.stats.unwrap());

            Ok(Response::new(response))
        }

        async fn add_trace(
            &self,
            request: Request<ReporterTrace>,
        ) -> Result<Response<ReporterResponse>, Status> {
            println!("received request: {:?}", request);
            let msg = request.into_inner();
            let response = ReporterResponse {
                message: "Report accepted".to_string(),
            };
            let mut tpq = self.tpq.lock().await;
            let entry = tpq.entry(msg.key).or_insert(TracesAndStats {
                trace: vec![],
                stats_with_context: vec![],
                ..Default::default()
            });
            entry.trace.push(msg.trace.unwrap());

            Ok(Response::new(response))
        }
    }
}
