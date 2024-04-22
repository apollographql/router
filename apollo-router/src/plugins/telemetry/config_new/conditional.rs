use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::{DefaultForLevel, Selector};
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use parking_lot::Mutex;
use schemars::gen::SchemaGenerator;
use schemars::schema::Schema;
use schemars::JsonSchema;
use serde::Deserialize;
use std::any::type_name;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Conditional<T> {
    pub(crate) selector: T,
    pub(crate) condition: Option<Arc<Mutex<Condition<T>>>>,
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

impl<T> DefaultForLevel for Conditional<T>
where
    T: DefaultForLevel,
{
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.selector.defaults_for_level(requirement_level, kind);
    }
}

impl<T, Request, Response> Selector for Conditional<T>
where
    T: Selector<Request = Request, Response = Response>,
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
