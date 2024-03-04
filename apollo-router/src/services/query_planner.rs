#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use async_trait::async_trait;
use derivative::Derivative;
use serde::Deserialize;
use serde::Serialize;
use static_assertions::assert_impl_all;

use crate::error::QueryPlannerError;
use crate::graphql;
use crate::query_planner::QueryPlan;
use crate::Context;

assert_impl_all!(Request: Send);
/// [`Context`] for the request.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct Request {
    pub(crate) query: String,
    pub(crate) operation_name: Option<String>,
    pub(crate) context: Context,
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerRequest.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerRequest.
    #[builder]
    pub(crate) fn new(query: String, operation_name: Option<String>, context: Context) -> Request {
        Self {
            query,
            operation_name,
            context,
        }
    }
}

/// [`Context`] for the request.
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub(crate) struct CachingRequest {
    pub(crate) query: String,
    pub(crate) operation_name: Option<String>,
    pub(crate) context: Context,
}

#[buildstructor::buildstructor]
impl CachingRequest {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerRequest.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerRequest.
    #[builder]
    pub(crate) fn new(
        query: String,
        operation_name: Option<String>,
        context: Context,
    ) -> CachingRequest {
        Self {
            query,
            operation_name,
            context,
        }
    }
}

assert_impl_all!(Response: Send);
/// [`Context`] and [`QueryPlan`] for the response.
pub(crate) struct Response {
    /// Optional in case of error
    pub(crate) content: Option<QueryPlannerContent>,
    pub(crate) errors: Vec<graphql::Error>,
    pub(crate) context: Context,
}

/// Query, QueryPlan and Introspection data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum QueryPlannerContent {
    Plan { plan: Arc<QueryPlan> },
    Response { response: Box<graphql::Response> },
    IntrospectionDisabled,
}

#[buildstructor::buildstructor]
impl Response {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerResponse.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerResponse.
    #[builder]
    pub(crate) fn new(
        content: Option<QueryPlannerContent>,
        context: Context,
        errors: Vec<graphql::Error>,
    ) -> Response {
        Self {
            content,
            context,
            errors,
        }
    }
}

pub(crate) type BoxService = tower::util::BoxService<Request, Response, QueryPlannerError>;
#[allow(dead_code)]
pub(crate) type BoxCloneService =
    tower::util::BoxCloneService<Request, Response, QueryPlannerError>;
#[allow(dead_code)]
pub(crate) type ServiceResult = Result<Response, QueryPlannerError>;
#[allow(dead_code)]
pub(crate) type Body = hyper::Body;
#[allow(dead_code)]
pub(crate) type Error = hyper::Error;

#[async_trait]
pub(crate) trait QueryPlannerPlugin: Send + Sync + 'static {
    /// This service runs right after the query planner cache, which means that it will be called once per unique
    /// query, unless the cache entry was evicted
    fn query_planner_service(&self, service: BoxService) -> BoxService;
}
