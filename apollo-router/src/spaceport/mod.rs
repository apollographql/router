// With regards to ELv2 licensing, this entire file is license key functionality
// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

#[allow(unreachable_pub)]
mod report {
    tonic::include_proto!("report");
}

#[allow(unreachable_pub)]
mod agent {
    tonic::include_proto!("agent");
}

/// The server module contains the server components
pub(crate) mod server;

use std::error::Error;

use agent::reporter_client::ReporterClient;
pub(crate) use agent::*;
pub(crate) use prost::*;
pub(crate) use report::*;
use serde::ser::SerializeStruct;
use tokio::task::JoinError;
use tonic::codegen::http::uri::InvalidUri;
use tonic::transport::Channel;
use tonic::transport::Endpoint;
use tonic::Request;
use tonic::Response;
use tonic::Status;

/// Reporting Error type
#[derive(Debug)]
pub(crate) struct ReporterError {
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

/// The Reporter accepts requests from clients to transfer statistics
/// and traces to the Apollo spaceport.
#[derive(Debug)]
pub(crate) struct Reporter {
    client: ReporterClient<Channel>,
    ep: Endpoint,
}

impl Reporter {
    /// Try to create a new reporter which will communicate with the supplied address.
    ///
    /// This can fail if:
    ///  - the address cannot be parsed
    ///  - the reporter can't connect to the address
    pub(crate) async fn try_new<T: AsRef<str>>(addr: T) -> Result<Self, ReporterError>
    where
        prost::bytes::Bytes: From<T>,
    {
        let ep = Endpoint::from_shared(addr)?;
        let client = ReporterClient::connect(ep.clone()).await?;
        Ok(Self { client, ep })
    }

    /// Try to re-connect a reporter.
    ///
    /// This can fail if:
    ///  - the reporter can't connect to the address
    pub(crate) async fn reconnect(&mut self) -> Result<(), ReporterError> {
        self.client = ReporterClient::connect(self.ep.clone()).await?;
        Ok(())
    }

    /// Submit a report onto the spaceport for eventual processing.
    ///
    /// The spaceport will buffer reports, transferring them when convenient.
    pub(crate) async fn submit(
        &mut self,
        request: ReporterRequest,
    ) -> Result<Response<ReporterResponse>, Status> {
        self.client.add(Request::new(request)).await
    }
}

pub(crate) fn serialize_timestamp<S>(
    timestamp: &Option<prost_types::Timestamp>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match timestamp {
        Some(ts) => {
            let mut ts_strukt = serializer.serialize_struct("Timestamp", 2)?;
            ts_strukt.serialize_field("seconds", &ts.seconds)?;
            ts_strukt.serialize_field("nanos", &ts.nanos)?;
            ts_strukt.end()
        }
        None => serializer.serialize_none(),
    }
}

#[cfg(not(windows))] // git checkout converts \n to \r\n, making == below fail
#[test]
fn check_reports_proto_is_up_to_date() {
    let proto_url = "https://usage-reporting.api.apollographql.com/proto/reports.proto";
    let response = reqwest::blocking::get(proto_url).unwrap();
    let content = response.text().unwrap();
    // Not using assert_eq! as printing the entire file would be too verbose
    assert!(
        content == include_str!("proto/reports.proto"),
        "Protobuf file is out of date. Run this command to update it:\n\n    \
            curl -f {proto_url} > apollo-router/src/spaceport/proto/reports.proto\n\n"
    );
}
