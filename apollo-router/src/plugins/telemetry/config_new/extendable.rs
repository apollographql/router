use std::any::type_name;
use std::collections::BTreeMap;
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

use super::Stage;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::Context;

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
        // Extendable json schema is composed of and anyOf of A and additional properties of E
        // To allow this to happen we need to generate a schema that contains all the properties of A
        // and a schema ref to A.
        // We can then add additional properties to the schema of type E.

        let attributes = gen.subschema_for::<A>();
        let custom = gen.subschema_for::<HashMap<String, E>>();

        // Get a list of properties from the attributes schema
        let attribute_schema = gen
            .dereference(&attributes)
            .expect("failed to dereference attributes");
        let mut properties = BTreeMap::new();
        if let Schema::Object(schema_object) = attribute_schema {
            if let Some(object_validation) = &schema_object.object {
                for key in object_validation.properties.keys() {
                    properties.insert(key.clone(), Schema::Bool(true));
                }
            }
        }
        let mut schema = attribute_schema.clone();
        if let Schema::Object(schema_object) = &mut schema {
            if let Some(object_validation) = &mut schema_object.object {
                object_validation.additional_properties = custom
                    .into_object()
                    .object
                    .expect("could not get obejct validation")
                    .additional_properties;
            }
        }
        schema
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

impl<A, E, Request, Response, EventResponse> Selectors for Extendable<A, E>
where
    A: Default + Selectors<Request = Request, Response = Response, EventResponse = EventResponse>,
    E: Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

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

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Vec<KeyValue> {
        let mut attrs = self.attributes.on_error(error, ctx);
        let custom_attributes = self.custom.iter().filter_map(|(key, value)| {
            value
                .on_error(error, ctx)
                .map(|v| KeyValue::new(key.clone(), v))
        });
        attrs.extend(custom_attributes);

        attrs
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) -> Vec<KeyValue> {
        let mut attrs = self.attributes.on_response_event(response, ctx);
        let custom_attributes = self.custom.iter().filter_map(|(key, value)| {
            value
                .on_response_event(response, ctx)
                .map(|v| KeyValue::new(key.clone(), v))
        });
        attrs.extend(custom_attributes);

        attrs
    }

    fn on_response_field(
        &self,
        attrs: &mut Vec<KeyValue>,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &serde_json_bytes::Value,
        ctx: &Context,
    ) {
        self.attributes
            .on_response_field(attrs, ty, field, value, ctx);
        let custom_attributes = self.custom.iter().filter_map(|(key, v)| {
            v.on_response_field(ty, field, value, ctx)
                .map(|v| KeyValue::new(key.clone(), v))
        });
        attrs.extend(custom_attributes);
    }
}

impl<A, E, Request, Response, EventResponse> Extendable<A, E>
where
    A: Default + Selectors<Request = Request, Response = Response, EventResponse = EventResponse>,
    E: Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
{
    pub(crate) fn validate(&self, restricted_stage: Option<Stage>) -> Result<(), String> {
        if let Some(Stage::Request) = &restricted_stage {
            for (name, custom) in &self.custom {
                if !custom.is_active(Stage::Request) {
                    return Err(format!("cannot set the attribute {name:?} because it is using a selector computed in another stage than 'request' so it will not be computed"));
                }
            }
        }

        Ok(())
    }
}
#[cfg(test)]
mod test {
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
                graphql_operation_type: Some(true),
                cost: Default::default()
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
                condition: Some(Mutex::new(Condition::Eq([
                    SelectorOrValue::Value(200.into()),
                    SelectorOrValue::Selector(RouterSelector::ResponseStatus {
                        response_status: ResponseStatus::Code
                    })
                ]))),
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
