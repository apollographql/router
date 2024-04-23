use std::any::type_name;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use opentelemetry::KeyValue;
use schemars::gen::SchemaGenerator;
use schemars::schema::Schema;
use schemars::JsonSchema;
use serde::de::Error;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde::Deserialize;
use serde::Deserializer;
use serde_json::Map;
use serde_json::Value;
use tower::BoxError;

use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

/// This struct can be used as an attributes container, it has a custom JsonSchema implementation that will merge the schemas of the attributes and custom fields.
#[derive(Clone, Debug)]
pub(crate) struct Extendable<Att, Ext>
where
    Att: Default,
{
    pub(crate) attributes: Att,
    pub(crate) custom: HashMap<String, Ext>,
}

impl<Att, Ext> DefaultForLevel for Extendable<Att, Ext>
where
    Att: DefaultForLevel + Default,
{
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.attributes.defaults_for_level(requirement_level, kind);
    }
}

impl Extendable<(), ()> {
    pub(crate) fn empty_arc<A, E>() -> Arc<Extendable<A, E>>
    where
        A: Default,
    {
        Default::default()
    }
}

/// Custom Deserializer for attributes that will deserialize into a custom field if possible, but otherwise into one of the pre-defined attributes.
impl<'de, Att, Ext> Deserialize<'de> for Extendable<Att, Ext>
where
    Att: Default + Deserialize<'de> + Debug + Sized,
    Ext: Deserialize<'de> + Debug + Sized,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ExtendableVisitor<Att, Ext> {
            _phantom: std::marker::PhantomData<(Att, Ext)>,
        }
        impl<'de, Att, Ext> Visitor<'de> for ExtendableVisitor<Att, Ext>
        where
            Att: Default + Deserialize<'de> + Debug,
            Ext: Deserialize<'de> + Debug,
        {
            type Value = Extendable<Att, Ext>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "a map structure")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut attributes = Map::new();
                let mut custom: HashMap<String, Ext> = HashMap::new();
                while let Some(key) = map.next_key()? {
                    let value: Value = map.next_value()?;
                    match Ext::deserialize(value.clone()) {
                        Ok(value) => {
                            custom.insert(key, value);
                        }
                        Err(_err) => {
                            // We didn't manage to deserialize as a custom attribute, so stash the value and we'll try again later
                            // but let's try and deserialize it now so that we get a decent error message rather than 'unknown field'
                            let mut temp_attributes: Map<String, Value> = Map::new();
                            temp_attributes.insert(key.clone(), value.clone());
                            Att::deserialize(Value::Object(temp_attributes)).map_err(|e| {
                                A::Error::custom(format!(
                                    "failed to parse attribute '{}': {}",
                                    key, e
                                ))
                            })?;
                            attributes.insert(key, value);
                        }
                    }
                }

                let attributes =
                    Att::deserialize(Value::Object(attributes)).map_err(A::Error::custom)?;

                Ok(Extendable { attributes, custom })
            }
        }

        deserializer.deserialize_map(ExtendableVisitor::<Att, Ext> {
            _phantom: Default::default(),
        })
    }
}

impl<A, E> JsonSchema for Extendable<A, E>
where
    A: Default + JsonSchema,
    E: JsonSchema,
{
    fn schema_name() -> String {
        format!(
            "extendable_attribute_{}_{}",
            type_name::<A>(),
            type_name::<E>()
        )
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        let mut attributes = gen.subschema_for::<A>();
        let custom = gen.subschema_for::<HashMap<String, E>>();
        if let Schema::Object(schema) = &mut attributes {
            if let Some(object) = &mut schema.object {
                object.additional_properties =
                    custom.into_object().object().additional_properties.clone();
            }
        }

        attributes
    }
}

impl<A, E> Default for Extendable<A, E>
where
    A: Default,
{
    fn default() -> Self {
        Self {
            attributes: Default::default(),
            custom: HashMap::new(),
        }
    }
}

