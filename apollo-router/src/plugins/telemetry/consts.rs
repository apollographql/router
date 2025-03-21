pub(crate) const OTEL_NAME: &str = "otel.name";
pub(crate) const OTEL_ORIGINAL_NAME: &str = "otel.original_name";
pub(crate) const OTEL_KIND: &str = "otel.kind";
pub(crate) const OTEL_STATUS_CODE: &str = "otel.status_code";
pub(crate) const OTEL_STATUS_MESSAGE: &str = "otel.status_message";
#[allow(dead_code)]
pub(crate) const OTEL_STATUS_DESCRIPTION: &str = "otel.status_description";
pub(crate) const OTEL_STATUS_CODE_OK: &str = "OK";
pub(crate) const OTEL_STATUS_CODE_ERROR: &str = "ERROR";

pub(crate) const FIELD_EXCEPTION_MESSAGE: &str = "exception.message";
pub(crate) const FIELD_EXCEPTION_STACKTRACE: &str = "exception.stacktrace";
pub(crate) const SUPERGRAPH_SPAN_NAME: &str = "supergraph";
pub(crate) const SUBGRAPH_SPAN_NAME: &str = "subgraph";
pub(crate) const ROUTER_SPAN_NAME: &str = "router";
pub(crate) const EXECUTION_SPAN_NAME: &str = "execution";
pub(crate) const REQUEST_SPAN_NAME: &str = "request";
pub(crate) const QUERY_PLANNING_SPAN_NAME: &str = "query_planning";
pub(crate) const HTTP_REQUEST_SPAN_NAME: &str = "http_request";
pub(crate) const SUBGRAPH_REQUEST_SPAN_NAME: &str = "subgraph_request";
pub(crate) const QUERY_PARSING_SPAN_NAME: &str = "parse_query";
pub(crate) const CACHE_LOOKUP_SPAN_NAME: &str = "cache_lookup";
pub(crate) const CACHING_QUERY_PLANNER_SPAN_NAME: &str = "caching_query_planner_wrapper";
pub(crate) const QUERY_PLANNER_POOL_SPAN_NAME: &str = "parallelism_pool";
pub(crate) const BRIDGE_QUERY_PLANNER_PLAN_SPAN_NAME: &str = "plan";
pub(crate) const BRIDGE_QUERY_PLANNER_WORKER_POOL_SPAN_NAME: &str = "worker_pool";
pub(crate) const BRIDGE_QUERY_PLANNER_CALL_SPAN_NAME: &str = "invoke_planner";
pub(crate) const WAITING_TO_RECEIVE_CACHE_SPAN_NAME: &str = "waiting_for_cache";

pub(crate) const BUILT_IN_SPAN_NAMES: [&str; 9] = [
    REQUEST_SPAN_NAME,
    ROUTER_SPAN_NAME,
    SUPERGRAPH_SPAN_NAME,
    SUBGRAPH_SPAN_NAME,
    SUBGRAPH_REQUEST_SPAN_NAME,
    HTTP_REQUEST_SPAN_NAME,
    QUERY_PLANNING_SPAN_NAME,
    EXECUTION_SPAN_NAME,
    QUERY_PARSING_SPAN_NAME,
];
