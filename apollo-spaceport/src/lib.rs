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
/// and traces to the Apollo Ingress spaceport.
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

    /// Submit these stats onto the spaceport for eventual processing.
    ///
    /// The spaceport will buffer traces and stats, transferring them when convenient.
    pub async fn submit_stats(
        &mut self,
        graph: ReporterGraph,
        key: String,
        stats: ContextualizedStats,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client
            .add_stats(Request::new(ReporterStats {
                graph: Some(graph),
                key,
                stats: Some(stats),
            }))
            .await
    }

    /// Submit this trace onto the spaceport for eventual processing.
    ///
    /// The spaceport will buffer traces and stats, transferring them when convenient.
    pub async fn submit_trace(
        &mut self,
        graph: ReporterGraph,
        key: String,
        trace: Trace,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client
            .add_trace(Request::new(ReporterTrace {
                graph: Some(graph),
                key,
                trace: Some(trace),
            }))
            .await
    }
}

/// The spaceport module contains the spaceport components
pub mod spaceport {
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

    static DEFAULT_INGRESS: &str =
        "https://usage-reporting.api.apollographql.com/api/ingress/traces";

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
                    stats_with_context: vec![],
                    ..Default::default()
                },
                StatsOrTrace::Trace(_) => TracesAndStats {
                    trace: vec![],
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
    pub struct ReportSpaceport {
        addr: SocketAddr,
        // This HashMap will only have a single entry if used internally from a router.
        tpq: Arc<Mutex<HashMap<ReporterGraph, HashMap<String, report::TracesAndStats>>>>,
        tx: Sender<()>,
        total: AtomicU32,
    }

    impl ReportSpaceport {
        /// Create a new ReportSpaceport which is configured to serve requests at the
        /// supplied address
        ///
        /// The spaceport will buffer data and attempt to transfer it to the Apollo Ingress
        /// every 5 seconds. This transfer will be triggered sooner if data is
        /// accumulating more quickly than usual.
        ///
        /// The spaceport will attempt to make the transfer 5 times before failing. If
        /// the spaceport fails, the data is discarded.
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
                            tracing::trace!("spaceport triggered");
                            match mopt {
                                Some(_msg) => {
                                    for result in extract_tpq(&client, task_tpq.clone()).await {
                                        match result {
                                            Ok(v) => tracing::debug!("Report submission succeeded: {:?}", v),
                                            Err(e) => tracing::error!("Report submission failed: {}", e),
                                        }
                                    }
                                },
                                None => break
                            }
                        },
                        _ = interval.tick() => {
                            tracing::trace!("spaceport ticked");
                            for result in extract_tpq(&client, task_tpq.clone()).await {
                                match result {
                                    Ok(v) => tracing::debug!("Report submission succeeded: {:?}", v),
                                    Err(e) => tracing::error!("Report submission failed: {}", e),
                                }
                            }
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
            let mut backoff = Duration::from_millis(0);
            let ingress = match std::env::var("APOLLO_INGRESS") {
                Ok(v) => v,
                Err(_e) => DEFAULT_INGRESS.to_string(),
            };
            let req = client
                .post(ingress)
                .body(compressed_content)
                .header("X-Api-Key", key)
                .header("Content-Encoding", "gzip")
                .header("Content-Type", "application/protobuf")
                .header("Accept", "application/json")
                .build()
                .map_err(|e| Status::unavailable(e.to_string()))?;

            for i in 0..4 {
                // We know these requests can be cloned
                let my_req = req.try_clone().expect("requests must be clone-able");
                match client.execute(my_req).await {
                    Ok(v) => {
                        let data = v
                            .text()
                            .await
                            .map_err(|e| Status::internal(e.to_string()))?;
                        tracing::debug!("text: {:?}", data);
                        /*
                        let ar: ApolloResponse = v
                            .json()
                            .await
                            .map_err(|e| Status::internal(e.to_string()))?;
                        tracing::debug!("json: {:?}", ar);
                        */
                        let response = ReporterResponse {
                            message: "Report accepted".to_string(),
                        };
                        return Ok(Response::new(response));
                    }
                    Err(e) => {
                        tracing::warn!("attempt: {}, could not transfer: {}", i + 1, e);
                        backoff += Duration::from_millis(50);
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
            // One last try to transfer, if fail, report unavailable
            match client.execute(req).await {
                Ok(v) => {
                    let data = v
                        .text()
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                    tracing::debug!("text: {:?}", data);
                    /*
                    let ar: ApolloResponse = v
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
                Err(e) => Err(Status::unavailable(e.to_string())),
            }
        }
    }

    #[tonic::async_trait]
    impl Reporter for ReportSpaceport {
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

    impl ReportSpaceport {
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

            // This is inherently both imprecise and racy, but it doesn't matter
            // because we are just hinting to the spaceport that it's probably a
            // good idea to try to transfer data up to the ingress. Multiple
            // notifications just trigger more transfers.
            let total = self.total.fetch_add(1, Ordering::SeqCst);
            if total > 5000 {
                let mut backoff = Duration::from_millis(0);
                for i in 0..4 {
                    match self.tx.send(()).await {
                        Ok(_v) => {
                            self.total.store(0, Ordering::SeqCst);
                            return Ok(Response::new(response));
                        }
                        Err(e) => {
                            tracing::warn!("attempt: {}, could not trigger transfer: {}", i + 1, e);
                            if i == 4 {
                                return Err(Status::unavailable(e.to_string()));
                            } else {
                                backoff += Duration::from_millis(50);
                                tokio::time::sleep(backoff).await;
                            }
                        }
                    }
                }
            }

            Ok(Response::new(response))
        }
    }

    async fn extract_tpq(
        client: &Client,
        task_tpq: Arc<Mutex<HashMap<ReporterGraph, HashMap<String, report::TracesAndStats>>>>,
    ) -> Vec<Result<Response<ReporterResponse>, Status>> {
        let mut all_entries = task_tpq.lock().await;
        let drained = all_entries
            .drain()
            .collect::<Vec<(ReporterGraph, HashMap<String, report::TracesAndStats>)>>();
        // Release the lock ASAP so that clients can continue to add data
        drop(all_entries);
        let mut results = vec![];
        for (graph, tpq) in drained {
            if !tpq.is_empty() {
                tracing::info!("submitting: {} records", tpq.len());
                tracing::debug!("containing: {:?}", tpq);
                match crate::Report::try_new(&graph.reference) {
                    Ok(mut report) => {
                        report.traces_per_query = tpq;
                        let time = match SystemTime::now().duration_since(UNIX_EPOCH) {
                            Ok(t) => t,
                            Err(e) => {
                                results.push(Err(Status::internal(e.to_string())));
                                continue;
                            }
                        };
                        let seconds = time.as_secs();
                        let nanos = time.as_nanos() - (seconds as u128 * 1_000_000_000);
                        let ts_end = Timestamp {
                            seconds: seconds as i64,
                            nanos: nanos as i32,
                        };
                        report.end_time = Some(ts_end);

                        results
                            .push(ReportSpaceport::submit_report(client, graph.key, report).await)
                    }
                    Err(e) => {
                        results.push(Err(Status::internal(e.to_string())));
                        continue;
                    }
                }
            }
        }
        results
    }
}
