//! Connectors telemetry.

use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;

pub(crate) mod attributes;
pub(crate) mod events;
pub(crate) mod instruments;
pub(crate) mod selectors;
pub(crate) mod spans;

pub(crate) type ConnectorRequest = HttpRequest;
pub(crate) type ConnectorResponse = HttpResponse;
