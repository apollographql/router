// This entire file is license key functionality
use crate::{
    agent::{
        reporter_server::{Reporter, ReporterServer},
        ReporterRequest, ReporterResponse,
    },
    report::Report,
};
use bytes::BytesMut;
use flate2::write::GzEncoder;
use flate2::Compression;
use prost::Message;
use reqwest::Client;
use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration, MissedTickBehavior};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Error, Server};
use tonic::{Request, Response, Status};

type QueuedReports = Arc<Mutex<Vec<ReporterRequest>>>;

static DEFAULT_APOLLO_USAGE_REPORTING_INGRESS_URL: &str =
    "https://usage-reporting.api.apollographql.com/api/ingress/traces";
static INGRESS_CLOCK_TICK: Duration = Duration::from_secs(5);
static TRIGGER_BATCH_LIMIT: u32 = 50; // TODO: arbitrary but it seems to work :D

/// Accept Traces and Stats from clients and transfer to an Apollo Ingress
pub struct ReportSpaceport {
    shutdown_signal: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
    listener: Option<TcpListener>,
    addr: SocketAddr,
    // This vec will contains all queued reports to send.
    // Previously we were doing some aggregation on metrics in Spaceport, but for now this has been removed.
    // Each report potentially contains details about the machine that the report came from, so we would
    // have to think about if aggregation is really appropriate at the Spaceport level.
    queued_reports: QueuedReports,
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
        let queued_reports: QueuedReports = Arc::new(Mutex::new(Vec::new()));
        let task_queued_reports = queued_reports.clone();
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
                            Some(_msg) => process_all_reports(&client, task_queued_reports.clone()).await,
                            None => break
                        }
                    },
                    _ = interval.tick() => {
                        tracing::trace!("spaceport ticked");
                        task_total.store(0, Ordering::SeqCst);
                        process_all_reports(&client, task_queued_reports.clone()).await;
                    }
                };
            }
        });
        Ok(Self {
            shutdown_signal,
            listener: Some(listener),
            addr,
            queued_reports: queued_reports,
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
    async fn add(
        &self,
        request: Request<ReporterRequest>,
    ) -> Result<Response<ReporterResponse>, Status> {
        tracing::debug!("received request: {:?}", request);
        let msg = request.into_inner();
        self.add_report(msg).await
    }
}

impl ReportSpaceport {
    async fn add_report(
        &self,
        report: ReporterRequest,
    ) -> Result<Response<ReporterResponse>, Status> {
        let mut queued_reports = self.queued_reports.lock().await;
        queued_reports.push(report);
        // Drop the graph_usage lock to maximise concurrency
        drop(queued_reports);

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

async fn process_all_reports(client: &Client, queued_reports: QueuedReports) {
    for result in process_reports(client, queued_reports).await {
        match result {
            Ok(v) => tracing::debug!("report submission succeeded: {:?}", v),
            Err(e) => tracing::error!("report submission failed: {}", e),
        }
    }
}

async fn process_reports(
    client: &Client,
    queued_reports: QueuedReports,
) -> Vec<Result<Response<ReporterResponse>, Status>> {
    let mut all_entries = queued_reports.lock().await;
    let drained = std::mem::replace(&mut *all_entries, Vec::new());
    // Release the lock ASAP so that clients can continue to add data
    drop(all_entries);
    let mut results = Vec::with_capacity(drained.len());
    tracing::debug!("submitting: {} reports", drained.len());
    for report in drained {
        if let Some(report_to_send) = report.report {
            results.push(
                ReportSpaceport::submit_report(client, report.apollo_key, report_to_send).await,
            )
        } else {
            results.push(Err(Status::internal("missing report")));
        }
    }
    results
}
