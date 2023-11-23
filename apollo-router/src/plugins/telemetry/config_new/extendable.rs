use std::any::type_name;
use std::collections::HashMap;
use std::collections::LinkedList;
use std::fmt::Debug;

use opentelemetry::KeyValue;
use schemars::gen::SchemaGenerator;
use schemars::schema::Schema;
use schemars::JsonSchema;
use serde::de::Error;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde::Deserialize;
use serde::Deserializer;
#[cfg(test)]
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use tower::BoxError;

use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Selectors;

/// This struct can be used as an attributes container, it has a custom JsonSchema implementation that will merge the schemas of the attributes and custom fields.
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
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
    fn defaults_for_level(&mut self, requirement_level: DefaultAttributeRequirementLevel) {
        self.attributes.defaults_for_level(requirement_level);
    }
}

impl Extendable<(), ()> {
    pub(crate) fn empty<A, E>() -> Extendable<A, E>
    where
        A: Default,
    {
        Default::default()
    }
}

/// Custom Deserializer for attributes that will deserializse into a custom field if possible, but otherwise into one of the pre-defined attributes.
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
                let mut attributes: Map<String, Value> = Map::new();
                let mut custom: HashMap<String, Ext> = HashMap::new();
                while let Some(key) = map.next_key()? {
                    let value: Value = map.next_value()?;
                    match Ext::deserialize(value.clone()) {
                        Ok(value) => {
                            custom.insert(key, value);
                        }
                        Err(_err) => {
                            // We didn't manage to deserialize as a custom attribute, so stash the value and we'll try again later
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

    fn on_request(&self, request: &Self::Request) -> LinkedList<KeyValue> {
        let mut attrs = self.attributes.on_request(request);
        let custom_attributes = self.custom.iter().filter_map(|(key, value)| {
            value
                .on_request(request)
                .map(|v| KeyValue::new(key.clone(), v))
        });
        attrs.extend(custom_attributes);

        attrs
    }

    fn on_response(&self, response: &Self::Response) -> LinkedList<KeyValue> {
        let mut attrs = self.attributes.on_response(response);
        let custom_attributes = self.custom.iter().filter_map(|(key, value)| {
            value
                .on_response(response)
                .map(|v| KeyValue::new(key.clone(), v))
        });
        attrs.extend(custom_attributes);

        attrs
    }

    fn on_error(&self, error: &BoxError) -> LinkedList<KeyValue> {
        self.attributes.on_error(error)
    }
}

#[cfg(test)]
mod test {
    use insta::assert_yaml_snapshot;

    use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
    use crate::plugins::telemetry::config_new::extendable::Extendable;
    use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;

    #[test]
    fn test_extendable_serde() {
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.bind(|| {
            let o = serde_json::from_value::<Extendable<SupergraphAttributes, SupergraphSelector>>(
                serde_json::json!({
                        "graphql.operation.name": true,
                        "graphql.operation.type": true,
                        "custom_1": {
                            "operation_name": "string"
                        },
                        "custom_2": {
                            "operation_name": "string"
                        }
                }),
            )
            .unwrap();
            assert_yaml_snapshot!(o);
        });
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
}
