#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use async_trait::async_trait;
use derivative::Derivative;
use serde::Deserialize;
use serde::Serialize;
use static_assertions::assert_impl_all;

use super::layers::query_analysis::ParsedDocument;
use crate::Context;
use crate::compute_job::ComputeJobType;
use crate::compute_job::MaybeBackPressureError;
use crate::error::QueryPlannerError;
use crate::graphql;
use crate::query_planner::QueryPlan;

/// Options for planning a query
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlanOptions {
    /// Which labels to override during query planning
    pub(crate) override_conditions: Vec<String>,
}

assert_impl_all!(Request: Send);
/// [`Context`] for the request.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct Request {
    pub(crate) query: String,
    pub(crate) operation_name: Option<String>,
    pub(crate) document: ParsedDocument,
    pub(crate) metadata: crate::plugins::authorization::CacheKeyMetadata,
    pub(crate) plan_options: PlanOptions,
    pub(crate) compute_job_type: ComputeJobType,
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerRequest.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerRequest.
    #[builder]
    pub(crate) fn new(
        query: String,
        operation_name: Option<String>,
        document: ParsedDocument,
        metadata: crate::plugins::authorization::CacheKeyMetadata,
        plan_options: PlanOptions,
        compute_job_type: ComputeJobType,
    ) -> Request {
        Self {
            query,
            operation_name,
            document,
            metadata,
            plan_options,
            compute_job_type,
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
}

/// Query, QueryPlan and Introspection data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum QueryPlannerContent {
    Plan { plan: Arc<QueryPlan> },
    Response { response: Box<graphql::Response> },
    CachedIntrospectionResponse { response: Box<graphql::Response> },
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
        errors: Vec<graphql::Error>,
    ) -> Response {
        Self { content, errors }
    }
}

pub(crate) type ServiceError = MaybeBackPressureError<QueryPlannerError>;
pub(crate) type BoxService = tower::util::BoxService<Request, Response, ServiceError>;
#[allow(dead_code)]
pub(crate) type BoxCloneService = tower::util::BoxCloneService<Request, Response, ServiceError>;
#[allow(dead_code)]
pub(crate) type ServiceResult = Result<Response, ServiceError>;

#[async_trait]
pub(crate) trait QueryPlannerPlugin: Send + Sync + 'static {
    /// This service runs right after the query planner cache, which means that it will be called once per unique
    /// query, unless the cache entry was evicted
    fn query_planner_service(&self, service: BoxService) -> BoxService;
}
