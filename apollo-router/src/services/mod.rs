//! Implementation of the various steps in the router's processing pipeline.

use std::sync::Arc;

use parking_lot::Mutex;

pub(crate) use self::execution::service::*;
pub(crate) use self::query_planner::*;
pub(crate) use self::subgraph_service::*;
pub(crate) use self::supergraph::service::*;
use crate::graphql::Request;
use crate::http_ext;
pub use crate::http_ext::TryIntoHeaderName;
pub use crate::http_ext::TryIntoHeaderValue;
pub use crate::query_planner::OperationKind;
pub(crate) use crate::services::connect::Request as ConnectRequest;
pub(crate) use crate::services::connect::Response as ConnectResponse;
pub(crate) use crate::services::execution::Request as ExecutionRequest;
pub(crate) use crate::services::execution::Response as ExecutionResponse;
pub(crate) use crate::services::fetch::FetchRequest;
pub(crate) use crate::services::fetch::Response as FetchResponse;
pub(crate) use crate::services::query_planner::Request as QueryPlannerRequest;
pub(crate) use crate::services::query_planner::Response as QueryPlannerResponse;
pub(crate) use crate::services::router::Request as RouterRequest;
pub(crate) use crate::services::router::Response as RouterResponse;
pub(crate) use crate::services::subgraph::Request as SubgraphRequest;
pub(crate) use crate::services::subgraph::Response as SubgraphResponse;
pub(crate) use crate::services::supergraph::Request as SupergraphRequest;
pub(crate) use crate::services::supergraph::Response as SupergraphResponse;
pub(crate) use crate::services::supergraph::service::SupergraphCreator;

pub(crate) mod connect;
/// Services for Apollo Connectors.
pub mod connector;
pub(crate) mod connector_service;
pub mod execution;
pub(crate) mod external;
pub(crate) mod fetch;
pub(crate) mod fetch_service;
pub(crate) mod hickory_dns_connector;
pub(crate) mod http;
pub(crate) mod layers;
pub(crate) mod new_service;
pub(crate) mod query_planner;
pub mod router;
pub mod subgraph;
pub(crate) mod subgraph_service;
pub mod supergraph;

impl AsRef<Request> for http_ext::Request<Request> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

impl AsRef<Request> for Arc<http_ext::Request<Request>> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

// Public-hidden for tests
#[allow(missing_docs)]
pub static APOLLO_KEY: Mutex<Option<String>> = Mutex::new(None);
#[allow(missing_docs)]
pub static APOLLO_GRAPH_REF: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn apollo_key() -> Option<String> {
    APOLLO_KEY.lock().clone()
}

pub(crate) fn apollo_graph_reference() -> Option<String> {
    APOLLO_GRAPH_REF.lock().clone()
}

// set the supported `@defer` specification version to https://github.com/graphql/graphql-spec/pull/742/commits/01d7b98f04810c9a9db4c0e53d3c4d54dbf10b82
pub(crate) const MULTIPART_DEFER_SPEC_PARAMETER: &str = "deferSpec";
pub(crate) const MULTIPART_DEFER_SPEC_VALUE: &str = "20220824";
pub(crate) const MULTIPART_DEFER_ACCEPT: &str = "multipart/mixed;deferSpec=20220824";
pub(crate) const MULTIPART_DEFER_CONTENT_TYPE: &str =
    "multipart/mixed;boundary=\"graphql\";deferSpec=20220824";

pub(crate) const MULTIPART_SUBSCRIPTION_ACCEPT: &str = "multipart/mixed;subscriptionSpec=1.0";
pub(crate) const MULTIPART_SUBSCRIPTION_CONTENT_TYPE: &str =
    "multipart/mixed;boundary=\"graphql\";subscriptionSpec=1.0";
pub(crate) const MULTIPART_SUBSCRIPTION_SPEC_PARAMETER: &str = "subscriptionSpec";
pub(crate) const MULTIPART_SUBSCRIPTION_SPEC_VALUE: &str = "1.0";

#[cfg(unix)]
pub(crate) const DEFAULT_SOCKET_PATH: &str = "/";
pub(crate) const PATH_QUERY_PARAM: &str = "path=";

/// Parse a Unix socket URL path (the part after `unix://`) and extract the socket path
/// and HTTP path (if provided). Supports an optional `path` query parameter to specify the HTTP path.
///
/// Examples:
/// - `/tmp/socket.sock` -> (`/tmp/socket.sock`, `/`)
/// - `/tmp/socket.sock?path=/api/v1` -> (`/tmp/socket.sock`, `/api/v1`)
///
/// Requires:
/// - when using query params, the param must be denoted by `?path=`
#[cfg(unix)]
pub(crate) fn parse_unix_socket_url(url_path: &str) -> (&str, &str) {
    if let Some(query_start) = url_path.find('?') {
        let socket_path = &url_path[..query_start];
        let query = &url_path[query_start + 1..];

        // Parse the `path` parameter from the query string
        let http_path = query
            .split('&')
            .find_map(|param| param.strip_prefix(PATH_QUERY_PARAM))
            .unwrap_or(DEFAULT_SOCKET_PATH);

        (socket_path, http_path)
    } else {
        (url_path, DEFAULT_SOCKET_PATH)
    }
}

#[cfg(unix)]
#[cfg(test)]
mod unix_socket_url_tests {
    use rstest::rstest;

    use super::parse_unix_socket_url;

    #[rstest]
    #[case::without_query("/tmp/coprocessor.sock", "/tmp/coprocessor.sock", "/")]
    #[case::with_path_param(
        "/tmp/coprocessor.sock?path=/api/v1",
        "/tmp/coprocessor.sock",
        "/api/v1"
    )]
    #[case::with_multiple_params(
        "/tmp/coprocessor.sock?other=value&path=/api/v1&another=x",
        "/tmp/coprocessor.sock",
        "/api/v1"
    )]
    #[case::with_other_params_only(
        "/tmp/coprocessor.sock?other=value",
        "/tmp/coprocessor.sock",
        "/"
    )]
    #[case::with_empty_query("/tmp/coprocessor.sock?", "/tmp/coprocessor.sock", "/")]
    #[case::with_nested_http_path(
        "/tmp/coprocessor.sock?path=/api/v1/coprocessor/hook",
        "/tmp/coprocessor.sock",
        "/api/v1/coprocessor/hook"
    )]
    #[case::with_empty_path_param("/tmp/coprocessor.sock?path", "/tmp/coprocessor.sock", "/")]
    #[case::without_leading_slash(
        "/tmp/coprocessor.sock?path=no_leading_slash",
        "/tmp/coprocessor.sock",
        "no_leading_slash"
    )]
    fn parse_socket_url(
        #[case] input: &str,
        #[case] expected_socket: &str,
        #[case] expected_http_path: &str,
    ) {
        let (socket, http_path) = parse_unix_socket_url(input);
        assert_eq!(socket, expected_socket);
        assert_eq!(http_path, expected_http_path);
    }
}
