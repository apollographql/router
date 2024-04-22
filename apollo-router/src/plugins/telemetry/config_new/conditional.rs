use std::any::type_name;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use parking_lot::Mutex;
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

use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
/// Conditional is a stateful structure that may be called multiple times during the course of a request/response cycle.
/// As each callback is called the underlying condition is updated. If the condition can eventually be evaluated then it returns
/// Some(true|false) otherwise it returns None.
#[derive(Clone, Debug, Default)]
pub(crate) struct Conditional<Att> {
    pub(crate) selector: Att,
    pub(crate) condition: Option<Arc<Mutex<Condition<Att>>>>,
}

impl<T> JsonSchema for Conditional<T>
where
    T: JsonSchema,
{
    fn schema_name() -> String {
        format!("conditional_attribute_{}", type_name::<T>())
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        let mut selector = gen.subschema_for::<HashMap<String, T>>();
        if let Schema::Object(schema) = &mut selector {
            if let Some(object) = &mut schema.object {
                object
                    .properties
                    .insert("condition".to_string(), gen.subschema_for::<Condition<T>>());
            }
        }

        selector
    }
}

impl<Att> DefaultForLevel for Conditional<Att>
where
    Att: DefaultForLevel,
{
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.selector.defaults_for_level(requirement_level, kind);
    }
}

impl<Att, Request, Response> Selector for Conditional<Att>
where
    Att: Selector<Request = Request, Response = Response>,
{
    type Request = Request;
    type Response = Response;

    fn on_request(&self, request: &Self::Request) -> Option<opentelemetry::Value> {
        match &self.condition {
            Some(condition) => {
                if condition.lock().evaluate_request(request) == Some(true) {
                    self.selector.on_request(request)
                } else {
                    None
                }
            }
            None => self.selector.on_request(request),
        }
    }

    fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value> {
        match &self.condition {
            Some(condition) => {
                if condition.lock().evaluate_response(response) {
                    self.selector.on_response(response)
                } else {
                    None
                }
            }
            None => self.selector.on_response(response),
        }
    }
}

/// Custom Deserializer for attributes that will deserialize into a custom field if possible, but otherwise into one of the pre-defined attributes.
impl<'de, Att> Deserialize<'de> for Conditional<Att>
where
    Att: Deserialize<'de> + Debug + Sized,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ConditionalVisitor<Att> {
            _phantom: std::marker::PhantomData<Att>,
        }
        impl<'de, Att> Visitor<'de> for ConditionalVisitor<Att>
        where
            Att: Deserialize<'de> + Debug,
        {
            type Value = Conditional<Att>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "a map structure")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut selector: Option<Att> = None;
                let mut condition: Option<Condition<Att>> = None;
                while let Some(key) = map.next_key::<String>()? {
                    let value: Value = map.next_value()?;
                    if key == "condition" {
                        condition = Some(
                            Condition::<Att>::deserialize(value.clone())
                                .map_err(|e| Error::custom(e.to_string()))?,
                        )
                    } else {
                        let mut map = Map::new();
                        map.insert(key.clone(), value);
                        let o = Value::Object(map);
                        selector =
                            Some(Att::deserialize(o).map_err(|e| Error::custom(e.to_string()))?)
                    }
                }
                if selector.is_none() {
                    return Err(A::Error::custom("selector is required"));
                }

                Ok(Conditional {
                    selector: selector.expect("selector is required"),
                    condition: condition.map(|c| Arc::new(Mutex::new(c))),
                })
            }
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Ok(Conditional {
                    selector: Att::deserialize(Value::String(v.to_string()))
                        .map_err(|e| Error::custom(e.to_string()))?,
                    condition: None,
                })
            }
        }

        deserializer.deserialize_any(ConditionalVisitor::<Att> {
            _phantom: Default::default(),
        })
    }
}

#[cfg(test)]
mod test {
    use http::StatusCode;
    use opentelemetry_api::Value;

    use crate::plugins::telemetry::config_new::selectors::RouterSelector;
    use crate::plugins::telemetry::config_new::Selector;

    #[test]
    fn test_deserialization_ok_() {
        let config = r#"
            static: "there was an error"
            condition:
              any:
              - eq:
                - response_status: code
                - 201
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = conditional.on_response(
            &crate::services::router::Response::fake_builder()
                .status_code(StatusCode::from_u16(201).unwrap())
                .build()
                .expect("req"),
        );
        //TODO, none always get returned.
        assert_eq!(
            result.expect("expected result"),
            Value::String("there was an error".into())
        );
    }

    #[test]
    fn test_deserialization_missing_selector() {
        let config = r#"
            condition:
              any:
              - eq:
                - response_status: code
                - 201
        "#;

        serde_yaml::from_str::<super::Conditional<RouterSelector>>(config)
            .expect_err("Could have failed to deserialize");
    }

    #[test]
    fn test_deserialization_invalid_selector() {
        let config = r#"
            invalid: "foo"
            condition:
              any:
              - eq:
                - response_status: code
                - 201
        "#;

        let result = serde_yaml::from_str::<super::Conditional<RouterSelector>>(config);
        assert!(result
            .expect_err("should have got error")
            .to_string()
            .contains("data did not match any variant of untagged enum RouterSelector"),)
    }

    #[test]
    fn test_deserialization_invalid_condition() {
        let config = r#"
            static: "foo"
            condition:
              aaargh: ""
        "#;

        let result = serde_yaml::from_str::<super::Conditional<RouterSelector>>(config);
        assert!(result
            .expect_err("should have got error")
            .to_string()
            .contains("unknown variant `aaargh`"),)
    }

    #[test]
    fn test_simple_value() {
        let config = r#"
            "foo"
        "#;

        let result = serde_yaml::from_str::<super::Conditional<RouterSelector>>(config)
            .expect("should have parsed");
        assert!(result.condition.is_none());
        assert!(matches!(result.selector, RouterSelector::Static(_)));
    }
}
