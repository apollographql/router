//! Fetch request and response types.

use std::sync::Arc;

use apollo_federation::query_plan::SubscriptionNode;
use serde_json_bytes::Value;
use tower::BoxError;

use crate::error::Error;
use crate::error::FetchError;
use crate::graphql::Request as GraphQLRequest;
use crate::json_ext::Path;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::Variables;
use crate::Context;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

pub(crate) enum Request {
    Fetch(FetchRequest),
    #[allow(dead_code)]
    Subscription(SubscriptionRequest),
}

#[non_exhaustive]
pub(crate) struct FetchRequest {
    pub(crate) context: Context,
    pub(crate) fetch_node: FetchNode,
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
    pub(crate) current_dir: Path,
}

#[allow(dead_code)]
pub(crate) struct SubscriptionRequest {
    pub(crate) context: Context,
    pub(crate) subscription_node: SubscriptionNode,
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
    pub(crate) current_dir: Path,
}

pub(crate) type Response = (Value, Vec<Error>);

#[buildstructor::buildstructor]
impl FetchRequest {
    /// This is the constructor (or builder) to use when constructing a real Request.
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
