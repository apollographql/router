pub mod report {
    tonic::include_proto!("report");
}

mod agent {
    tonic::include_proto!("agent");
}

use agent::reporter_client::ReporterClient;
use agent::ReportResponse;
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
    return "an error";
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

    pub async fn submit(&mut self, report: Report) -> Result<Response<ReportResponse>, Status> {
        self.client.send_report(Request::new(report)).await
    }
}

pub mod server {
    use super::report;
    use bytes::BytesMut;
    use prost::Message;
    use reqwest::Client;
    use serde::{Deserialize, Serialize};
    use std::net::SocketAddr;
    use tonic::transport::{Error, Server};
    use tonic::{Request, Response, Status};

    pub use crate::agent::reporter_server::{Reporter, ReporterServer};
    use crate::agent::ReportResponse;

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ApolloResponse {
        traces_ignored: bool,
    }

    pub struct ReportServer {
        addr: SocketAddr,
        client: Client,
    }

    impl ReportServer {
        pub fn new(addr: SocketAddr) -> Self {
            Self {
                addr,
                client: Client::new(),
            }
        }

        pub async fn serve(self) -> Result<(), Error> {
            let addr = self.addr;
            Server::builder()
                .add_service(ReporterServer::new(self))
                .serve(addr)
                .await
        }
    }

    #[tonic::async_trait]
    impl Reporter for ReportServer {
        async fn send_report(
            &self,
            request: Request<report::Report>,
        ) -> Result<Response<ReportResponse>, Status> {
            println!("received request: {:?}", request);
            let msg = request.into_inner();
            let mut content = BytesMut::new();
            msg.encode(&mut content)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
            let res = self
                .client
                .post("https://usage-reporting.api.apollographql.com/api/ingress/traces")
                .body(content.to_vec())
                .header(
                    "X-Api-Key",
                    std::env::var("X_API_KEY")
                        .map_err(|e| Status::unauthenticated(e.to_string()))?,
                )
                .header("Content-Type", "application/protobuf")
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(|e| Status::failed_precondition(e.to_string()))?;
            println!("result: {:?}", res);
            let ar: ApolloResponse = res
                .json()
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
            println!("json: {:?}", ar);
            let response = ReportResponse {
                message: "Report accepted".to_string(),
            };
            Ok(Response::new(response))
        }
    }
}
