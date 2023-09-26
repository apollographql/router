use apollo_compiler::hir;
use serde::Deserialize;
use serde::Serialize;

use super::query::parse_hir_value;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::spec::Schema;

#[derive(Debug)]
pub(crate) struct InvalidValue;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FieldType(pub(crate) hir::Type);

// hir::Type does not implement Serialize or Deserialize,
// and <https://serde.rs/remote-derive.html> seems not to work for recursive types.
// Instead have explicit `impl`s that are based on derived impl of purpose-built types.

impl Serialize for FieldType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        struct BorrowedFieldType<'a>(&'a hir::Type);

        impl<'a> Serialize for BorrowedFieldType<'a> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                #[derive(Serialize)]
                enum NestedBorrowed<'a> {
                    NonNull(BorrowedFieldType<'a>),
                    List(BorrowedFieldType<'a>),
                    Named(&'a str),
                }
                match &self.0 {
                    hir::Type::NonNull { ty, .. } => NestedBorrowed::NonNull(BorrowedFieldType(ty)),
                    hir::Type::List { ty, .. } => NestedBorrowed::List(BorrowedFieldType(ty)),
                    hir::Type::Named { name, .. } => NestedBorrowed::Named(name),
                }
                .serialize(serializer)
            }
        }

        BorrowedFieldType(&self.0).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FieldType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        enum WithoutLocation {
            NonNull(FieldType),
            List(FieldType),
            Named(String),
        }
        WithoutLocation::deserialize(deserializer).map(|ty| match ty {
            WithoutLocation::NonNull(ty) => FieldType(hir::Type::NonNull {
                ty: Box::new(ty.0),
                loc: None,
            }),
            WithoutLocation::List(ty) => FieldType(hir::Type::List {
                ty: Box::new(ty.0),
                loc: None,
            }),
            WithoutLocation::Named(name) => FieldType(hir::Type::Named { name, loc: None }),
        })
    }
}

impl std::fmt::Display for FieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

fn validate_input_value(
    ty: &hir::Type,
    value: &Value,
    schema: &Schema,
) -> Result<(), InvalidValue> {
    match (ty, value) {
        (hir::Type::Named { name, .. }, Value::String(_)) if name == "String" => Ok(()),
        // Spec: https://spec.graphql.org/June2018/#sec-Int
        (hir::Type::Named { name, .. }, maybe_int) if name == "Int" => {
            if maybe_int == &Value::Null || maybe_int.is_valid_int_input() {
                Ok(())
            } else {
                Err(InvalidValue)
            }
        }
        // Spec: https://spec.graphql.org/draft/#sec-Float.Input-Coercion
        (hir::Type::Named { name, .. }, maybe_float) if name == "Float" => {
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
        (hir::Type::Named { name, .. }, Value::String(_)) if name == "ID" => Ok(()),
        (hir::Type::Named { name, .. }, value) if name == "ID" => {
            if value == &Value::Null || value.is_valid_id_input() {
                Ok(())
            } else {
                Err(InvalidValue)
            }
        }
        (hir::Type::Named { name, .. }, Value::Bool(_)) if name == "Boolean" => Ok(()),
        (hir::Type::List { ty: inner_ty, .. }, Value::Array(vec)) => vec
            .iter()
            .try_for_each(|x| validate_input_value(inner_ty, x, schema)),
        // For coercion from single value to list
        (hir::Type::List { ty: inner_ty, .. }, val) if val != &Value::Null => {
            validate_input_value(inner_ty, val, schema)
        }
        (hir::Type::NonNull { ty: inner_ty, .. }, value) => {
            if value.is_null() {
                Err(InvalidValue)
            } else {
                validate_input_value(inner_ty, value, schema)
            }
        }
        (hir::Type::Named { name, .. }, _)
            if schema
                .type_system
                .definitions
                .scalars
                .get(name)
                .map(|def| !def.is_built_in())
                .unwrap_or(false)
                || schema.type_system.definitions.enums.contains_key(name) =>
        {
            Ok(())
        }
        // NOTE: graphql's types are all optional by default
        (_, Value::Null) => Ok(()),
        (hir::Type::Named { name, .. }, value) => {
            if let Some(value) = value.as_object() {
                if let Some(object_ty) = schema.type_system.definitions.input_objects.get(name) {
                    object_ty.fields().try_for_each(|field| {
                        let default: Value;
                        let value = match value.get(field.name()) {
                            Some(&Value::Null) | None => {
                                default = field
                                    .default_value()
                                    .and_then(parse_hir_value)
                                    .unwrap_or(Value::Null);
                                &default
                            }
                            Some(value) => value,
                        };
                        validate_input_value(field.ty(), value, schema)
                    })
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

/// TODO: make `hir::Type::name` return &str instead of String
fn hir_type_name(ty: &hir::Type) -> &str {
    match ty {
        hir::Type::NonNull { ty, .. } => hir_type_name(ty),
        hir::Type::List { ty, .. } => hir_type_name(ty),
        hir::Type::Named { name, .. } => name,
    }
}

impl FieldType {
    pub(crate) fn new_named(name: impl Into<String>) -> Self {
        Self(hir::Type::Named {
            name: name.into(),
            loc: None,
        })
    }

    // This function validates input values according to the graphql specification.
    // Each of the values are validated against the "input coercion" rules.
    pub(crate) fn validate_input_value(
        &self,
        value: &Value,
        schema: &Schema,
    ) -> Result<(), InvalidValue> {
        validate_input_value(&self.0, value, schema)
    }

    /// return the name of the type on which selections happen
    ///
    /// Example if we get the field `list: [User!]!`, it will return "User"
    pub(crate) fn inner_type_name(&self) -> Option<&str> {
        let name = hir_type_name(&self.0);
        if is_built_in(name) {
            None // TODO: is this case important or could this method return unconditional `&str`?
        } else {
            Some(name)
        }
    }

    pub(crate) fn is_builtin_scalar(&self) -> bool {
        match &self.0 {
            hir::Type::NonNull { .. } => false,
            hir::Type::List { .. } => false,
            hir::Type::Named { name, .. } => is_built_in(name),
        }
    }

    pub(crate) fn is_non_null(&self) -> bool {
        self.0.is_non_null()
    }
}

fn is_built_in(name: &str) -> bool {
    matches!(name, "String" | "Int" | "Float" | "ID" | "Boolean")
}

impl From<&'_ hir::Type> for FieldType {
    fn from(ty: &'_ hir::Type) -> Self {
        Self(ty.clone())
    }
}

/// Make sure custom Serialize and Deserialize impls are compatible with each other
#[test]
fn test_field_type_serialization() {
    let ty = FieldType(hir::Type::NonNull {
        ty: Box::new(hir::Type::List {
            ty: Box::new(hir::Type::Named {
                name: "ID".into(),
                loc: None,
            }),
            loc: None,
        }),
        loc: None,
    });
    assert_eq!(
        serde_json::from_str::<FieldType>(&serde_json::to_string(&ty).unwrap()).unwrap(),
        ty
    )
}
