pub mod report {
    tonic::include_proto!("report");
}

mod agent {
    tonic::include_proto!("agent");
}

use agent::reporter_client::ReporterClient;
pub use agent::ReporterGraph;
use agent::{ReporterResponse, ReporterStats, ReporterTrace};
pub use report::*;
use std::error::Error;
use std::hash::{Hash, Hasher};
use sys_info::hostname;
use tokio::task::JoinError;
use tonic::codegen::http::uri::InvalidUri;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};

#[derive(Debug)]
pub struct ReporterError {
    source: Box<dyn Error + Send + Sync + 'static>,
    msg: String,
}

#[derive(Debug, Clone)]
pub struct StudioGraph {
    pub reference: String,
    pub key: String,
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

impl Eq for ReporterGraph {}

// PartialEq is derived in the generated code, but Hash isn't and we need
// it to use this as key in a HashMap. We have to make sure this
// implementation always matches the derived PartialEq in the generated
// code.
#[allow(clippy::derive_hash_xor_eq)]
impl Hash for ReporterGraph {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.reference.hash(state);
        self.key.hash(state);
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
    // Best we can do on windows right now
    let sysname = sys_info::os_type().unwrap_or("Windows".to_string());
    let nodename = sys_info::hostname().unwrap_or("unknown".to_string());
    let release = sys_info::os_release().unwrap_or("unknown".to_string());
    let version = "unknown";
    let machine = "unknown";
    Ok(format!(
        "{}, {}, {}, {}, {}",
        sysname, nodename, release, version, machine
    ))
}

#[cfg(not(target_os = "windows"))]
fn get_uname() -> Result<String, std::io::Error> {
    let u = uname::uname()?;
    Ok(format!(
        "{}, {}, {}, {}, {},",
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
        graph: ReporterGraph,
        q: String,
        stats: ContextualizedStats,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client
            .add_stats(Request::new(ReporterStats {
                graph: Some(graph),
                key: q,
                stats: Some(stats),
            }))
            .await
    }

    /// Relay trace onto the collector
    pub async fn submit_trace(
        &mut self,
        graph: ReporterGraph,
        q: String,
        trace: Trace,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client
            .add_trace(Request::new(ReporterTrace {
                graph: Some(graph),
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
    use tokio::sync::mpsc::Sender;
    use tokio::sync::Mutex;
    use tokio::time::{interval, Duration, MissedTickBehavior};
    use tonic::transport::{Error, Server};
    use tonic::{Request, Response, Status};

    pub use crate::agent::reporter_server::{Reporter, ReporterServer};
    use crate::agent::{ReporterGraph, ReporterResponse};

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ApolloResponse {
        traces_ignored: bool,
    }

    pub struct ReportServer {
        addr: SocketAddr,
        // This HashMap will only have a single entry if used internally from a router.
        tpq: Arc<Mutex<HashMap<ReporterGraph, HashMap<String, report::TracesAndStats>>>>,
        tx: Sender<()>,
    }

    impl ReportServer {
        pub fn new(addr: SocketAddr) -> Self {
            // Spawn a task which will check if there are reports to
            // submit every interval.
            let tpq: Arc<Mutex<HashMap<ReporterGraph, HashMap<String, report::TracesAndStats>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let task_tpq = tpq.clone();
            let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(100);
            tokio::task::spawn(async move {
                let client = Client::new();
                let mut interval = interval(Duration::from_secs(5));
                interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                interval.tick().await;
                loop {
                    tokio::select! {
                        biased;
                        mopt = rx.recv() => {
                            match mopt {
                                Some(_msg) => {
                                    relay_tpq(&client, task_tpq.clone()).await;
                                },
                                None => break
                            }
                        },
                        _ = interval.tick() => {
                            relay_tpq(&client, task_tpq.clone()).await;
                        }
                    };
                }
            });
            Self { addr, tpq, tx }
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
            key: String,
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
            let mut backoff = 0;
            for i in 0..4 {
                let res = client
                    .post("https://usage-reporting.api.apollographql.com/api/ingress/traces")
                    .body(compressed_content.clone())
                    .header("X-Api-Key", key.clone())
                    .header("Content-Encoding", "gzip")
                    .header("Content-Type", "application/protobuf")
                    .header("Accept", "application/json")
                    .send()
                    .await;
                match res {
                    Ok(_v) => break,
                    Err(e) => {
                        tracing::warn!("attempt: {}, could not trigger transfer: {}", i + 1, e);
                        backoff += 500;
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff)).await;
                    }
                }
            }
            // Final attempt to transfer, if fails report error
            let res = client
                .post("https://usage-reporting.api.apollographql.com/api/ingress/traces")
                .body(compressed_content)
                .header("X-Api-Key", key)
                .header("Content-Encoding", "gzip")
                .header("Content-Type", "application/protobuf")
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(|e| Status::failed_precondition(e.to_string()))?;
            tracing::debug!("result: {:?}", res);
            let data = res
                .text()
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
            tracing::debug!("text: {:?}", data);
            /*
            let ar: ApolloResponse = res
                .json()
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
            tracing::debug!("json: {:?}", ar);
            */
            let response = ReporterResponse {
                message: "Report accepted".to_string(),
            };
            Ok(Response::new(response))
        }
    }
    #[allow(clippy::large_enum_variant)]
    enum StatsOrTrace {
        Stats(ReporterStats),
        Trace(ReporterTrace),
    }

    impl StatsOrTrace {
        fn get_traces_and_stats(&self) -> TracesAndStats {
            match self {
                StatsOrTrace::Stats(_) => TracesAndStats {
                    trace: vec![],
                    stats_with_context: vec![],
                    ..Default::default()
                },
                StatsOrTrace::Trace(_) => TracesAndStats {
                    stats_with_context: vec![],
                    ..Default::default()
                },
            }
        }

        fn graph(&self) -> Option<ReporterGraph> {
            match self {
                StatsOrTrace::Stats(s) => s.graph.clone(),
                StatsOrTrace::Trace(t) => t.graph.clone(),
            }
        }

        fn key(&self) -> String {
            match self {
                StatsOrTrace::Stats(s) => s.key.clone(),
                StatsOrTrace::Trace(t) => t.key.clone(),
            }
        }
    }

    #[tonic::async_trait]
    impl Reporter for ReportServer {
        async fn add_stats(
            &self,
            request: Request<ReporterStats>,
        ) -> Result<Response<ReporterResponse>, Status> {
            tracing::debug!("received request: {:?}", request);
            let msg = request.into_inner();
            self.add_stats_or_trace(StatsOrTrace::Stats(msg)).await
        }

        async fn add_trace(
            &self,
            request: Request<ReporterTrace>,
        ) -> Result<Response<ReporterResponse>, Status> {
            tracing::debug!("received request: {:?}", request);
            let msg = request.into_inner();
            self.add_stats_or_trace(StatsOrTrace::Trace(msg)).await
        }
    }

    impl ReportServer {
        async fn add_stats_or_trace(
            &self,
            record: StatsOrTrace,
        ) -> Result<Response<ReporterResponse>, Status> {
            let response = ReporterResponse {
                message: "Report accepted".to_string(),
            };
            let mut tpq = self.tpq.lock().await;
            let graph_map = tpq
                .entry(record.graph().unwrap())
                .or_insert_with(HashMap::new);
            let entry = graph_map
                .entry(record.key())
                .or_insert_with(|| record.get_traces_and_stats());
            match record {
                StatsOrTrace::Stats(mut s) => {
                    entry.stats_with_context.push(s.stats.take().unwrap())
                }
                StatsOrTrace::Trace(mut t) => entry.trace.push(t.trace.take().unwrap()),
            }

            // Trigger a dispatch if we have "too much" data
            let mut total = 0;
            for (_graph, entry) in tpq.iter() {
                total += entry.len();
            }

            if total > 10 {
                let mut backoff = 0;
                for _i in 0..4 {
                    match self.tx.send(()).await {
                        Ok(_v) => break,
                        Err(e) => {
                            tracing::warn!("could not trigger transfer: {}", e);
                            backoff += 500;
                            tokio::time::sleep(tokio::time::Duration::from_millis(backoff)).await;
                        }
                    }
                }
                // One last try and return error if fail
                self.tx
                    .send(())
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?;
            }

            Ok(Response::new(response))
        }
    }

    async fn relay_tpq(
        client: &Client,
        task_tpq: Arc<Mutex<HashMap<ReporterGraph, HashMap<String, report::TracesAndStats>>>>,
    ) {
        let mut all_entries = task_tpq.lock().await;
        for (graph, tpq) in all_entries.drain() {
            if !tpq.is_empty() {
                tracing::info!("submitting: {} records", tpq.len());
                tracing::debug!("containing: {:?}", tpq);
                let mut report = crate::Report::try_new(&graph.reference).expect("XXX");
                report.traces_per_query = tpq;
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

                match ReportServer::submit_report(client, graph.key, report).await {
                    Ok(v) => tracing::debug!("Report submission succeeded: {:?}", v),
                    Err(e) => tracing::error!("Report submission failed: {}", e),
                }
            }
        }
    }
}
