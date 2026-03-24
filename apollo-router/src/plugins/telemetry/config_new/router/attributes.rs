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

    /// Optional client name populated from the request headers.
    #[serde(rename = "client.name")]
    pub(crate) client_name: Option<StandardAttribute>,
    /// Optional client version populated from the request headers.
    #[serde(rename = "client.version")]
    pub(crate) client_version: Option<StandardAttribute>,

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
        match requirement_level {
            DefaultAttributeRequirementLevel::Required
            | DefaultAttributeRequirementLevel::Recommended => {
                if self.client_name.is_none() {
                    self.client_name = Some(StandardAttribute::Bool(true));
                }
                if self.client_version.is_none() {
                    self.client_version = Some(StandardAttribute::Bool(true));
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
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
    use crate::plugins::telemetry::otlp::TelemetryDataKind;
    use crate::services::router;

    #[test]
    fn test_router_trace_attributes() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let span_context = SpanContext::new(
                TraceId::from(42),
                SpanId::from(42),
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
                client_name: None,
                client_version: None,
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
                client_name: None,
                client_version: None,
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

    #[test]
    fn test_defaults_for_level_sets_client_name_and_version() {
        let mut attrs = RouterAttributes::default();
        assert!(attrs.client_name.is_none());
        assert!(attrs.client_version.is_none());

        attrs.defaults_for_level(
            DefaultAttributeRequirementLevel::Required,
            TelemetryDataKind::Traces,
        );
        assert_eq!(
            attrs.client_name,
            Some(StandardAttribute::Bool(true)),
            "client_name should default to true at Required level"
        );
        assert_eq!(
            attrs.client_version,
            Some(StandardAttribute::Bool(true)),
            "client_version should default to true at Required level"
        );
    }

    #[test]
    fn test_defaults_for_level_preserves_explicit_config() {
        let mut attrs = RouterAttributes {
            client_name: Some(StandardAttribute::Bool(false)),
            client_version: Some(StandardAttribute::Aliased {
                alias: "my_version".to_string(),
            }),
            ..Default::default()
        };

        attrs.defaults_for_level(
            DefaultAttributeRequirementLevel::Required,
            TelemetryDataKind::Traces,
        );
        assert_eq!(
            attrs.client_name,
            Some(StandardAttribute::Bool(false)),
            "explicit false should not be overwritten by defaults"
        );
        assert_eq!(
            attrs.client_version,
            Some(StandardAttribute::Aliased {
                alias: "my_version".to_string()
            }),
            "explicit alias should not be overwritten by defaults"
        );
    }

    #[test]
    fn test_defaults_for_level_none_does_not_set_client_attrs() {
        let mut attrs = RouterAttributes::default();
        attrs.defaults_for_level(
            DefaultAttributeRequirementLevel::None,
            TelemetryDataKind::Traces,
        );
        assert!(
            attrs.client_name.is_none(),
            "client_name should remain None at None level"
        );
        assert!(
            attrs.client_version.is_none(),
            "client_version should remain None at None level"
        );
    }
}
