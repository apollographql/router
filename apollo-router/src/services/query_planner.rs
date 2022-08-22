#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use static_assertions::assert_impl_all;

use crate::graphql;
use crate::query_planner::QueryPlan;
use crate::spec::Query;
use crate::Context;

assert_impl_all!(Request: Send);
/// [`Context`] for the request.
#[derive(Clone, Debug)]
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

assert_impl_all!(Response: Send);
/// [`Context`] and [`QueryPlan`] for the response.
pub(crate) struct Response {
    pub(crate) content: QueryPlannerContent,
    pub(crate) context: Context,
}

/// Query, QueryPlan and Introspection data.
#[derive(Debug, Clone)]
pub enum QueryPlannerContent {
    Plan {
        query: Arc<Query>,
        plan: Arc<QueryPlan>,
    },
    Introspection {
        response: Box<graphql::Response>,
    },
    IntrospectionDisabled,
}

#[buildstructor::buildstructor]
impl Response {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerResponse.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerResponse.
    #[builder]
    pub(crate) fn new(content: QueryPlannerContent, context: Context) -> Response {
        Self { content, context }
    }
}
