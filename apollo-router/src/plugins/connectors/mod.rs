pub(crate) mod configuration;
mod form_encoding;
pub(crate) mod handle_responses;
pub(crate) mod http_json_transport;
pub(crate) mod incompatible;
pub(crate) mod make_requests;
pub(crate) mod mapping;
pub(crate) mod plugin;
pub(crate) mod query_plans;
pub(crate) mod request_limit;
pub(crate) mod tracing;

#[cfg(test)]
pub(crate) mod tests;
