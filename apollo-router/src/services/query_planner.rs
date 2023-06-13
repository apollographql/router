#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use apollo_compiler::ApolloCompiler;
use derivative::Derivative;
use serde::Deserialize;
use serde::Serialize;
use static_assertions::assert_impl_all;
use tokio::sync::Mutex;

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
    #[derivative(Debug = "ignore")]
    pub(crate) compiler: Arc<Mutex<ApolloCompiler>>,
    pub(crate) context: Context,
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
        compiler: Arc<Mutex<ApolloCompiler>>,
        context: Context,
    ) -> Request {
        Self {
            query,
            operation_name,
            compiler,
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
