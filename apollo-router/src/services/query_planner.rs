#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;
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

assert_impl_all!(Response: Send);
/// [`Context`] and [`QueryPlan`] for the response.
pub(crate) struct Response {
    /// Optional in case of error
    pub(crate) content: Option<QueryPlannerContent>,
    pub(crate) errors: Vec<graphql::Error>,
    pub(crate) context: Context,
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

/// Query, QueryPlan and Introspection data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum QueryPlannerContent {
    Plan { plan: Arc<QueryPlan> },
    Introspection { response: Box<graphql::Response> },
    IntrospectionDisabled,
}

impl QueryPlannerContent{
    pub(crate) fn add_apq_hash(&self) -> Option<QueryPlannerContent> {
        if let &QueryPlannerContent::Plan { plan: query_plan } = &self {
            let QueryPlan {
                usage_reporting,
                root: node,
                formatted_query_plan,
                query,
                options,
            } = query_plan.as_ref();
            let node_with_hashes = node.add_apq_hash();
            return Some(QueryPlannerContent::Plan {
                plan: Arc::new(QueryPlan {
                    usage_reporting: usage_reporting.clone(),
                    root: node_with_hashes,
                    formatted_query_plan: formatted_query_plan.clone(),
                    query: query.clone(),
                    options: options.clone(),
                })
            });
        }
        None
    }
}
