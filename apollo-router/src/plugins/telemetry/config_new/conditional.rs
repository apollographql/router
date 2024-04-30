use std::any::type_name;
use std::fmt::Debug;
use std::mem;
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

/// The state of the conditional.
#[derive(Debug, Default)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) enum State<T> {
    /// The conditional has not been evaluated yet or no value has been set via selector.
    #[default]
    Pending,
    /// The conditional has been evaluated and the value has been obtained.
    Value(T),
    /// The conditional has been evaluated and the value has been returned, no further processing should take place.
    Returned,
}

impl<T> From<T> for State<T> {
    fn from(value: T) -> Self {
        State::Value(value)
    }
}

/// Conditional is a stateful structure that may be called multiple times during the course of a request/response cycle.
/// As each callback is called the underlying condition is updated. If the condition can eventually be evaluated then it returns
/// Some(true|false) otherwise it returns None.
#[derive(Clone, Debug, Default)]
pub(crate) struct Conditional<Att> {
    pub(crate) selector: Att,
    pub(crate) condition: Option<Arc<Mutex<Condition<Att>>>>,
    pub(crate) value: Arc<Mutex<State<opentelemetry::Value>>>,
}

#[cfg(test)]
impl<Att> PartialEq for Conditional<Att>
where
    Att: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        let condition_eq = match (&self.condition, &other.condition) {
            (Some(l), Some(r)) => *(l.lock()) == *(r.lock()),
            (None, None) => true,
            _ => false,
        };
        let value_eq = *(self.value.lock()) == *(other.value.lock());
        self.selector == other.selector && value_eq && condition_eq
    }
}

