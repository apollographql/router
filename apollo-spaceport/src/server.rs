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

type TPQMap = HashMap<String, TracesAndStats>;
type GraphMap = Arc<Mutex<HashMap<ReporterGraph, TPQMap>>>;

static DEFAULT_INGRESS: &str = "https://usage-reporting.api.apollographql.com/api/ingress/traces";

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
    addr: SocketAddr,
    // This Map will only have a single entry if used internally from a router.
    // (because a router can only be serving a single graph)
    tpq: GraphMap,
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
        let tpq: GraphMap = Arc::new(Mutex::new(HashMap::new()));
        let task_tpq = tpq.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(10);
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

        for i in 0..5 {
            // We know these requests can be cloned
            let my_req = req.try_clone().expect("requests must be clone-able");
            match client.execute(my_req).await {
                Ok(v) => {
                    let data = v
                        .text()
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                    tracing::debug!("ingress response text: {:?}", data);
                    let response = ReporterResponse {
                        message: "Report accepted".to_string(),
                    };
                    return Ok(Response::new(response));
                }
                Err(e) => {
                    tracing::warn!("attempt: {}, could not transfer: {}", i + 1, e);
                    if i == 4 {
                        return Err(Status::unavailable(e.to_string()));
                    }
                    backoff += Duration::from_millis(50);
                    tokio::time::sleep(backoff).await;
                }
            }
        }
        // The compiler can't figure out the exit paths are covered,
        // so to keep it happy have...
        Err(Status::unavailable("should not happen..."))
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
        let mut tpq = self.tpq.lock().await;
        let graph_map = tpq
            .entry(record.graph().unwrap())
            .or_insert_with(HashMap::new);
        let entry = graph_map
            .entry(record.key())
            .or_insert_with(|| record.get_traces_and_stats());
        match record {
            StatsOrTrace::Stats(mut s) => entry.stats_with_context.push(s.stats.take().unwrap()),
            StatsOrTrace::Trace(mut t) => entry.trace.push(t.trace.take().unwrap()),
        }
        // Drop the tpq lock to maximise concurrency
        drop(tpq);

        // This is inherently both imprecise and racy, but it doesn't matter
        // because we are just hinting to the spaceport that it's probably a
        // good idea to try to transfer data up to the ingress. Multiple
        // notifications just trigger more transfers.
        //
        // 5000 is a fairly arbitrary number which indicates that we are adding
        // a lot of data for transfer. It is intended to represent a rate of
        // approx. 1,000 records/second.
        let total = self.total.fetch_add(1, Ordering::SeqCst);
        if total > 5000 {
            let mut backoff = Duration::from_millis(0);
            for i in 0..5 {
                match self.tx.send(()).await {
                    Ok(_v) => {
                        self.total.store(0, Ordering::SeqCst);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("attempt: {}, could not trigger transfer: {}", i + 1, e);
                        if i == 4 {
                            // Not being able to trigger a transfer isn't an "error". We can
                            // let the client know that the transfer was Ok and hope that the
                            // backend server eventually catches up with the workload and
                            // clears the incoming trigger messages.
                            break;
                        } else {
                            backoff += Duration::from_millis(50);
                            tokio::time::sleep(backoff).await;
                        }
                    }
                }
            }
        }

        let response = ReporterResponse {
            message: "Report accepted".to_string(),
        };
        Ok(Response::new(response))
    }
}

async fn extract_tpq(
    client: &Client,
    task_tpq: GraphMap,
) -> Vec<Result<Response<ReporterResponse>, Status>> {
    let mut all_entries = task_tpq.lock().await;
    let drained = all_entries
        .drain()
        .filter(|(_graph, tpq)| !tpq.is_empty())
        .collect::<Vec<(ReporterGraph, TPQMap)>>();
    // Release the lock ASAP so that clients can continue to add data
    drop(all_entries);
    let mut results = Vec::with_capacity(drained.len());
    for (graph, tpq) in drained {
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
