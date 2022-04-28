pub mod report {
    tonic::include_proto!("report");
}

mod agent {
    tonic::include_proto!("agent");
}

/// The server module contains the server components
pub mod server;

use agent::reporter_client::ReporterClient;
pub use agent::ReporterGraph;
use agent::{ReporterResponse, ReporterStats, ReporterTrace};
pub use report::*;
use std::collections::HashMap;
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
        header.agent_version = format!(
            "{}@{}",
            std::env!("CARGO_PKG_NAME"),
            std::env!("CARGO_PKG_VERSION")
        );
        header.runtime_version = "rust".to_string();
        header.uname = get_uname()?;
        header.graph_ref = graph.to_string();
        // TODO: The executable_schema_id field is missing. Fine for
        // now but will need to be addressed at some point.
        Ok(header)
    }
}

/// The Reporter accepts requests from clients to transfer statistics
/// and traces to the Apollo spaceport.
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
        fields: HashMap<String, ReferencedFieldsForType>,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client
            .add_stats(Request::new(ReporterStats {
                graph: Some(graph),
                key,
                stats: Some(stats),
                fields,
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
        fields: HashMap<String, ReferencedFieldsForType>,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client
            .add_trace(Request::new(ReporterTrace {
                graph: Some(graph),
                key,
                trace: Some(trace),
                fields,
            }))
            .await
    }
}
