use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use opentelemetry_api::Key;
use std::collections::HashMap;
use tower::BoxError;

/// These modules contain a new config structure for telemetry that will progressively move to
pub(crate) mod attributes;
pub(crate) mod conditions;

pub(crate) mod events;
pub(crate) mod extendable;
pub(crate) mod instruments;
pub(crate) mod logging;
pub(crate) mod selectors;
pub(crate) mod spans;

pub(crate) trait GetAttributes<Request, Response> {
    fn on_request(&self, request: &Request) -> HashMap<Key, AttributeValue>;
    fn on_response(&self, response: &Response) -> HashMap<Key, AttributeValue>;
    fn on_error(&self, error: &BoxError) -> HashMap<Key, AttributeValue>;
}

pub(crate) trait GetAttribute<Request, Response> {
    fn on_request(&self, request: &Request) -> Option<AttributeValue>;
    fn on_response(&self, response: &Response) -> Option<AttributeValue>;
}

pub(crate) trait DefaultForLevel {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel);
}
