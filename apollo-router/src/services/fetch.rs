//! Fetch request and response types.

use std::sync::Arc;

use serde_json_bytes::json;
use serde_json_bytes::Value;
use tokio::sync::mpsc;
use tower::BoxError;

use crate::error::Error;
use crate::error::FetchError;
use crate::graphql::Request as GraphQLRequest;
use crate::json_ext::Path;
use crate::plugins::subscription::SubscriptionConfig;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::Variables;
use crate::query_planner::subscription::SubscriptionHandle;
use crate::query_planner::subscription::SubscriptionNode;
use crate::Context;

const FETCH_SUBGRAPH_NAME_EXTENSION_KEY: &str = "fetch_subgraph_name";

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

pub(crate) enum Request {
    Fetch(FetchRequest),
    Subscription(SubscriptionRequest),
}

pub(crate) type Response = (Value, Vec<Error>);

#[non_exhaustive]
pub(crate) struct FetchRequest {
    pub(crate) context: Context,
    pub(crate) fetch_node: FetchNode,
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
    pub(crate) current_dir: Path,
}

#[buildstructor::buildstructor]
impl FetchRequest {
    /// This is the constructor (or builder) to use when constructing a fetch Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
    fn new(
        context: Context,
        fetch_node: FetchNode,
        supergraph_request: Arc<http::Request<GraphQLRequest>>,
        variables: Variables,
        current_dir: Path,
    ) -> Self {
        Self {
            context,
            fetch_node,
            supergraph_request,
            variables,
            current_dir,
        }
    }
}

pub(crate) struct SubscriptionRequest {
    pub(crate) context: Context,
    pub(crate) subscription_node: SubscriptionNode,
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
    pub(crate) current_dir: Path,
    pub(crate) sender: mpsc::Sender<crate::graphql::Response>,
    pub(crate) subscription_handle: Option<SubscriptionHandle>,
    pub(crate) subscription_config: Option<SubscriptionConfig>,
}

#[buildstructor::buildstructor]
impl SubscriptionRequest {
    /// This is the constructor (or builder) to use when constructing a subscription Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
    fn new(
        context: Context,
        subscription_node: SubscriptionNode,
        supergraph_request: Arc<http::Request<GraphQLRequest>>,
        variables: Variables,
        current_dir: Path,
        sender: mpsc::Sender<crate::graphql::Response>,
        subscription_handle: Option<SubscriptionHandle>,
        subscription_config: Option<SubscriptionConfig>,
    ) -> Self {
        Self {
            context,
            subscription_node,
            supergraph_request,
            variables,
            current_dir,
            sender,
            subscription_handle,
            subscription_config,
        }
    }
}

/// Map a fetch error result to a [GraphQL error](GraphQLError).
pub(crate) trait ErrorMapping<T> {
    fn map_to_graphql_error(self, service_name: String, current_dir: &Path) -> Result<T, Error>;
}

impl<T> ErrorMapping<T> for Result<T, BoxError> {
    fn map_to_graphql_error(self, service_name: String, current_dir: &Path) -> Result<T, Error> {
        // TODO this is a problem since it restores details about failed service
        //  when errors have been redacted in the include_subgraph_errors module.
        //  Unfortunately, not easy to fix here, because at this point we don't
        //  know if we should be redacting errors for this subgraph...
        self.map_err(|e| match e.downcast::<FetchError>() {
            Ok(inner) => match *inner {
                FetchError::SubrequestHttpError { .. } => *inner,
                _ => FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service_name,
                    reason: inner.to_string(),
                },
            },
            Err(e) => FetchError::SubrequestHttpError {
                status_code: None,
                service: service_name,
                reason: e.to_string(),
            },
        })
        .map_err(|e| e.to_graphql_error(Some(current_dir.to_owned())))
    }
}

/// Extension trait for adding a subgraph name associated with an error.
pub(crate) trait AddSubgraphNameExt {
    /// Add the subgraph name associated with an error
    fn add_subgraph_name(self, subgraph_name: &str) -> Self;
}

impl AddSubgraphNameExt for Error {
    fn add_subgraph_name(mut self, subgraph_name: &str) -> Self {
        self.extensions
            .insert(FETCH_SUBGRAPH_NAME_EXTENSION_KEY, json!(subgraph_name));
        self
    }
}

impl<T> AddSubgraphNameExt for Result<T, Error> {
    fn add_subgraph_name(self, subgraph_name: &str) -> Self {
        self.map_err(|e| e.add_subgraph_name(subgraph_name))
    }
}

/// Extension trait for getting the subgraph name associated with an error, if any.
pub(crate) trait SubgraphNameExt {
    /// Get the subgraph name associated with an error, if any
    fn subgraph_name(&mut self) -> Option<String>;
}

impl SubgraphNameExt for Error {
    fn subgraph_name(&mut self) -> Option<String> {
        if let Some(subgraph_name) = self.extensions.remove(FETCH_SUBGRAPH_NAME_EXTENSION_KEY) {
            subgraph_name
                .as_str()
                .map(|subgraph_name| subgraph_name.to_string())
        } else {
            None
        }
    }
}
