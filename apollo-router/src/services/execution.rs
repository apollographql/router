// With regards to ELv2 licensing, this entire file is license key functionality

#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::graphql;
use crate::spec::Schema;
use crate::Context;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

// Reachable from Request
pub use crate::query_planner::QueryPlan;

assert_impl_all!(Request: Send);
#[non_exhaustive]
pub struct Request {
    /// Original request to the Router.
    pub supergraph_request: http::Request<graphql::Request>,

    pub query_plan: Arc<QueryPlan>,

    /// An apollo-compiler context that contains `self.query_plan.query`.
    ///
    /// It normally also contains type information from the schema,
    /// but might not if this `Request` was created in tests
    /// with `fake_builder()` without providing a `schema` parameter.
    #[allow(unused)] // TODO: find some uses
    pub(crate) compiler: apollo_compiler::Snapshot,

    pub context: Context,
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
    ) -> Request {
        let compiler = query_plan.query.uncached_compiler(None).snapshot();
        Self {
            supergraph_request,
            query_plan,
            compiler,
            context,
        }
    }

    #[builder(visibility = "pub(crate)")]
    #[allow(clippy::needless_lifetimes)] // needed by buildstructor-generated code
    async fn internal_new<'a>(
        supergraph_request: http::Request<graphql::Request>,
        query_plan: Arc<QueryPlan>,
        schema: &'a Schema,
        context: Context,
    ) -> Request {
        let compiler = query_plan.query.compiler(Some(schema)).await.snapshot();
        Self {
            supergraph_request,
            query_plan,
            compiler,
            context,
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
    ) -> Request {
        Request::new(
            supergraph_request.unwrap_or_default(),
            Arc::new(query_plan.unwrap_or_else(|| QueryPlan::fake_builder().build())),
            context.unwrap_or_default(),
        )
    }
}

/// The response type for execution services is the same as for supergraph services.
pub type Response = super::supergraph::Response;
