//! Implementation of the various steps in the router's processing pipeline.

use std::sync::Arc;

use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum::Display;
use tower::BoxError;

pub(crate) use self::query_planner::*;
pub(crate) use self::subgraph::service::*;
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
pub(crate) mod connector;
pub(crate) mod connector_service;
pub mod execution;
pub(crate) mod external;
pub(crate) mod fetch;
pub(crate) mod fetch_service;
pub(crate) mod header_masking;
pub(crate) mod hickory_dns_connector;
pub(crate) mod http;
pub(crate) mod layers;
pub(crate) mod query_parsing;
pub(crate) mod query_planner;
pub mod router;
pub mod subgraph;
pub mod supergraph;

/// Represents the steps of the pipeline that can support user-extensibility.
#[derive(Clone, Debug, Display, Deserialize, PartialEq, Serialize, JsonSchema)]
pub(crate) enum PipelineStep {
    RouterRequest,
    RouterResponse,
    SupergraphRequest,
    SupergraphResponse,
    ExecutionRequest,
    ExecutionResponse,
    SubgraphRequest,
    SubgraphResponse,
    ConnectorRequest,
    ConnectorResponse,
}

impl From<PipelineStep> for opentelemetry::Value {
    fn from(val: PipelineStep) -> Self {
        val.to_string().into()
    }
}

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

/// How a caller permits query parameters on an external Unix-socket URL.
#[derive(Clone, Copy)]
pub(crate) enum UnixSocketQueryPolicy {
    /// Preserve the historically permissive coprocessor query handling.
    Any,
    /// Accept no query, or exactly one non-empty absolute `path` parameter.
    OptionalAbsolutePath,
}

/// Parse and validate a URL for an external HTTP service.
///
/// This centralizes the common HTTP(S)/Unix scheme and absolute-socket-path checks while allowing
/// callers to choose whether ordinary HTTP queries and legacy Unix queries are accepted.
pub(crate) fn validate_external_service_url(
    url: &str,
    config_path: &str,
    allow_http_query: bool,
    unix_query_policy: UnixSocketQueryPolicy,
) -> Result<reqwest::Url, BoxError> {
    if url == "unix://" {
        return Err(format!("{config_path}: Unix socket URL must include a path").into());
    }
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| format!("{config_path}: invalid URL `{url}`: {error}"))?;
    if parsed.fragment().is_some() {
        return Err(format!("{config_path}: URL must not contain a fragment").into());
    }

    match parsed.scheme() {
        "http" | "https" => {
            if !allow_http_query && parsed.query().is_some() {
                return Err(format!("{config_path}: URL must not contain a query").into());
            }
        }
        "unix" => {
            if parsed.host_str().is_some() || !parsed.path().starts_with('/') {
                return Err(format!(
                    "{config_path}: Unix socket path should be absolute (for example, `unix:///var/run/service.sock`)"
                )
                .into());
            }
            if parsed.path() == "/" {
                return Err(format!("{config_path}: Unix socket URL must include a path").into());
            }
            if matches!(
                unix_query_policy,
                UnixSocketQueryPolicy::OptionalAbsolutePath
            ) && parsed.query().is_some()
            {
                let pairs = parsed.query_pairs().collect::<Vec<_>>();
                if pairs.len() != 1
                    || pairs[0].0 != "path"
                    || pairs[0].1.is_empty()
                    || !pairs[0].1.starts_with('/')
                {
                    return Err(format!(
                        "{config_path}: Unix socket query must contain exactly one absolute `path` parameter"
                    )
                    .into());
                }
            }
        }
        scheme => {
            return Err(format!(
                "{config_path}: URL must use http, https, or unix, not `{scheme}`"
            )
            .into());
        }
    }

    Ok(parsed)
}

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
