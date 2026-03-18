//! Dynamic attribute management for telemetry spans and events.
//!
//! This module provides functionality for adding and managing dynamic attributes on
//! tracing spans and events. Attributes are stored using `Vec<KeyValue>` rather than
//! a `HashMap<Key, Value>` despite us not allowing duplicate keys because the vector
//! performs better at low number of elements, which is realistic for the typical number of
//! attributes we see in practice.

use opentelemetry::Key;
use opentelemetry::KeyValue;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use super::consts::OTEL_KIND;
use super::consts::OTEL_NAME;
use super::consts::OTEL_STATUS_CODE;
use super::consts::OTEL_STATUS_MESSAGE;
use super::formatters::APOLLO_CONNECTOR_PREFIX;
use super::formatters::APOLLO_PRIVATE_PREFIX;
use super::otel::OtelData;
use super::otel::layer::str_to_span_kind;
use super::otel::layer::str_to_status;
use super::utils::upsert_attribute;
use crate::plugins::telemetry::reload::otel::IsSampled;

#[derive(Debug, Default)]
pub(crate) struct LogAttributes {
    attributes: Vec<KeyValue>,
}

impl LogAttributes {
    pub(crate) fn attributes(&self) -> &Vec<KeyValue> {
        &self.attributes
    }

    pub(crate) fn insert(&mut self, kv: KeyValue) {
        upsert_attribute(&mut self.attributes, kv);
    }

    pub(crate) fn extend(&mut self, other: impl IntoIterator<Item = KeyValue>) {
        for kv in other {
            upsert_attribute(&mut self.attributes, kv);
        }
    }
}

/// To add dynamic attributes for spans
pub(crate) struct DynAttributeLayer;

impl<S> Layer<S> for DynAttributeLayer
where
    S: tracing_core::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        _attrs: &tracing_core::span::Attributes<'_>,
        id: &tracing_core::span::Id,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if extensions.get_mut::<LogAttributes>().is_none() {
                extensions.insert(LogAttributes::default());
            }
            if extensions.get_mut::<EventAttributes>().is_none() {
                extensions.insert(EventAttributes::default());
            }
        } else {
            tracing::error!("Span not found, this is a bug");
        }
    }
}

impl DynAttributeLayer {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

/// To add dynamic attributes for spans
pub(crate) trait SpanDynAttribute {
    fn set_span_dyn_attribute(&self, key: Key, value: opentelemetry::Value);
    fn set_span_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>);
}

