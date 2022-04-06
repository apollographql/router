use crate::*;
use apollo_parser::ast;

#[derive(Debug)]
pub(crate) struct InvalidValue;

// Primitives are taken from scalars: https://spec.graphql.org/draft/#sec-Scalars
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum FieldType {
    /// Only used for introspection queries when types are prefixed by __
    Introspection(String),
    Named(String),
    List(Box<FieldType>),
    NonNull(Box<FieldType>),
    String,
    Int,
    Float,
    Id,
    Boolean,
}

impl FieldType {
    pub(crate) fn validate_value(
        &self,
        value: &Value,
        schema: &Schema,
    ) -> Result<(), InvalidValue> {
        match (self, value) {
            // Type coercion from string to Int, Float or Boolean
            (FieldType::Int | FieldType::Float | FieldType::Boolean, Value::String(s)) => {
                if let Ok(value) = Value::from_bytes(s.inner().clone()) {
                    self.validate_value(&value, schema)
                } else {
                    Err(InvalidValue)
                }
            }
            (FieldType::String, Value::String(_)) => Ok(()),
            // Spec: https://spec.graphql.org/June2018/#sec-Int
            (FieldType::Int, Value::Number(number)) if number.is_i64() || number.is_u64() => {
                if number
                    .as_i64()
                    .and_then(|x| i32::try_from(x).ok())
                    .is_some()
                    || number
                        .as_u64()
                        .and_then(|x| i32::try_from(x).ok())
                        .is_some()
                {
                    Ok(())
                } else {
                    Err(InvalidValue)
                }
            }
            // Spec: https://spec.graphql.org/draft/#sec-Float
            (FieldType::Float, Value::Number(number)) if number.is_f64() => Ok(()),
            // "The ID scalar type represents a unique identifier, often used to refetch an object
            // or as the key for a cache. The ID type is serialized in the same way as a String;
            // however, it is not intended to be human-readable. While it is often numeric, it
            // should always serialize as a String."
            //
            // In practice it seems Int works too
            (FieldType::Id, Value::String(_) | Value::Number(_)) => Ok(()),
            (FieldType::Boolean, Value::Bool(_)) => Ok(()),
            (FieldType::List(inner_ty), Value::Array(vec)) => vec
                .iter()
                .try_for_each(|x| inner_ty.validate_value(x, schema)),
            (FieldType::NonNull(inner_ty), value) => {
                if value.is_null() {
                    Err(InvalidValue)
                } else {
                    inner_ty.validate_value(value, schema)
                }
            }
            (FieldType::Named(name), _)
                if schema.custom_scalars.contains(name) || schema.enums.contains_key(name) =>
            {
                Ok(())
            }
            (FieldType::Named(name), value) if value.is_object() => {
                if let Some(object_ty) = schema.input_types.get(name) {
                    object_ty
                        .validate_object(value.as_object().unwrap(), schema)
                        .map_err(|_| InvalidValue)
                } else {
                    Err(InvalidValue)
                }
            }
            // NOTE: graphql's types are all optional by default
            (_, Value::Null) => Ok(()),
            _ => Err(InvalidValue),
        }
    }

    /// return the name of the type on which selections happen
    ///
    /// Example if we get the field `list: [User!]!`, it will return "User"
    pub fn inner_type_name(&self) -> Option<&str> {
        match self {
            FieldType::Named(name) | FieldType::Introspection(name) => Some(name.as_str()),
            FieldType::List(inner) | FieldType::NonNull(inner) => inner.inner_type_name(),
            FieldType::String
            | FieldType::Int
            | FieldType::Float
            | FieldType::Id
            | FieldType::Boolean => None,
        }
    }

    pub fn is_builtin_scalar(&self) -> bool {
        match self {
            FieldType::Named(_)
            | FieldType::Introspection(_)
            | FieldType::List(_)
            | FieldType::NonNull(_) => false,
            FieldType::String
            | FieldType::Int
            | FieldType::Float
            | FieldType::Id
            | FieldType::Boolean => true,
        }
    }

    pub fn is_non_null(&self) -> bool {
        matches!(self, FieldType::NonNull(_))
    }
}

impl From<ast::Type> for FieldType {
    // Spec: https://spec.graphql.org/draft/#sec-Type-References
    fn from(ty: ast::Type) -> Self {
        match ty {
            ast::Type::NamedType(named) => named.into(),
            ast::Type::ListType(list) => list.into(),
            ast::Type::NonNullType(non_null) => non_null.into(),
        }
    }
}

impl From<ast::NamedType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#NamedType
    fn from(named: ast::NamedType) -> Self {
        let name = named
            .name()
            .expect("the node Name is not optional in the spec; qed")
            .text()
            .to_string();
        match name.as_str() {
            "String" => Self::String,
            "Int" => Self::Int,
            "Float" => Self::Float,
            "ID" => Self::Id,
            "Boolean" => Self::Boolean,
            _ => Self::Named(name),
        }
    }
}

impl From<ast::ListType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#ListType
    fn from(list: ast::ListType) -> Self {
        Self::List(Box::new(
            list.ty()
                .expect("the node Type is not optional in the spec; qed")
                .into(),
        ))
    }
}

impl From<ast::NonNullType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#NonNullType
    fn from(non_null: ast::NonNullType) -> Self {
        if let Some(list) = non_null.list_type() {
            Self::NonNull(Box::new(list.into()))
        } else if let Some(named) = non_null.named_type() {
            Self::NonNull(Box::new(named.into()))
        } else {
            eprintln!("{:?}", non_null);
            eprintln!("{:?}", non_null.to_string());
            unreachable!("either the NamedType node is provided, either the ListType node; qed")
        }
    }
}
