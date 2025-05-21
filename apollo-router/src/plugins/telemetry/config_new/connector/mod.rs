//! Connectors telemetry.

pub(crate) mod attributes;
pub(crate) mod events;
pub(crate) mod instruments;
pub(crate) mod selectors;
pub(crate) mod spans;

pub(crate) type ConnectorRequest = crate::services::connector::request_service::Request;
pub(crate) type ConnectorResponse = crate::services::connector::request_service::Response;
