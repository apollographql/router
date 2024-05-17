use apollo_compiler::executable::Field;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::demand_control::cost_calculator::schema_aware_response::TypedValue;
use crate::plugins::telemetry::config_new::Selector;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum FieldLength {
    Value,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum FieldName {
    String,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum FieldType {
    String,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum TypeName {
    String,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum GraphQLSelector {
    /// The length of the field
    FieldLength {
        field_length: FieldLength,
    },
    FieldName {
        field_name: FieldName,
    },
    FieldType {
        field_type: FieldType,
    },
    TypeName {
        type_name: TypeName,
    },
}

impl Selector for GraphQLSelector {
    type Request = Field;
    type Response = TypedValue;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::FieldName { .. } => Some(request.name.to_string().into()),
            GraphQLSelector::FieldType { .. } => todo!(),
            GraphQLSelector::TypeName { .. } => todo!(),
            _ => None,
        }
    }

    fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::FieldLength {
                field_length: FieldLength::Value,
            } => match response {
                TypedValue::Array(_, items) => Some((items.len() as f64).into()),
                _ => None,
            },
            _ => None,
        }
    }

    fn on_error(&self, _error: &BoxError) -> Option<opentelemetry::Value> {
        None
    }
}
