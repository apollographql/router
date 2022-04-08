use crate::{
    agent::{
        reporter_server::{Reporter, ReporterServer},
        ReporterGraph, ReporterResponse,
    },
    report::Report,
    ReporterStats, ReporterTrace, TracesAndStats,
};
use bytes::BytesMut;
use flate2::write::GzEncoder;
use flate2::Compression;
use prost::Message;
use prost_types::Timestamp;
use reqwest::Client;
use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration, MissedTickBehavior};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Error, Server};
use tonic::{Request, Response, Status};

type QueryUsageMap = HashMap<String, TracesAndStats>;
type GraphUsageMap = Arc<Mutex<HashMap<ReporterGraph, QueryUsageMap>>>;

static DEFAULT_APOLLO_USAGE_REPORTING_INGRESS_URL: &str =
    "https://usage-reporting.api.apollographql.com/api/ingress/traces";
static INGRESS_CLOCK_TICK: Duration = Duration::from_secs(5);
static TRIGGER_BATCH_LIMIT: u32 = 50;

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

/// Accept Traces and Stats from clients and transfer to an Apollo Ingress
pub struct ReportSpaceport {
    shutdown_signal: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
    listener: Option<TcpListener>,
    addr: SocketAddr,
    // This Map will only have a single entry if used internally from a router.
    // (because a router can only be serving a single graph)
    graph_usage: GraphUsageMap,
    tx: Sender<()>,
    total: Arc<AtomicU32>,
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
    pub async fn new(
        addr: SocketAddr,
        shutdown_signal: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;

        // Spawn a task which will check if there are reports to
        // submit every interval.
        let graph_usage: GraphUsageMap = Arc::new(Mutex::new(HashMap::new()));
        let task_graph_usage = graph_usage.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(10);
        let total = Arc::new(AtomicU32::new(0u32));
        let task_total = total.clone();
        tokio::task::spawn(async move {
            let client = Client::new();
            let mut interval = interval(INGRESS_CLOCK_TICK);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            interval.tick().await;
            loop {
                tokio::select! {
                    biased;
                    mopt = rx.recv() => {
                        tracing::debug!("spaceport triggered");
                        match mopt {
                            Some(_msg) => process_all_graphs(&client, task_graph_usage.clone()).await,
                            None => break
                        }
                    },
                    _ = interval.tick() => {
                        tracing::trace!("spaceport ticked");
                        task_total.store(0, Ordering::SeqCst);
                        process_all_graphs(&client, task_graph_usage.clone()).await;
                    }
                };
            }
        });
        Ok(Self {
            shutdown_signal,
            listener: Some(listener),
            addr,
            graph_usage,
            tx,
            total,
        })
    }

    pub fn address(&self) -> &SocketAddr {
        &self.addr
    }

    /// Start serving requests.
    pub async fn serve(mut self) -> Result<(), Error> {
        let shutdown_signal = self
            .shutdown_signal
            .take()
            .unwrap_or_else(|| Box::pin(std::future::pending()));
        let listener = self
            .listener
            .take()
            .expect("should have allocated listener");
        Server::builder()
            .add_service(ReporterServer::new(self))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown_signal)
            .await
    }

    async fn submit_report(
        client: &Client,
        key: String,
        report: Report,
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
        let ingress = match std::env::var("APOLLO_USAGE_REPORTING_INGRESS_URL") {
            Ok(v) => v,
            Err(_e) => DEFAULT_APOLLO_USAGE_REPORTING_INGRESS_URL.to_string(),
        };
        let req = client
            .post(ingress)
            .body(compressed_content)
            .header("X-Api-Key", key)
            .header("Content-Encoding", "gzip")
            .header("Content-Type", "application/protobuf")
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
                        let response = ReporterResponse {
                            message: "Report accepted".to_string(),
                        };
                        return Ok(Response::new(response));
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
            backoff += Duration::from_millis(50);
            tokio::time::sleep(backoff).await;
        }
        Err(Status::unavailable(msg))
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
        let mut graph_usage = self.graph_usage.lock().await;
        let graph_map = graph_usage
            .entry(record.graph().unwrap())
            .or_insert_with(HashMap::new);
        let entry = graph_map
            .entry(record.key())
            .or_insert_with(|| record.get_traces_and_stats());
        match record {
            StatsOrTrace::Stats(mut s) => entry.stats_with_context.push(s.stats.take().unwrap()),
            StatsOrTrace::Trace(mut t) => entry.trace.push(t.trace.take().unwrap()),
        }
        // Drop the graph_usage lock to maximise concurrency
        drop(graph_usage);

        // This is inherently both imprecise and racy, but it doesn't matter
        // because we are just hinting to the spaceport that it's probably a
        // good idea to try to transfer data up to the ingress. Multiple
        // notifications just trigger more transfers.
        //
        // TRIGGER_BATCH_LIMIT is a fairly arbitrary number which indicates
        // that we are adding a lot of data for transfer. It is derived
        // empirically from load testing.
        let total = self.total.fetch_add(1, Ordering::SeqCst);
        if total > TRIGGER_BATCH_LIMIT {
            match self.tx.send(()).await {
                Ok(_v) => {
                    self.total.store(0, Ordering::SeqCst);
                }
                Err(e) => {
                    // Not being able to trigger a transfer isn't an "error". We can
                    // let the client know that the transfer was Ok and hope that the
                    // backend server eventually catches up with the workload and
                    // clears the incoming trigger messages.
                    tracing::warn!("could not trigger transfer: {}", e);
                }
            }
        }

        let response = ReporterResponse {
            message: "Report accepted".to_string(),
        };
        Ok(Response::new(response))
    }
}

async fn process_all_graphs(client: &Client, task_graph_usage: GraphUsageMap) {
    for result in extract_graph_usage(client, task_graph_usage).await {
        match result {
            Ok(v) => tracing::debug!("report submission succeeded: {:?}", v),
            Err(e) => tracing::error!("report submission failed: {}", e),
        }
    }
}

async fn extract_graph_usage(
    client: &Client,
    task_graph_usage: GraphUsageMap,
) -> Vec<Result<Response<ReporterResponse>, Status>> {
    let mut all_entries = task_graph_usage.lock().await;
    let drained = all_entries
        .drain()
        .filter(|(_graph, graph_usage)| !graph_usage.is_empty())
        .collect::<Vec<(ReporterGraph, QueryUsageMap)>>();
    // Release the lock ASAP so that clients can continue to add data
    drop(all_entries);
    let mut results = Vec::with_capacity(drained.len());
    for (graph, graph_usage) in drained {
        tracing::info!("submitting: {} records", graph_usage.len());
        tracing::debug!("containing: {:?}", graph_usage);
        match crate::Report::try_new(&graph.reference) {
            Ok(mut report) => {
                report.traces_per_query = graph_usage;
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

                results.push(ReportSpaceport::submit_report(client, graph.key, report).await)
            }
            Err(e) => {
                results.push(Err(Status::internal(e.to_string())));
                continue;
            }
        }
    }
    results
}