impl<T> JsonSchema for Conditional<T>
where
    T: JsonSchema,
{
    fn schema_name() -> String {
        format!("conditional_attribute_{}", type_name::<T>())
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        // Add condition to each variant in the schema.
        //Maybe we can rearrange this for a smaller schema
        let mut selector = gen.subschema_for::<T>();

        if let Schema::Object(schema) = &mut selector {
            if let Some(object) = &mut schema.subschemas {
                if let Some(any_of) = &mut object.any_of {
                    for mut variant in any_of {
                        if let Schema::Object(variant) = &mut variant {
                            if let Some(object) = &mut variant.object {
                                object.properties.insert(
                                    "condition".to_string(),
                                    gen.subschema_for::<Condition<T>>(),
                                );
                            }
                        }
                    }
                }
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
                let request_condition_res = condition.lock().evaluate_request(request);
                match request_condition_res {
                    None => {
                        if let Some(value) = self.selector.on_request(request) {
                            *self.value.lock() = value.into();
                        }
                        None
                    }
                    Some(true) => {
                        // The condition evaluated to true, so we can just return the value but may need to try again on the response.
                        match self.selector.on_request(request) {
                            None => None,
                            Some(value) => {
                                *self.value.lock() = State::Returned;
                                Some(value)
                            }
                        }
                    }
                    Some(false) => {
                        // The condition has been evaluated to false, so we can return None. it will never return true.
                        *self.value.lock() = State::Returned;
                        None
                    }
                }
            }
            None => {
                // There is no condition to evaluate, so we can just return the value.
                match self.selector.on_request(request) {
                    None => None,
                    Some(value) => {
                        *self.value.lock() = State::Returned;
                        Some(value)
                    }
                }
            }
        }
    }

    fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value> {
        // We may have got the value from the request.
        let value = mem::take(&mut *self.value.lock());

        match (value, &self.condition) {
            (State::Value(value), Some(condition)) => {
                // We have a value already, let's see if the condition was evaluated to true.
                if condition.lock().evaluate_response(response) {
                    *self.value.lock() = State::Returned;
                    Some(value)
                } else {
                    None
                }
            }
            (State::Pending, Some(condition)) => {
                // We don't have a value already, let's try to get it from the response if the condition was evaluated to true.
                if condition.lock().evaluate_response(response) {
                    self.selector.on_response(response)
                } else {
                    None
                }
            }
            (State::Pending, None) => {
                // We don't have a value already, and there is no condition.
                self.selector.on_response(response)
            }
            _ => None,
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
                let mut condition: Option<Condition<Att>> = None;
                let mut attributes = Map::new();
                // Separate out the condition from the rest of the attributes.
                while let Some(key) = map.next_key::<String>()? {
                    let value: Value = map.next_value()?;
                    if key == "condition" {
                        condition = Some(
                            Condition::<Att>::deserialize(value.clone())
                                .map_err(|e| Error::custom(e.to_string()))?,
                        )
                    } else {
                        attributes.insert(key.clone(), value);
                    }
                }

                // Try to parse the attribute
                let selector =
                    Att::deserialize(Value::Object(attributes)).map_err(A::Error::custom)?;

                Ok(Conditional {
                    selector,
                    condition: condition.map(|c| Arc::new(Mutex::new(c))),
                    value: Arc::new(Default::default()),
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
                    value: Arc::new(Default::default()),
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

    use crate::plugins::telemetry::config_new::conditional::Conditional;
    use crate::plugins::telemetry::config_new::selectors::RouterSelector;
    use crate::plugins::telemetry::config_new::Selector;

    fn on_response(conditional: Conditional<RouterSelector>) -> Option<Value> {
        conditional.on_response(
            &crate::services::router::Response::fake_builder()
                .status_code(StatusCode::from_u16(201).unwrap())
                .build()
                .expect("resp"),
        )
    }

    fn on_request(conditional: &Conditional<RouterSelector>) -> Option<Value> {
        conditional.on_request(
            &crate::services::router::Request::fake_builder()
                .header("head", "val")
                .build()
                .expect("req"),
        )
    }

    #[test]
    fn test_value_from_response_condition_from_request() {
        let config = r#"
            response_status: code
            condition:
              any:
              - eq:
                - request_header: head
                - "val"
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert!(result.is_none());
        let result = on_response(conditional);
        assert_eq!(result.expect("expected result"), Value::I64(201));
    }

    #[test]
    fn test_value_from_request_condition_from_response() {
        let config = r#"
            request_header: head
            condition:
              any:
              - eq:
                - response_status: code
                - 201
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert!(result.is_none());
        let result = on_response(conditional);
        assert_eq!(
            result.expect("expected result"),
            Value::String("val".into())
        );
    }

    #[test]
    fn test_value_from_request_condition_from_request() {
        let config = r#"
            request_header: head
            condition:
              any:
              - eq:
                - request_header: head
                - val
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert_eq!(
            result.expect("expected result"),
            Value::String("val".into())
        );

        let result = on_response(conditional);
        assert!(result.is_none());
    }

    #[test]
    fn test_value_from_response_condition_from_response() {
        let config = r#"
            response_status: code
            condition:
              any:
              - eq:
                - response_status: code
                - 201
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert!(result.is_none());
        let result = on_response(conditional);
        assert_eq!(result.expect("expected result"), Value::I64(201));
    }

    #[test]
    fn test_response_condition_from_request_fail() {
        let config = r#"
            response_status: code
            condition:
              any:
              - eq:
                - request_header: head
                - 999
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert!(result.is_none());
        let result = on_response(conditional);
        assert!(result.is_none());
    }
    #[test]
    fn test_response_condition_from_response_fail() {
        let config = r#"
            response_status: code
            condition:
              any:
              - eq:
                - response_status: code
                - 999
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert!(result.is_none());
        let result = on_response(conditional);
        assert!(result.is_none());
    }

    #[test]
    fn test_request_condition_from_request_fail() {
        let config = r#"
            request_header: head
            condition:
              any:
              - eq:
                - request_header: head
                - 999
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert!(result.is_none());
        let result = on_response(conditional);
        assert!(result.is_none());
    }
    #[test]
    fn test_request_condition_from_response_fail() {
        let config = r#"
            request_header: head
            condition:
              any:
              - eq:
                - response_status: code
                - 999
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert!(result.is_none());
        let result = on_response(conditional);
        assert!(result.is_none());
    }

    #[test]
    fn test_deserialization() {
        let config = r#"
            request_header: head
            default: hmm
            condition:
              any:
              - eq:
                - response_status: code
                - 201
        "#;

        let conditional: super::Conditional<RouterSelector> = serde_yaml::from_str(config).unwrap();
        let result = on_request(&conditional);
        assert!(result.is_none());
        let result = on_response(conditional);
        assert_eq!(
            result.expect("expected result"),
            Value::String("val".into())
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
