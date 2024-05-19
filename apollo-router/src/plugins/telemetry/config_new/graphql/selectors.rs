use crate::plugins::demand_control::cost_calculator::schema_aware_response::TypedValue;
use crate::Context;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config_new::Selector;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ArrayLength {
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
    /// The length of the array
    ArrayLength {
        #[allow(dead_code)]
        field_length: ArrayLength,
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
            GraphQLSelector::ArrayLength { .. } => match typed_value {
                TypedValue::Array(_, _, array) => Some((array.len() as i64).into()),
                _ => None,
            },
            GraphQLSelector::FieldName { .. } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(_, f, _)
                | TypedValue::Number(_, f, _)
                | TypedValue::String(_, f, _)
                | TypedValue::Array(_, f, _)
                | TypedValue::Object(_, f, _) => Some(f.name.to_string().into()),
                TypedValue::Root(_) => None,
            },
            GraphQLSelector::FieldType { .. } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(_, f, _)
                | TypedValue::Number(_, f, _)
                | TypedValue::String(_, f, _)
                | TypedValue::Array(_, f, _)
                | TypedValue::Object(_, f, _) => {
                    Some(f.definition.ty.inner_named_type().to_string().into())
                }
                TypedValue::Root(_) => None,
            },
            GraphQLSelector::TypeName { .. } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(ty, _, _)
                | TypedValue::Number(ty, _, _)
                | TypedValue::String(ty, _, _)
                | TypedValue::Array(ty, _, _)
                | TypedValue::Object(ty, _, _) => Some(ty.to_string().into()),
                TypedValue::Root(_) => None,
            },
        }
    }
}