impl SpanDynAttribute for ::tracing::Span {
    fn set_span_dyn_attribute(&self, key: Key, value: opentelemetry::Value) {
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        if s.is_sampled() {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<OtelData>() {
                                Some(otel_data) => {
                                    update_otel_data(otel_data, &key, &value);
                                    if let Some(attrs) = otel_data.builder.attributes.as_mut() {
                                        // Replace existing attribute with same key, or add new one
                                        upsert_attribute(attrs, KeyValue::new(key, value));
                                    } else {
                                        otel_data.builder.attributes =
                                            Some([KeyValue::new(key, value)].into_iter().collect());
                                    }
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            if key.as_str().starts_with(APOLLO_PRIVATE_PREFIX)
                                || key.as_str().starts_with(APOLLO_CONNECTOR_PREFIX)
                            {
                                return;
                            }
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<LogAttributes>() {
                                Some(attributes) => {
                                    attributes.insert(KeyValue::new(key, value));
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no LogAttributes, this is a bug");
                                }
                            }
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }

    fn set_span_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>) {
        let mut attributes = attributes.into_iter().peekable();
        if attributes.peek().is_none() {
            return;
        }
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => eprintln!("no spanref, this is a bug"),
                    Some(s) => {
                        if s.is_sampled() {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<OtelData>() {
                                Some(otel_data) => {
                                    let attributes: Vec<KeyValue> = attributes
                                        .inspect(|attr| {
                                            update_otel_data(otel_data, &attr.key, &attr.value)
                                        })
                                        .collect();
                                    if let Some(existing_attributes) =
                                        otel_data.builder.attributes.as_mut()
                                    {
                                        for attr in attributes {
                                            upsert_attribute(existing_attributes, attr);
                                        }
                                    } else {
                                        otel_data.builder.attributes = Some(attributes);
                                    }
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            let mut attributes = attributes
                                .filter(|kv| {
                                    !kv.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX)
                                        && !kv.key.as_str().starts_with(APOLLO_CONNECTOR_PREFIX)
                                })
                                .peekable();
                            if attributes.peek().is_none() {
                                return;
                            }
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<LogAttributes>() {
                                Some(registered_attributes) => {
                                    registered_attributes.extend(attributes);
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no LogAttributes, this is a bug");
                                }
                            }
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }
}

fn update_otel_data(otel_data: &mut OtelData, key: &Key, value: &opentelemetry::Value) {
    match key.as_str() {
        OTEL_NAME if otel_data.forced_span_name.is_none() => {
            otel_data.forced_span_name = Some(value.to_string())
        }
        OTEL_KIND => otel_data.builder.span_kind = str_to_span_kind(&value.as_str()),
        OTEL_STATUS_CODE => otel_data.forced_status = str_to_status(&value.as_str()).into(),
        OTEL_STATUS_MESSAGE => {
            otel_data.builder.status =
                opentelemetry::trace::Status::error(value.as_str().to_string())
        }
        _ => {}
    }
}

#[derive(Debug, Default)]
pub(crate) struct EventAttributes {
    attributes: Vec<KeyValue>,
}

impl EventAttributes {
    pub(crate) fn extend(&mut self, other: impl IntoIterator<Item = KeyValue>) {
        for kv in other {
            upsert_attribute(&mut self.attributes, kv);
        }
    }

    pub(crate) fn take(&mut self) -> Vec<KeyValue> {
        std::mem::take(&mut self.attributes)
    }
}

/// To add dynamic attributes for spans
pub(crate) trait EventDynAttribute {
    /// Always use before sending the event
    fn set_event_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>);
}

impl EventDynAttribute for ::tracing::Span {
    fn set_event_dyn_attributes(&self, attributes: impl IntoIterator<Item = KeyValue>) {
        let mut attributes = attributes.into_iter().peekable();
        if attributes.peek().is_none() {
            return;
        }
        self.with_subscriber(move |(id, dispatch)| {
            if let Some(reg) = dispatch.downcast_ref::<Registry>() {
                match reg.span(id) {
                    None => {
                        eprintln!("no spanref, this is a bug");
                    }
                    Some(s) => {
                        if s.is_sampled() {
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<OtelData>() {
                                Some(otel_data) => match &mut otel_data.event_attributes {
                                    Some(event_attributes) => {
                                        // No need to use the upsert function here as they're going into a Map
                                        event_attributes.extend(
                                            attributes
                                                .map(|KeyValue { key, value, .. }| (key, value)),
                                        );
                                    }
                                    None => {
                                        otel_data.event_attributes = Some(
                                            attributes
                                                .map(|KeyValue { key, value, .. }| (key, value))
                                                .collect(),
                                        );
                                    }
                                },
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no OtelData, this is a bug");
                                }
                            }
                        } else {
                            let mut attributes = attributes
                                .filter(|kv| {
                                    !kv.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX)
                                        && !kv.key.as_str().starts_with(APOLLO_CONNECTOR_PREFIX)
                                })
                                .peekable();
                            if attributes.peek().is_none() {
                                return;
                            }
                            let mut extensions = s.extensions_mut();
                            match extensions.get_mut::<EventAttributes>() {
                                Some(registered_attributes) => {
                                    registered_attributes.extend(attributes);
                                }
                                None => {
                                    // Can't use ::tracing::error! because it could create deadlock on extensions
                                    eprintln!("no EventAttributes, this is a bug");
                                }
                            }
                        }
                    }
                };
            } else {
                ::tracing::error!("no Registry, this is a bug");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use opentelemetry::Key;
    use opentelemetry::KeyValue;

    use super::EventAttributes;
    use super::LogAttributes;

    #[test]
    fn test_log_attributes_insert_replaces_existing() {
        let mut attrs = LogAttributes::default();

        // Insert initial attribute
        attrs.insert(KeyValue::new(Key::from_static_str("http.method"), "GET"));
        assert_eq!(attrs.attributes().len(), 1);
        assert_eq!(attrs.attributes()[0].value.as_str(), Cow::Borrowed("GET"));

        // Insert attribute with same key - should replace
        attrs.insert(KeyValue::new(Key::from_static_str("http.method"), "POST"));
        assert_eq!(attrs.attributes().len(), 1);
        assert_eq!(attrs.attributes()[0].value.as_str(), Cow::Borrowed("POST"));
    }

    #[test]
    fn test_log_attributes_insert_adds_new() {
        let mut attrs = LogAttributes::default();

        attrs.insert(KeyValue::new(Key::from_static_str("http.method"), "GET"));
        attrs.insert(KeyValue::new(
            Key::from_static_str("http.route"),
            "/graphql",
        ));

        assert_eq!(attrs.attributes().len(), 2);
    }

    #[test]
    fn test_log_attributes_extend_replaces_existing() {
        let mut attrs = LogAttributes::default();

        // Insert initial attributes
        attrs.insert(KeyValue::new(Key::from_static_str("http.method"), "GET"));
        attrs.insert(KeyValue::new(Key::from_static_str("http.route"), "/old"));

        // Extend with new values for existing keys
        attrs.extend([
            KeyValue::new(Key::from_static_str("http.method"), "POST"),
            KeyValue::new(Key::from_static_str("http.route"), "/new"),
            KeyValue::new(Key::from_static_str("http.status"), "200"),
        ]);

        assert_eq!(attrs.attributes().len(), 3);

        let expected_kvs = [
            ("http.method", "POST"),
            ("http.route", "/new"),
            ("http.status", "200"),
        ];

        for (key, expected_value) in expected_kvs {
            let attributes = attrs.attributes();
            let kv = attributes.iter().find(|kv| kv.key.as_str() == key).unwrap();
            assert_eq!(kv.value.as_str(), expected_value, "key = {key}");
        }
    }

    #[test]
    fn test_log_attributes_extend_preserves_order_for_new_keys() {
        let mut attrs = LogAttributes::default();

        attrs.insert(KeyValue::new(Key::from_static_str("first"), "1"));
        attrs.extend([
            KeyValue::new(Key::from_static_str("second"), "2"),
            KeyValue::new(Key::from_static_str("third"), "3"),
        ]);

        assert_eq!(attrs.attributes().len(), 3);
        assert_eq!(attrs.attributes()[0].key.as_str(), "first");
        assert_eq!(attrs.attributes()[1].key.as_str(), "second");
        assert_eq!(attrs.attributes()[2].key.as_str(), "third");
    }

    #[test]
    fn test_event_attributes_extend_replaces_existing() {
        let mut attrs = EventAttributes::default();

        // Extend with initial attributes
        attrs.extend([
            KeyValue::new(Key::from_static_str("http.method"), "GET"),
            KeyValue::new(Key::from_static_str("http.route"), "/old"),
        ]);

        // Extend with new values for existing keys
        attrs.extend([
            KeyValue::new(Key::from_static_str("http.method"), "POST"),
            KeyValue::new(Key::from_static_str("http.route"), "/new"),
            KeyValue::new(Key::from_static_str("http.status"), "200"),
        ]);

        let taken = attrs.take();
        assert_eq!(taken.len(), 3);

        let expected_kvs = [
            ("http.method", "POST"),
            ("http.route", "/new"),
            ("http.status", "200"),
        ];

        for (key, expected_value) in expected_kvs {
            let kv = taken.iter().find(|kv| kv.key.as_str() == key).unwrap();
            assert_eq!(kv.value.as_str(), expected_value, "key = {key}");
        }
    }

    #[test]
    fn test_event_attributes_extend_adds_new() {
        let mut attrs = EventAttributes::default();

        attrs.extend([
            KeyValue::new(Key::from_static_str("http.method"), "GET"),
            KeyValue::new(Key::from_static_str("http.route"), "/graphql"),
        ]);

        let taken = attrs.take();
        assert_eq!(taken.len(), 2);
    }

    #[test]
    fn test_event_attributes_take_clears_attributes() {
        let mut attrs = EventAttributes::default();

        attrs.extend([KeyValue::new(Key::from_static_str("http.method"), "GET")]);
        let taken = attrs.take();
        assert_eq!(taken.len(), 1);

        // After take, attributes should be empty
        let taken_again = attrs.take();
        assert_eq!(taken_again.len(), 0);
    }
}
