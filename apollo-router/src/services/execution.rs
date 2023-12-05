#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use serde_json_bytes::Value;
use static_assertions::assert_impl_all;
use tokio::sync::mpsc;
use tower::BoxError;

use crate::graphql;
use crate::Context;

pub(crate) mod service;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

// Reachable from Request
use super::SubscriptionTaskParams;
pub use crate::query_planner::QueryPlan;

assert_impl_all!(Request: Send);
#[non_exhaustive]
pub struct Request {
    /// Original request to the Router.
    pub supergraph_request: http::Request<graphql::Request>,

    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
    /// Initial data coming from subscription event if it's a subscription
    pub(crate) source_stream_value: Option<Value>,
    /// Channel to send all parameters needed for the subscription
    pub(crate) subscription_tx: Option<mpsc::Sender<SubscriptionTaskParams>>,
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real ExecutionRequest.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a ExecutionRequest.
    #[builder(visibility = "pub")]
    fn new(
        supergraph_request: http::Request<graphql::Request>,
        query_plan: Arc<QueryPlan>,
        context: Context,
        source_stream_value: Option<Value>,
        subscription_tx: Option<mpsc::Sender<SubscriptionTaskParams>>,
    ) -> Request {
        Self {
            supergraph_request,
            query_plan,
            context,
            source_stream_value,
            subscription_tx,
        }
    }

    #[builder(visibility = "pub(crate)")]
    #[allow(clippy::needless_lifetimes)] // needed by buildstructor-generated code
    async fn internal_new(
        supergraph_request: http::Request<graphql::Request>,
        query_plan: Arc<QueryPlan>,
        context: Context,
        source_stream_value: Option<Value>,
        subscription_tx: Option<mpsc::Sender<SubscriptionTaskParams>>,
    ) -> Request {
        Self {
            supergraph_request,
            query_plan,
            context,
            source_stream_value,
            subscription_tx,
        }
    }

    /// This is the constructor (or builder) to use when constructing a "fake" ExecutionRequest.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// ExecutionRequest. It's usually enough for testing, when a fully consructed ExecutionRequest is
    /// difficult to construct and not required for the pusposes of the test.
    #[builder(visibility = "pub")]
    fn fake_new(
        supergraph_request: Option<http::Request<graphql::Request>>,
        query_plan: Option<QueryPlan>,
        context: Option<Context>,
        source_stream_value: Option<Value>,
        subscription_tx: Option<mpsc::Sender<SubscriptionTaskParams>>,
    ) -> Request {
        Request::new(
            supergraph_request.unwrap_or_default(),
            Arc::new(query_plan.unwrap_or_else(|| QueryPlan::fake_builder().build())),
            context.unwrap_or_default(),
            source_stream_value,
            subscription_tx,
        )
    }
}

/// The response type for execution services is the same as for supergraph services.
pub type Response = super::supergraph::Response;
