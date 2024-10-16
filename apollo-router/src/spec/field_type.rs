use apollo_compiler::Name;
use apollo_compiler::schema;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Error as _;

use super::query::parse_hir_value;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::spec::Schema;

#[derive(Debug)]
pub(crate) struct InvalidValue;

/// {0}
#[derive(thiserror::Error, displaydoc::Display, Debug, Clone, Serialize, Eq, PartialEq)]
pub(crate) struct InvalidInputValue(pub(crate) String);

fn describe_json_value(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "map",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FieldType(pub(crate) schema::Type);

/// A path within a JSON object that doesn’t need heap allocation in the happy path
pub(crate) enum JsonValuePath<'a> {
    Variable {
        name: &'a str,
    },
    ObjectKey {
        key: &'a str,
        parent: &'a JsonValuePath<'a>,
    },
    ArrayItem {
        index: usize,
        parent: &'a JsonValuePath<'a>,
    },
}

// schema::Type does not implement Serialize or Deserialize,
// and <https://serde.rs/remote-derive.html> seems not to work for recursive types.
// Instead have explicit `impl`s that are based on derived impl of purpose-built types.

impl Serialize for FieldType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        struct BorrowedFieldType<'a>(&'a schema::Type);

        impl Serialize for BorrowedFieldType<'_> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                #[derive(Serialize)]
                enum NestedBorrowed<'a> {
                    Named(&'a str),
                    NonNullNamed(&'a str),
                    List(BorrowedFieldType<'a>),
                    NonNullList(BorrowedFieldType<'a>),
                }
                match &self.0 {
                    schema::Type::Named(name) => NestedBorrowed::Named(name),
                    schema::Type::NonNullNamed(name) => NestedBorrowed::NonNullNamed(name),
                    schema::Type::List(ty) => NestedBorrowed::List(BorrowedFieldType(ty)),
                    schema::Type::NonNullList(ty) => {
                        NestedBorrowed::NonNullList(BorrowedFieldType(ty))
                    }
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
            Named(String),
            NonNullNamed(String),
            List(FieldType),
            NonNullList(FieldType),
        }
        Ok(match WithoutLocation::deserialize(deserializer)? {
            WithoutLocation::Named(name) => FieldType(schema::Type::Named(
                name.try_into().map_err(D::Error::custom)?,
            )),
            WithoutLocation::NonNullNamed(name) => FieldType(
                schema::Type::Named(name.try_into().map_err(D::Error::custom)?).non_null(),
            ),
            WithoutLocation::List(ty) => FieldType(ty.0.list()),
            WithoutLocation::NonNullList(ty) => FieldType(ty.0.list().non_null()),
        })
    }
}

impl std::fmt::Display for FieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// This function currently stops at the first error it finds.
/// It may be nicer to return a `Vec` of errors, but its size should be limited
/// in case e.g. every item of a large array is invalid.
fn validate_input_value(
    ty: &schema::Type,
    value: Option<&Value>,
    schema: &Schema,
    path: &JsonValuePath<'_>,
) -> Result<(), InvalidInputValue> {
    let fmt_path = || match path {
        JsonValuePath::Variable { .. } => format!("variable `{path}`"),
        _ => format!("input value at `{path}`"),
    };
    let Some(value) = value else {
        if ty.is_non_null() {
            return Err(InvalidInputValue(format!(
                "missing {}: for required GraphQL type `{ty}`",
                fmt_path(),
            )));
        } else {
            return Ok(());
        }
    };
    let invalid = || {
        InvalidInputValue(format!(
            "invalid {}: found JSON {} for GraphQL type `{ty}`",
            fmt_path(),
            describe_json_value(value)
        ))
    };
    if value.is_null() {
        if ty.is_non_null() {
            return Err(invalid());
        } else {
            return Ok(());
        }
    }
    let type_name = match ty {
        schema::Type::Named(name) | schema::Type::NonNullNamed(name) => name,
        schema::Type::List(inner_type) | schema::Type::NonNullList(inner_type) => {
            if let Value::Array(vec) = value {
                for (i, x) in vec.iter().enumerate() {
                    let path = JsonValuePath::ArrayItem {
                        index: i,
                        parent: path,
                    };
                    validate_input_value(inner_type, Some(x), schema, &path)?
                }
                return Ok(());
            } else {
                // For coercion from single value to list
                return validate_input_value(inner_type, Some(value), schema, path);
            }
        }
    };
    let from_bool = |condition| {
        if condition { Ok(()) } else { Err(invalid()) }
    };
    match type_name.as_str() {
        "String" => return from_bool(value.is_string()),
        // Spec: https://spec.graphql.org/June2018/#sec-Int
        "Int" => {
            let valid = value.is_valid_int_input();

            if value.as_i32().is_none() {
                tracing::warn!(
                    "Input INT '{}' is larger than 32-bits and is not GraphQL spec-compliant.",
                    value.to_string()
                )
            }

            return from_bool(valid);
        }
        // Spec: https://spec.graphql.org/draft/#sec-Float.Input-Coercion
        "Float" => return from_bool(value.is_valid_float_input()),
        // "The ID scalar type represents a unique identifier, often used to refetch an object
        // or as the key for a cache. The ID type is serialized in the same way as a String;
        // however, it is not intended to be human-readable. While it is often numeric, it
        // should always serialize as a String."
        //
        // In practice it seems Int works too
        "ID" => return from_bool(value.is_valid_id_input()),
        "Boolean" => return from_bool(value.is_boolean()),
        _ => {}
    }
    let type_def = schema
        .supergraph_schema()
        .types
        .get(type_name)
        // Should never happen in a valid schema
        .ok_or_else(invalid)?;
    match (type_def, value) {
        // Custom scalar: accept any JSON value
        (schema::ExtendedType::Scalar(_), _) => Ok(()),

        (schema::ExtendedType::Enum(def), Value::String(s)) => {
            from_bool(def.values.contains_key(s.as_str()))
        }
        (schema::ExtendedType::Enum(_), _) => Err(invalid()),

        (schema::ExtendedType::InputObject(def), Value::Object(obj)) => {
            // TODO: check keys in `obj` but not in `def.fields`?
            def.fields.values().try_for_each(|field| {
                let path = JsonValuePath::ObjectKey {
                    key: &field.name,
                    parent: path,
                };
                match obj.get(field.name.as_str()) {
                    Some(&Value::Null) | None => {
                        let default = field
                            .default_value
                            .as_ref()
                            .and_then(|v| parse_hir_value(v));
                        validate_input_value(&field.ty, default.as_ref(), schema, &path)
                    }
                    value => validate_input_value(&field.ty, value, schema, &path),
                }
            })
        }
        _ => Err(invalid()),
    }
}

impl FieldType {
    pub(crate) fn new_named(name: Name) -> Self {
        Self(schema::Type::Named(name))
    }

    // This function validates input values according to the graphql specification.
    // Each of the values are validated against the "input coercion" rules.
    pub(crate) fn validate_input_value(
        &self,
        value: Option<&Value>,
        schema: &Schema,
        path: &JsonValuePath<'_>,
    ) -> Result<(), InvalidInputValue> {
        validate_input_value(&self.0, value, schema, path)
    }

    pub(crate) fn is_non_null(&self) -> bool {
        self.0.is_non_null()
    }
}

impl From<&'_ schema::Type> for FieldType {
    fn from(ty: &'_ schema::Type) -> Self {
        Self(ty.clone())
    }
}

impl std::fmt::Display for JsonValuePath<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Variable { name } => {
                f.write_str("$")?;
                f.write_str(name)
            }
            Self::ObjectKey { key, parent } => {
                parent.fmt(f)?;
                f.write_str(".")?;
                f.write_str(key)
            }
            Self::ArrayItem { index, parent } => {
                parent.fmt(f)?;
                write!(f, "[{index}]")
            }
        }
    }
}

/// Make sure custom Serialize and Deserialize impls are compatible with each other
#[test]
fn test_field_type_serialization() {
    let ty = FieldType(apollo_compiler::ty!([ID]!));
    assert_eq!(
        serde_json::from_str::<FieldType>(&serde_json::to_string(&ty).unwrap()).unwrap(),
        ty
    )
}
