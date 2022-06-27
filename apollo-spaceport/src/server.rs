// This entire file is license key functionality
use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::pin::Pin;

use bytes::BytesMut;
use flate2::write::GzEncoder;
use flate2::Compression;
use prost::Message;
use reqwest::Client;
use tokio::net::TcpListener;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::Sender;
use tokio::time::Duration;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Error;
use tonic::transport::Server;
use tonic::Request;
use tonic::Response;
use tonic::Status;

use crate::agent::reporter_server::Reporter;
use crate::agent::reporter_server::ReporterServer;
use crate::agent::ReporterRequest;
use crate::agent::ReporterResponse;
use crate::report::Report;

static DEFAULT_APOLLO_USAGE_REPORTING_INGRESS_URL: &str =
    "https://usage-reporting.api.apollographql.com/api/ingress/traces";

/// Accept Traces and Stats from clients and transfer to an Apollo Ingress
pub struct ReportSpaceport {
    shutdown_signal: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
    listener: Option<TcpListener>,
    addr: SocketAddr,
    tx: Sender<ReporterRequest>,
}

impl ReportSpaceport {
    /// Create a new ReportSpaceport which is configured to serve requests at the
    /// supplied address
    ///
    /// The spaceport will transfer reports to the Apollo Ingress.
    ///
    /// The spaceport will attempt to make the transfer 5 times before failing. If
    /// the spaceport fails, the data is discarded.
    pub async fn new(
        addr: SocketAddr,
        shutdown_signal: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;

        // Spawn a task which will transmit reports
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ReporterRequest>(1024);

        tokio::task::spawn(async move {
            let client = Client::new();
            while let Some(report) = rx.recv().await {
                if let Some(report_to_send) = report.report {
                    match ReportSpaceport::submit_report(&client, report.apollo_key, report_to_send)
                        .await
                    {
                        Ok(v) => tracing::debug!("report submission succeeded: {:?}", v),
                        Err(e) => tracing::error!("report submission failed: {}", e),
                    }
                }
            }
        });
        Ok(Self {
            shutdown_signal,
            listener: Some(listener),
            addr,
            tx,
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
        match self.tx.try_send(report) {
            Ok(()) => {
                let response = ReporterResponse {
                    message: "Report accepted".to_string(),
                };
                Ok(Response::new(response))
            }
            Err(TrySendError::Closed(_)) => Err(Status::internal("channel closed")),
            Err(TrySendError::Full(_)) => Err(Status::resource_exhausted("channel full")),
        }
    }
}
