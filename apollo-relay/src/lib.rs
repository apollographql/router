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

/// Reporting Error type
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
    /// Try to create a new Report.
    ///
    /// This can fail if the ReportHeader is not created.
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

        header.hostname = hostname()?;
        header.agent_version = std::env!("CARGO_PKG_VERSION").to_string();
        header.runtime_version = std::env!("CARGO_PKG_NAME").to_string();
        header.uname = get_uname()?;
        header.graph_ref = graph.to_string();
        Ok(header)
    }
}

/// The Reporter accepts requests from clients to transfer statistics
/// and traces to the Apollo Ingress relay.
#[derive(Debug)]
pub struct Reporter {
    client: ReporterClient<Channel>,
}

impl Reporter {
    /// Try to create a new reporter which will communicate with the supplied address.
    ///
    /// This can fail if:
    ///  - the address cannot be parsed
    ///  - the reporter can't connect to the address
    pub async fn try_new<T: AsRef<str>>(addr: T) -> Result<Self, ReporterError>
    where
        prost::bytes::Bytes: From<T>,
    {
        let ep = Endpoint::from_shared(addr)?;
        let client = ReporterClient::connect(ep).await?;
        Ok(Self { client })
    }

    /// Try to create a new reporter which will communicate with the supplied address.
    ///
    /// This can fail if:
    ///  - the address cannot be parsed
    ///  - the reporter can't connect to the address
    pub async fn try_new_with_static(addr: &'static str) -> Result<Self, ReporterError> {
        let ep = Endpoint::from_static(addr);
        let client = ReporterClient::connect(ep).await?;
        Ok(Self { client })
    }

    /// Submit these stats onto the relayer for eventual processing.
    ///
    /// The relayer will buffer traces and stats, transferring them when convenient.
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

    /// Submit this trace onto the relayer for eventual processing.
    ///
    /// The relayer will buffer traces and stats, transferring them when convenient.
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

/// The relay module contains the relaying components
pub mod relay {
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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::mpsc::Sender;
    use tokio::sync::Mutex;
    use tokio::time::{interval, Duration, MissedTickBehavior};
    use tonic::transport::{Error, Server};
    use tonic::{Request, Response, Status};

    pub use crate::agent::reporter_server::{Reporter, ReporterServer};
    use crate::agent::{ReporterGraph, ReporterResponse};

    /// Allows common transfer code to be more easily represented
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

    /// Response from Apollo Ingress
    #[derive(Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ApolloResponse {
        traces_ignored: bool,
    }

    /// Accept Traces and Stats from clients and transfer to an Apollo Ingress
    pub struct ReportRelay {
        addr: SocketAddr,
        // This HashMap will only have a single entry if used internally from a router.
        tpq: Arc<Mutex<HashMap<ReporterGraph, HashMap<String, report::TracesAndStats>>>>,
        tx: Sender<()>,
        total: AtomicU32,
    }

    impl ReportRelay {
        /// Create a new ReportRelay which is configured to serve requests at the
        /// supplied address
        ///
        /// The relay will buffer data and attempt to transfer it to the Apollo Ingress
        /// every 5 seconds. This transfer will be triggered sooner if data is
        /// accumulating more quickly than usual.
        ///
        /// The relay will attempt to make the transfer 5 times before failing. If
        /// the relay fails, the data is discarded.
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
                            tracing::trace!("relay triggered");
                            match mopt {
                                Some(_msg) => {
                                    relay_tpq(&client, task_tpq.clone()).await;
                                },
                                None => break
                            }
                        },
                        _ = interval.tick() => {
                            tracing::trace!("relay ticked");
                            relay_tpq(&client, task_tpq.clone()).await;
                        }
                    };
                }
            });
            Self {
                addr,
                tpq,
                tx,
                total: AtomicU32::new(0u32),
            }
        }

        /// Start serving requests.
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
            let req = client
                .post("https://usage-reporting.api.apollographql.com/api/ingress/traces")
                // .post("http://localhost:8080/api/ingress/traces") // XXX FOR TESTING
                .body(compressed_content)
                .header("X-Api-Key", key.clone())
                .header("Content-Encoding", "gzip")
                .header("Content-Type", "application/protobuf")
                .header("Accept", "application/json")
                .build()
                .map_err(|e| Status::failed_precondition(e.to_string()))?;

            for i in 0..4 {
                // We know these requests can be cloned
                let my_req = req.try_clone().expect("requests must be clone-able");
                let res = client.execute(my_req).await;
                match res {
                    Ok(_v) => break,
                    Err(e) => {
                        tracing::warn!("attempt: {}, could not transfer: {}", i + 1, e);
                        backoff += 50;
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff)).await;
                    }
                }
            }
            // Final attempt to transfer, if fails report error
            let res = client
                .execute(req)
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

    #[tonic::async_trait]
    impl Reporter for ReportRelay {
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

    impl ReportRelay {
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
            // Drop the tpq lock to maximise concurrency
            drop(tpq);

            let total = self.total.fetch_add(1, Ordering::SeqCst);
            if total > 5000 {
                let mut backoff = 0;
                for i in 0..4 {
                    match self.tx.send(()).await {
                        Ok(_v) => {
                            self.total.store(0, Ordering::SeqCst);
                            return Ok(Response::new(response));
                        }
                        Err(e) => {
                            tracing::warn!("attempt: {}, could not trigger transfer: {}", i + 1, e);
                            if i == 4 {
                                return Err(Status::internal(e.to_string()));
                            } else {
                                backoff += 50;
                                tokio::time::sleep(tokio::time::Duration::from_millis(backoff))
                                    .await;
                            }
                        }
                    }
                }
            }

            Ok(Response::new(response))
        }
    }

    async fn relay_tpq(
        client: &Client,
        task_tpq: Arc<Mutex<HashMap<ReporterGraph, HashMap<String, report::TracesAndStats>>>>,
    ) {
        let mut all_entries = task_tpq.lock().await;
        let drained = all_entries
            .drain()
            .collect::<Vec<(ReporterGraph, HashMap<String, report::TracesAndStats>)>>();
        // Release the lock ASAP so that clients can continue to add data
        drop(all_entries);
        for (graph, tpq) in drained {
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

                match ReportRelay::submit_report(client, graph.key, report).await {
                    Ok(v) => tracing::debug!("Report submission succeeded: {:?}", v),
                    Err(e) => tracing::error!("Report submission failed: {}", e),
                }
            }
        }
    }
}
