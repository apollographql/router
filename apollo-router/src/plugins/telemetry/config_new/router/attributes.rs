use std::fmt::Debug;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::baggage::BaggageExt;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tracing::Span;

use crate::Context;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::config_new::http_common::attributes::HttpCommonAttributes;
use crate::plugins::telemetry::config_new::http_server::attributes::HttpServerAttributes;
use crate::plugins::telemetry::config_new::trace_id;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterAttributes {
    /// The datadog trace ID.
    /// This can be output in logs and used to correlate traces in Datadog.
    #[serde(rename = "dd.trace_id")]
    pub(crate) datadog_trace_id: Option<StandardAttribute>,

    /// The OpenTelemetry trace ID.
    /// This can be output in logs.
    pub(crate) trace_id: Option<StandardAttribute>,

    /// All key values from trace baggage.
    pub(crate) baggage: Option<bool>,

    /// Http attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    pub(crate) common: HttpCommonAttributes,
    /// Http server attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    pub(crate) server: HttpServerAttributes,
}

impl DefaultForLevel for RouterAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.common.defaults_for_level(requirement_level, kind);
        self.server.defaults_for_level(requirement_level, kind);
    }
}

impl Selectors<router::Request, router::Response, ()> for RouterAttributes {
    fn on_request(&self, request: &router::Request) -> Vec<KeyValue> {
        let mut attrs = self.common.on_request(request);
        attrs.extend(self.server.on_request(request));
        if let Some(key) = self
            .trace_id
            .as_ref()
            .and_then(|a| a.key(Key::from_static_str("trace_id")))
            && let Some(trace_id) = trace_id()
        {
            attrs.push(KeyValue::new(key, trace_id.to_string()));
        }

        if let Some(key) = self
            .datadog_trace_id
            .as_ref()
            .and_then(|a| a.key(Key::from_static_str("dd.trace_id")))
            && let Some(trace_id) = trace_id()
        {
            attrs.push(KeyValue::new(key, trace_id.to_datadog()));
        }
        if let Some(true) = &self.baggage {
            let context = Span::current().context();
            let baggage = context.baggage();
            for (key, (value, _)) in baggage {
                attrs.push(KeyValue::new(key.clone(), value.clone()));
            }
        }

        attrs
    }

    fn on_response(&self, response: &router::Response) -> Vec<KeyValue> {
        let mut attrs = self.common.on_response(response);
        attrs.extend(self.server.on_response(response));
        attrs
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Vec<KeyValue> {
        let mut attrs = self.common.on_error(error, ctx);
        attrs.extend(self.server.on_error(error, ctx));
        attrs
    }
}

#[cfg(test)]
mod test {
    use opentelemetry::Context;
    use opentelemetry::KeyValue;
    use opentelemetry::baggage::BaggageExt;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use tracing::span;
    use tracing::subscriber;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::otel;
    use crate::services::router;

    #[test]
    fn test_router_trace_attributes() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let span_context = SpanContext::new(
                TraceId::from_u128(42),
                SpanId::from_u64(42),
                TraceFlags::default().with_sampled(true),
                false,
                TraceState::default(),
            );
            let _context = Context::current()
                .with_remote_span_context(span_context)
                .with_baggage(vec![
                    KeyValue::new("baggage_key", "baggage_value"),
                    KeyValue::new("baggage_key_bis", "baggage_value_bis"),
                ])
                .attach();
            let span = span!(tracing::Level::INFO, "test");
            let _guard = span.enter();

            let attributes = RouterAttributes {
                datadog_trace_id: Some(StandardAttribute::Bool(true)),
                trace_id: Some(StandardAttribute::Bool(true)),
                baggage: Some(true),
                common: Default::default(),
                server: Default::default(),
            };
            let attributes =
                attributes.on_request(&router::Request::fake_builder().build().unwrap());

            assert_eq!(
                attributes
                    .iter()
                    .find(|key_val| key_val.key == opentelemetry::Key::from_static_str("trace_id"))
                    .map(|key_val| &key_val.value),
                Some(&"0000000000000000000000000000002a".into())
            );
            assert_eq!(
                attributes
                    .iter()
                    .find(
                        |key_val| key_val.key == opentelemetry::Key::from_static_str("dd.trace_id")
                    )
                    .map(|key_val| &key_val.value),
                Some(&"42".into())
            );
            assert_eq!(
                attributes
                    .iter()
                    .find(
                        |key_val| key_val.key == opentelemetry::Key::from_static_str("baggage_key")
                    )
                    .map(|key_val| &key_val.value),
                Some(&"baggage_value".into())
            );
            assert_eq!(
                attributes
                    .iter()
                    .find(|key_val| key_val.key
                        == opentelemetry::Key::from_static_str("baggage_key_bis"))
                    .map(|key_val| &key_val.value),
                Some(&"baggage_value_bis".into())
            );

            let attributes = RouterAttributes {
                datadog_trace_id: Some(StandardAttribute::Aliased {
                    alias: "datatoutou_id".to_string(),
                }),
                trace_id: Some(StandardAttribute::Aliased {
                    alias: "my_trace_id".to_string(),
                }),
                baggage: Some(false),
                common: Default::default(),
                server: Default::default(),
            };
            let attributes =
                attributes.on_request(&router::Request::fake_builder().build().unwrap());

            assert_eq!(
                attributes
                    .iter()
                    .find(
                        |key_val| key_val.key == opentelemetry::Key::from_static_str("my_trace_id")
                    )
                    .map(|key_val| &key_val.value),
                Some(&"0000000000000000000000000000002a".into())
            );
            assert_eq!(
                attributes
                    .iter()
                    .find(|key_val| key_val.key
                        == opentelemetry::Key::from_static_str("datatoutou_id"))
                    .map(|key_val| &key_val.value),
                Some(&"42".into())
            );
        });
    }
}
