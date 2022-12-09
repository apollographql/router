use apollo_compiler::hir;
use apollo_parser::ast;
use serde::Deserialize;
use serde::Serialize;

use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::*;

#[derive(Debug)]
pub(crate) struct InvalidValue;

// Primitives are taken from scalars: https://spec.graphql.org/draft/#sec-Scalars
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum FieldType {
    /// Only used for introspection queries when types are prefixed by __
    Introspection(String),
    /// Named type {0}
    Named(String),
    /// List type {0}
    List(Box<FieldType>),
    /// Non null type {0}
    NonNull(Box<FieldType>),
    /// String
    String,
    /// Int
    Int,
    /// Float
    Float,
    /// Id
    Id,
    /// Boolean
    Boolean,
}

impl std::fmt::Display for FieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FieldType::Introspection(ty) | FieldType::Named(ty) => write!(f, "{ty}"),
            FieldType::List(ty) => write!(f, "[{ty}]"),
            FieldType::NonNull(ty) => write!(f, "{ty}!"),
            FieldType::String => write!(f, "String"),
            FieldType::Int => write!(f, "Int"),
            FieldType::Float => write!(f, "Float"),
            FieldType::Id => write!(f, "ID"),
            FieldType::Boolean => write!(f, "Boolean"),
        }
    }
}

impl FieldType {
    // This function validates input values according to the graphql specification.
    // Each of the values are validated against the "input coercion" rules.
    pub(crate) fn validate_input_value(
        &self,
        value: &Value,
        schema: &Schema,
    ) -> Result<(), InvalidValue> {
        match (self, value) {
            (FieldType::String, Value::String(_)) => Ok(()),
            // Spec: https://spec.graphql.org/June2018/#sec-Int
            (FieldType::Int, maybe_int) => {
                if maybe_int == &Value::Null || maybe_int.is_valid_int_input() {
                    Ok(())
                } else {
                    Err(InvalidValue)
                }
            }
            // Spec: https://spec.graphql.org/draft/#sec-Float.Input-Coercion
            (FieldType::Float, maybe_float) => {
                if maybe_float == &Value::Null || maybe_float.is_valid_float_input() {
                    Ok(())
                } else {
                    Err(InvalidValue)
                }
            }
            // "The ID scalar type represents a unique identifier, often used to refetch an object
            // or as the key for a cache. The ID type is serialized in the same way as a String;
            // however, it is not intended to be human-readable. While it is often numeric, it
            // should always serialize as a String."
            //
            // In practice it seems Int works too
            (FieldType::Id, Value::String(_)) => Ok(()),
            (FieldType::Id, maybe_int) => {
                if maybe_int == &Value::Null || maybe_int.is_valid_int_input() {
                    Ok(())
                } else {
                    Err(InvalidValue)
                }
            }
            (FieldType::Boolean, Value::Bool(_)) => Ok(()),
            (FieldType::List(inner_ty), Value::Array(vec)) => vec
                .iter()
                .try_for_each(|x| inner_ty.validate_input_value(x, schema)),
            // For coercion from single value to list
            (FieldType::List(inner_ty), val) if val != &Value::Null => {
                inner_ty.validate_input_value(val, schema)
            }
            (FieldType::NonNull(inner_ty), value) => {
                if value.is_null() {
                    Err(InvalidValue)
                } else {
                    inner_ty.validate_input_value(value, schema)
                }
            }
            (FieldType::Named(name), _)
                if schema.custom_scalars.contains(name) || schema.enums.contains_key(name) =>
            {
                Ok(())
            }
            // NOTE: graphql's types are all optional by default
            (_, Value::Null) => Ok(()),
            (FieldType::Named(name), value) => {
                if let Some(value) = value.as_object() {
                    if let Some(object_ty) = schema.input_types.get(name) {
                        object_ty
                            .validate_object(value, schema)
                            .map_err(|_| InvalidValue)
                    } else {
                        Err(InvalidValue)
                    }
                } else {
                    Err(InvalidValue)
                }
            }
            _ => Err(InvalidValue),
        }
    }

    /// return the name of the type on which selections happen
    ///
    /// Example if we get the field `list: [User!]!`, it will return "User"
    pub(crate) fn inner_type_name(&self) -> Option<&str> {
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

    pub(crate) fn is_builtin_scalar(&self) -> bool {
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

    pub(crate) fn is_non_null(&self) -> bool {
        matches!(self, FieldType::NonNull(_))
    }
}

impl From<&'_ hir::Type> for FieldType {
    fn from(ty: &'_ hir::Type) -> Self {
        match ty {
            hir::Type::NonNull { ty, .. } => Self::NonNull(Box::new((&**ty).into())),
            hir::Type::List { ty, .. } => Self::List(Box::new((&**ty).into())),
            hir::Type::Named { name, .. } => match name.as_str() {
                "String" => Self::String,
                "Int" => Self::Int,
                "Float" => Self::Float,
                "ID" => Self::Id,
                "Boolean" => Self::Boolean,
                _ => Self::Named(name.clone()),
            },
        }
    }
}

impl TryFrom<ast::Type> for FieldType {
    type Error = SpecError;
    // Spec: https://spec.graphql.org/draft/#sec-Type-References
    fn try_from(ty: ast::Type) -> Result<Self, Self::Error> {
        match ty {
            ast::Type::NamedType(named) => named.try_into(),
            ast::Type::ListType(list) => list.try_into(),
            ast::Type::NonNullType(non_null) => non_null.try_into(),
        }
    }
}

impl TryFrom<ast::NamedType> for FieldType {
    type Error = SpecError;
    // Spec: https://spec.graphql.org/draft/#NamedType
    fn try_from(named: ast::NamedType) -> Result<Self, Self::Error> {
        let name = named
            .name()
            .ok_or_else(|| {
                SpecError::InvalidType("the node Name is not optional in the spec; qed".to_string())
            })?
            .text()
            .to_string();
        Ok(match name.as_str() {
            "String" => Self::String,
            "Int" => Self::Int,
            "Float" => Self::Float,
            "ID" => Self::Id,
            "Boolean" => Self::Boolean,
            _ => Self::Named(name),
        })
    }
}

impl TryFrom<ast::ListType> for FieldType {
    type Error = SpecError;

    // Spec: https://spec.graphql.org/draft/#ListType
    fn try_from(list: ast::ListType) -> Result<Self, Self::Error> {
        Ok(Self::List(Box::new(
            list.ty()
                .ok_or_else(|| {
                    SpecError::InvalidType("node Type is not optional in the spec; qed".to_string())
                })?
                .try_into()?,
        )))
    }
}

impl TryFrom<ast::NonNullType> for FieldType {
    type Error = SpecError;

    // Spec: https://spec.graphql.org/draft/#NonNullType
    fn try_from(non_null: ast::NonNullType) -> Result<Self, Self::Error> {
        if let Some(list) = non_null.list_type() {
            Ok(Self::NonNull(Box::new(list.try_into()?)))
        } else if let Some(named) = non_null.named_type() {
            Ok(Self::NonNull(Box::new(named.try_into()?)))
        } else {
            Err(SpecError::InvalidType(
                "either the NamedType node is provided, either the ListType node; qed".to_string(),
            ))
        }
    }
}
