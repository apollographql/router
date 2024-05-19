use crate::plugins::demand_control::cost_calculator::schema_aware_response::TypedValue;
use crate::Context;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

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
        #[allow(dead_code)]
        field_length: FieldLength,
    },
    FieldName {
        #[allow(dead_code)]
        field_name: FieldName,
    },
    FieldType {
        #[allow(dead_code)]
        field_type: FieldType,
    },
    TypeName {
        #[allow(dead_code)]
        type_name: TypeName,
    },
}

impl Selector for GraphQLSelector {
    type Request = crate::services::supergraph::Request;
    type Response = crate::services::supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, _request: &Self::Request) -> Option<opentelemetry::Value> {
        None
    }

    fn on_response(&self, _response: &Self::Response) -> Option<opentelemetry::Value> {
        None
    }

    fn on_error(&self, _error: &BoxError) -> Option<opentelemetry::Value> {
        None
    }

    fn on_response_field(
        &self,
        typed_value: &TypedValue,
        ctx: &Context,
    ) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::FieldLength { .. } => match typed_value {
                TypedValue::String(_, string) => Some((string.len() as i64).into()),
                TypedValue::Array(_, array) => Some((array.len() as i64).into()),
                _ => None,
            },
            GraphQLSelector::FieldName { .. } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(f, _)
                | TypedValue::Number(f, _)
                | TypedValue::String(f, _)
                | TypedValue::Array(f, _)
                | TypedValue::Object(f, _) => Some(f.name.to_string().into()),
                TypedValue::Root(_) => None,
            },
            GraphQLSelector::FieldType { .. } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(f, _)
                | TypedValue::Number(f, _)
                | TypedValue::String(f, _)
                | TypedValue::Array(f, _)
                | TypedValue::Object(f, _) => {
                    Some(f.definition.ty.inner_named_type().to_string().into())
                }
                TypedValue::Root(_) => None,
            },
            GraphQLSelector::TypeName { .. } => None,
        }
    }
}
