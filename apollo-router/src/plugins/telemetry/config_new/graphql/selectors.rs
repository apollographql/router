use apollo_compiler::executable::Field;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::demand_control::cost_calculator::schema_aware_response::TypedValue;
use crate::plugins::telemetry::config_new::Selector;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum GraphQLSelector {
    /// The length of the field
    FieldLength {
        field_length: bool,
    },
    FieldName {
        field_name: bool,
    },
    FieldType {
        field_type: bool,
    },
    TypeName {
        type_name: bool,
    },
}

impl Selector for GraphQLSelector {
    type Request = Field;
    type Response = TypedValue;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::FieldName { field_name: true } => {
                Some(request.name.to_string().into())
            }
            GraphQLSelector::FieldType { field_type: true } => todo!(),
            GraphQLSelector::TypeName { type_name } => todo!(),
            _ => None,
        }
    }

    fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::FieldLength { field_length: true } => match response {
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
