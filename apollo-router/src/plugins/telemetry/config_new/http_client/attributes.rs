use opentelemetry::KeyValue;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::http;

#[apollo_configuration::configuration]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct HttpClientAttributes {}

impl DefaultForLevel for HttpClientAttributes {
    fn defaults_for_level(
        &mut self,
        _requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
    }
}

impl Selectors<http::HttpRequest, http::HttpResponse, ()> for HttpClientAttributes {
    fn on_request(&self, _request: &http::HttpRequest) -> Vec<KeyValue> {
        Vec::new()
    }

    fn on_response(&self, _response: &http::HttpResponse) -> Vec<KeyValue> {
        Vec::new()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::new()
    }
}
