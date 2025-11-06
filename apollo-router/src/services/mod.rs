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
pub(crate) mod connector;
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
