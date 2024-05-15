use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config_new::Selector;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum GraphQLSelector {
    FieldLength { field_name: Option<String> },
}

impl Selector for GraphQLSelector {
    type Request = ();
    type Response = ();
    type EventResponse = ();

    fn on_request(&self, _request: &Self::Request) -> Option<opentelemetry::Value> {
        None
    }

    fn on_response(&self, _response: &Self::Response) -> Option<opentelemetry::Value> {
        None
    }

    fn on_response_field(
        &self,
        field: &apollo_compiler::ast::Field,
        value: &serde_json::Value,
    ) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::FieldLength { field_name } => {
                if field_name
                    .as_ref()
                    .is_some_and(|n| *n == field.name.to_string())
                {
                    match value {
                        serde_json::Value::Array(items) => {
                            Some(opentelemetry::Value::F64(items.len() as f64))
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
        }
    }

    fn on_error(&self, _error: &BoxError) -> Option<opentelemetry::Value> {
        None
    }
}
