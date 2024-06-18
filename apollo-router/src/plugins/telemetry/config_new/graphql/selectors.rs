use apollo_compiler::executable::Field;
use apollo_compiler::executable::NamedType;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use sha2::Digest;
use tower::BoxError;

use crate::context::OPERATION_NAME;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::selectors::OperationName;
use crate::plugins::telemetry::config_new::Selector;
use crate::Context;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ListLength {
    /// The length of the list
    Value,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum FieldName {
    /// The GraphQL field name
    String,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum FieldType {
    /// The GraphQL field name
    Name,
    /// The GraphQL field type
    /// - `bool`
    /// - `number`
    /// - `scalar`
    /// - `object`
    /// - `list`
    Type,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum TypeName {
    /// The GraphQL type name
    String,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum GraphQLSelector {
    /// If the field is a list, the length of the list
    ListLength {
        #[allow(dead_code)]
        list_length: ListLength,
    },
    /// The GraphQL field name
    FieldName {
        #[allow(dead_code)]
        field_name: FieldName,
    },
    /// The GraphQL field type
    FieldType {
        #[allow(dead_code)]
        field_type: FieldType,
    },
    /// The GraphQL type name
    TypeName {
        #[allow(dead_code)]
        type_name: TypeName,
    },
    OperationName {
        /// The operation name from the query.
        operation_name: OperationName,
        /// Optional default value.
        default: Option<String>,
    },
    StaticField {
        /// A static value
        r#static: AttributeValue,
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

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Option<opentelemetry::Value> {
        None
    }

    fn on_response_field(
        &self,
        ty: &NamedType,
        field: &Field,
        value: &Value,
        ctx: &Context,
    ) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::ListLength { .. } => match value {
                Value::Array(array) => Some((array.len() as i64).into()),
                _ => None,
            },
            GraphQLSelector::FieldName { .. } => match value {
                Value::Null => None,
                _ => Some(field.name.to_string().into()),
            },
            GraphQLSelector::FieldType {
                field_type: FieldType::Name,
            } => match value {
                Value::Null => None,
                _ => Some(field.definition.ty.inner_named_type().to_string().into()),
            },
            GraphQLSelector::FieldType {
                field_type: FieldType::Type,
            } => match value {
                Value::Null => None,
                Value::Bool(_) | Value::Number(_) | Value::String(_) => Some("scalar".into()),
                Value::Object(_) => Some("object".into()),
                Value::Array(_) => Some("list".into()),
            },
            GraphQLSelector::TypeName { .. } => match value {
                Value::Null => None,
                _ => Some(ty.to_string().into()),
            },
            GraphQLSelector::StaticField { r#static } => Some(r#static.clone().into()),
            GraphQLSelector::OperationName {
                operation_name,
                default,
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
}

#[cfg(test)]
mod tests {
    use opentelemetry::Value;
    use serde_json_bytes::json;

    use super::*;
    use crate::plugins::telemetry::config_new::test::field;
    use crate::plugins::telemetry::config_new::test::ty;

    #[test]
    fn array_length() {
        let selector = GraphQLSelector::ListLength {
            list_length: ListLength::Value,
        };
        let result = selector.on_response_field(
            &ty(),
            field(),
            &json!(vec![true, true, true]),
            &Context::default(),
        );
        assert_eq!(result, Some(Value::I64(3)));
    }

    #[test]
    fn field_name() {
        let selector = GraphQLSelector::FieldName {
            field_name: FieldName::String,
        };
        let result = selector.on_response_field(&ty(), field(), &json!(true), &Context::default());
        assert_eq!(result, Some(Value::String("field_name".into())));
    }

    #[test]
    fn field_type() {
        let selector = GraphQLSelector::FieldType {
            field_type: FieldType::Name,
        };
        let result = selector.on_response_field(&ty(), field(), &json!(true), &Context::default());
        assert_eq!(result, Some(Value::String("field_type".into())));
    }

    #[test]
    fn field_type_scalar_type() {
        assert_scalar(&ty(), field(), &json!("value"));
        assert_scalar(&ty(), field(), &json!(1));
    }

    fn assert_scalar(ty: &NamedType, field: &Field, value: &serde_json_bytes::Value) {
        let result = GraphQLSelector::FieldType {
            field_type: FieldType::Type,
        }
        .on_response_field(ty, field, value, &Context::default());
        assert_eq!(result, Some(Value::String("scalar".into())));
    }

    #[test]
    fn field_type_object_type() {
        let selector = GraphQLSelector::FieldType {
            field_type: FieldType::Type,
        };
        let result = selector.on_response_field(&ty(), field(), &json!({}), &Context::default());
        assert_eq!(result, Some(Value::String("object".into())));
    }

    #[test]
    fn field_type_list_type() {
        let selector = GraphQLSelector::FieldType {
            field_type: FieldType::Type,
        };
        let result =
            selector.on_response_field(&ty(), field(), &json!(vec![true]), &Context::default());
        assert_eq!(result, Some(Value::String("list".into())));
    }

    #[test]
    fn type_name() {
        let selector = GraphQLSelector::TypeName {
            type_name: TypeName::String,
        };
        let result =
            selector.on_response_field(&ty(), field(), &json!("true"), &Context::default());
        assert_eq!(result, Some(Value::String("type_name".into())));
    }

    #[test]
    fn static_field() {
        let selector = GraphQLSelector::StaticField {
            r#static: "static_value".into(),
        };
        let result = selector.on_response_field(&ty(), field(), &json!(true), &Context::default());
        assert_eq!(result, Some(Value::String("static_value".into())));
    }

    #[test]
    fn operation_name() {
        let selector = GraphQLSelector::OperationName {
            operation_name: OperationName::String,
            default: None,
        };
        let ctx = Context::default();
        let _ = ctx.insert(OPERATION_NAME, "some-operation".to_string());
        let result = selector.on_response_field(&ty(), field(), &json!(true), &ctx);
        assert_eq!(result, Some(Value::String("some-operation".into())));
    }

    #[test]
    fn operation_name_hash() {
        let selector = GraphQLSelector::OperationName {
            operation_name: OperationName::Hash,
            default: None,
        };
        let ctx = Context::default();
        let _ = ctx.insert(OPERATION_NAME, "some-operation".to_string());
        let result = selector.on_response_field(&ty(), field(), &json!(true), &ctx);
        assert_eq!(
            result,
            Some(Value::String(
                "1d507f770a74cffd6cb014b190ea31160d442ff41d9bde088b634847aeafaafd".into()
            ))
        );
    }

    #[test]
    fn operation_name_defaulted() {
        let selector = GraphQLSelector::OperationName {
            operation_name: OperationName::String,
            default: Some("no-operation".to_string()),
        };
        let result = selector.on_response_field(&ty(), field(), &json!(true), &Context::default());
        assert_eq!(result, Some(Value::String("no-operation".into())));
    }
}