impl<A, E, Request, Response> Selectors for Extendable<A, E>
where
    A: Default + Selectors<Request = Request, Response = Response>,
    E: Selector<Request = Request, Response = Response>,
{
    type Request = Request;
    type Response = Response;

    fn on_request(&self, request: &Self::Request) -> Vec<KeyValue> {
        let mut attrs = self.attributes.on_request(request);
        let custom_attributes = self.custom.iter().filter_map(|(key, value)| {
            value
                .on_request(request)
                .map(|v| KeyValue::new(key.clone(), v))
        });
        attrs.extend(custom_attributes);

        attrs
    }

    fn on_response(&self, response: &Self::Response) -> Vec<KeyValue> {
        let mut attrs = self.attributes.on_response(response);
        let custom_attributes = self.custom.iter().filter_map(|(key, value)| {
            value
                .on_response(response)
                .map(|v| KeyValue::new(key.clone(), v))
        });
        attrs.extend(custom_attributes);

        attrs
    }

    fn on_error(&self, error: &BoxError) -> Vec<KeyValue> {
        self.attributes.on_error(error)
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::config_new::attributes::HttpCommonAttributes;
    use crate::plugins::telemetry::config_new::attributes::HttpServerAttributes;
    use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
    use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
    use crate::plugins::telemetry::config_new::conditional::Conditional;
    use crate::plugins::telemetry::config_new::conditions::Condition;
    use crate::plugins::telemetry::config_new::conditions::SelectorOrValue;
    use crate::plugins::telemetry::config_new::extendable::Extendable;
    use crate::plugins::telemetry::config_new::selectors::OperationName;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::plugins::telemetry::config_new::selectors::RouterSelector;
    use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;

    #[test]
    fn test_extendable_serde() {
        let extendable_conf = serde_json::from_value::<
            Extendable<SupergraphAttributes, SupergraphSelector>,
        >(serde_json::json!({
                "graphql.operation.name": true,
                "graphql.operation.type": true,
                "custom_1": {
                    "operation_name": "string"
                },
                "custom_2": {
                    "operation_name": "string"
                }
        }))
        .unwrap();
        assert_eq!(
            extendable_conf.attributes,
            SupergraphAttributes {
                graphql_document: None,
                graphql_operation_name: Some(true),
                graphql_operation_type: Some(true)
            }
        );
        assert_eq!(
            extendable_conf.custom.get("custom_1"),
            Some(&SupergraphSelector::OperationName {
                operation_name: OperationName::String,
                redact: None,
                default: None
            })
        );
        assert_eq!(
            extendable_conf.custom.get("custom_2"),
            Some(&SupergraphSelector::OperationName {
                operation_name: OperationName::String,
                redact: None,
                default: None
            })
        );
    }

    #[test]
    fn test_extendable_serde_fail() {
        serde_json::from_value::<Extendable<SupergraphAttributes, SupergraphSelector>>(
            serde_json::json!({
                    "graphql.operation": true,
                    "graphql.operation.type": true,
                    "custom_1": {
                        "operation_name": "string"
                    },
                    "custom_2": {
                        "operation_name": "string"
                    }
            }),
        )
        .expect_err("Should have errored");
    }

    #[test]
    fn test_extendable_serde_conditional() {
        let extendable_conf = serde_json::from_value::<
            Extendable<RouterAttributes, Conditional<RouterSelector>>,
        >(serde_json::json!({
        "http.request.method": true,
        "http.response.status_code": true,
        "url.path": true,
        "http.request.header.x-my-header": {
          "request_header": "x-my-header",
          "condition": {
            "eq": [
                200,
                {
                    "response_status": "code"
                }
            ]
          }
        },
        "http.request.header.x-not-present": {
          "request_header": "x-not-present",
          "default": "nope"
        }
        }))
        .unwrap();
        assert_eq!(
            extendable_conf.attributes,
            RouterAttributes {
                datadog_trace_id: None,
                trace_id: None,
                baggage: None,
                common: HttpCommonAttributes {
                    http_request_method: Some(true),
                    http_response_status_code: Some(true),
                    ..Default::default()
                },
                server: HttpServerAttributes {
                    url_path: Some(true),
                    ..Default::default()
                }
            }
        );
        assert_eq!(
            extendable_conf
                .custom
                .get("http.request.header.x-my-header"),
            Some(&Conditional {
                selector: RouterSelector::RequestHeader {
                    request_header: String::from("x-my-header"),
                    redact: None,
                    default: None
                },
                condition: Some(Arc::new(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(200.into()),
                    SelectorOrValue::Selector(RouterSelector::ResponseStatus {
                        response_status: ResponseStatus::Code
                    })
                ])))),
                value: Default::default(),
            })
        );
        assert_eq!(
            extendable_conf
                .custom
                .get("http.request.header.x-not-present"),
            Some(&Conditional {
                selector: RouterSelector::RequestHeader {
                    request_header: String::from("x-not-present"),
                    redact: None,
                    default: Some(AttributeValue::String("nope".to_string()))
                },
                condition: None,
                value: Default::default(),
            })
        );
    }
}
