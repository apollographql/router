#![allow(missing_docs)] // FIXME

use std::hash::Hash;
use std::sync::Arc;

use static_assertions::assert_impl_all;

use crate::graphql;
use crate::query_planner::QueryPlan;
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

impl Eq for Request {}

impl PartialEq for Request {
    fn eq(&self, other: &Self) -> bool {
        self.query == other.query && self.operation_name == other.operation_name
    }
}

impl Hash for Request {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.query.hash(state);
        self.operation_name.hash(state);
    }
}

assert_impl_all!(Response: Send);
/// [`Context`] and [`QueryPlan`] for the response.
#[derive(Clone, Debug)]
pub(crate) struct Response {
    /// Optional in case of error
    pub(crate) content: Option<QueryPlannerContent>,
    pub(crate) errors: Vec<graphql::Error>,
    pub(crate) context: Context,
}

/// Query, QueryPlan and Introspection data.
#[derive(Debug, Clone)]
pub(crate) enum QueryPlannerContent {
    Plan { plan: Arc<QueryPlan> },
    Introspection { response: Box<graphql::Response> },
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
