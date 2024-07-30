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
