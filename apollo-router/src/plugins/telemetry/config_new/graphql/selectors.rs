use crate::context::OPERATION_NAME;
use crate::Context;
use schemars::JsonSchema;
use serde::Deserialize;
use sha2::Digest;
use tower::BoxError;

use crate::plugins::telemetry::config_new::selectors::OperationName;
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
    OperationName {
        /// The operation name from the query.
        operation_name: OperationName,
        /// Optional default value.
        default: Option<String>,
    },
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
        ty: &apollo_compiler::schema::Type,
        field: &apollo_compiler::schema::FieldDefinition,
        value: &serde_json::Value,
        ctx: &Context,
    ) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::FieldLength { field_length } => {
                if *field_length {
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
            GraphQLSelector::FieldName { field_name } => {
                if *field_name {
                    Some(opentelemetry::Value::String(field.name.to_string().into()))
                } else {
                    None
                }
            }
            GraphQLSelector::FieldType { field_type } => {
                if *field_type {
                    Some(opentelemetry::Value::String(field.ty.to_string().into()))
                } else {
                    None
                }
            }
            GraphQLSelector::TypeName { type_name } => {
                if *type_name {
                    Some(opentelemetry::Value::String(ty.to_string().into()))
                } else {
                    None
                }
            }
            GraphQLSelector::OperationName {
                operation_name,
                default,
                ..
            } => {
                let op_name = ctx.get(OPERATION_NAME).ok().flatten();
                match operation_name {
                    OperationName::String => op_name.or_else(|| default.clone()),
                    OperationName::Hash => op_name.or_else(|| default.clone()).map(|op_name| {
                        let mut hasher = sha2::Sha256::new();
                        hasher.update(op_name.as_bytes());
                        let result = hasher.finalize();
                        hex::encode(result)
                    }),
                }
                .map(opentelemetry::Value::from)
            }
        }
    }

    fn on_error(&self, _error: &BoxError) -> Option<opentelemetry::Value> {
        None
    }
}
