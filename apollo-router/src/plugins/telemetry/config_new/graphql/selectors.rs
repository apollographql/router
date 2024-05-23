use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::demand_control::cost_calculator::schema_aware_response::TypedValue;
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
        _ctx: &Context,
    ) -> Option<opentelemetry::Value> {
        match self {
            GraphQLSelector::ListLength { .. } => match typed_value {
                TypedValue::List(_, _, array) => Some((array.len() as i64).into()),
                _ => None,
            },
            GraphQLSelector::FieldName { .. } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(_, f, _)
                | TypedValue::Number(_, f, _)
                | TypedValue::String(_, f, _)
                | TypedValue::List(_, f, _)
                | TypedValue::Object(_, f, _) => Some(f.name.to_string().into()),
                TypedValue::Root(_) => None,
            },
            GraphQLSelector::FieldType {
                field_type: FieldType::Name,
            } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(_, f, _)
                | TypedValue::Number(_, f, _)
                | TypedValue::String(_, f, _)
                | TypedValue::List(_, f, _)
                | TypedValue::Object(_, f, _) => {
                    Some(f.definition.ty.inner_named_type().to_string().into())
                }
                TypedValue::Root(_) => None,
            },
            GraphQLSelector::FieldType {
                field_type: FieldType::Type,
            } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(_, _, _) => Some("scalar".into()),
                TypedValue::Number(_, _, _) => Some("scalar".into()),
                TypedValue::String(_, _, _) => Some("scalar".into()),
                TypedValue::Object(_, _, _) => Some("object".into()),
                TypedValue::List(_, _, _) => Some("list".into()),
                TypedValue::Root(_) => Some("object".into()),
            },
            GraphQLSelector::TypeName { .. } => match typed_value {
                TypedValue::Null => None,
                TypedValue::Bool(ty, _, _)
                | TypedValue::Number(ty, _, _)
                | TypedValue::String(ty, _, _)
                | TypedValue::List(ty, _, _)
                | TypedValue::Object(ty, _, _) => Some(ty.to_string().into()),
                TypedValue::Root(_) => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::Value;

    use super::*;
    use crate::plugins::telemetry::config_new::test::field;
    use crate::plugins::telemetry::config_new::test::ty;

    #[test]
    fn array_length() {
        let selector = GraphQLSelector::ListLength {
            list_length: ListLength::Value,
        };
        let typed_value = TypedValue::List(
            ty(),
            field(),
            vec![
                TypedValue::Bool(ty(), field(), &true),
                TypedValue::Bool(ty(), field(), &true),
                TypedValue::Bool(ty(), field(), &true),
            ],
        );
        let result = selector.on_response_field(&typed_value, &Context::default());
        assert_eq!(result, Some(Value::I64(3)));
    }

    #[test]
    fn field_name() {
        let selector = GraphQLSelector::FieldName {
            field_name: FieldName::String,
        };
        let typed_value = TypedValue::Bool(ty(), field(), &true);
        let result = selector.on_response_field(&typed_value, &Context::default());
        assert_eq!(result, Some(Value::String("field_name".into())));
    }

    #[test]
    fn field_type() {
        let selector = GraphQLSelector::FieldType {
            field_type: FieldType::Name,
        };
        let typed_value = TypedValue::Bool(ty(), field(), &true);
        let result = selector.on_response_field(&typed_value, &Context::default());
        assert_eq!(result, Some(Value::String("field_type".into())));
    }

    #[test]
    fn field_type_scalar_type() {
        assert_scalar(&TypedValue::String(ty(), field(), "value"));
        assert_scalar(&TypedValue::Number(
            ty(),
            field(),
            &serde_json::Number::from(1),
        ));
    }

    fn assert_scalar(typed_value: &TypedValue) {
        let result = GraphQLSelector::FieldType {
            field_type: FieldType::Type,
        }
        .on_response_field(typed_value, &Context::default());
        assert_eq!(result, Some(Value::String("scalar".into())));
    }

    #[test]
    fn field_type_object_type() {
        let selector = GraphQLSelector::FieldType {
            field_type: FieldType::Type,
        };
        let typed_value = TypedValue::Object(ty(), field(), [].into());
        let result = selector.on_response_field(&typed_value, &Context::default());
        assert_eq!(result, Some(Value::String("object".into())));
    }

    #[test]
    fn field_type_root_object_type() {
        let selector = GraphQLSelector::FieldType {
            field_type: FieldType::Type,
        };
        let typed_value = TypedValue::Root(Default::default());
        let result = selector.on_response_field(&typed_value, &Context::default());
        assert_eq!(result, Some(Value::String("object".into())));
    }

    #[test]
    fn field_type_list_type() {
        let selector = GraphQLSelector::FieldType {
            field_type: FieldType::Type,
        };
        let typed_value =
            TypedValue::List(ty(), field(), vec![TypedValue::Bool(ty(), field(), &true)]);
        let result = selector.on_response_field(&typed_value, &Context::default());
        assert_eq!(result, Some(Value::String("list".into())));
    }

    #[test]
    fn type_name() {
        let selector = GraphQLSelector::TypeName {
            type_name: TypeName::String,
        };
        let typed_value = TypedValue::Bool(ty(), field(), &true);
        let result = selector.on_response_field(&typed_value, &Context::default());
        assert_eq!(result, Some(Value::String("type_name".into())));
    }
}
