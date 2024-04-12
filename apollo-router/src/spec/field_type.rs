use apollo_compiler::ast;
use apollo_compiler::schema;
use serde::de::Error as _;
use serde::Deserialize;
use serde::Serialize;

use super::query::parse_hir_value;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::spec::Schema;

#[derive(Debug)]
pub(crate) struct InvalidValue;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FieldType(pub(crate) schema::Type);

// schema::Type does not implement Serialize or Deserialize,
// and <https://serde.rs/remote-derive.html> seems not to work for recursive types.
// Instead have explicit `impl`s that are based on derived impl of purpose-built types.

impl Serialize for FieldType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        struct BorrowedFieldType<'a>(&'a schema::Type);

        impl<'a> Serialize for BorrowedFieldType<'a> {
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

fn validate_input_value(
    ty: &schema::Type,
    value: &Value,
    schema: &Schema,
) -> Result<(), InvalidValue> {
    if value.is_null() {
        return match ty {
            schema::Type::Named(_) | schema::Type::List(_) => Ok(()),
            schema::Type::NonNullNamed(_) | schema::Type::NonNullList(_) => Err(InvalidValue),
        };
    }
    let type_name = match ty {
        schema::Type::Named(name) | schema::Type::NonNullNamed(name) => name,
        schema::Type::List(inner_type) | schema::Type::NonNullList(inner_type) => {
            return if let Value::Array(vec) = value {
                vec.iter()
                    .try_for_each(|x| validate_input_value(inner_type, x, schema))
            } else {
                // For coercion from single value to list
                validate_input_value(inner_type, value, schema)
            };
        }
    };
    let from_bool = |condition| if condition { Ok(()) } else { Err(InvalidValue) };
    match type_name.as_str() {
        "String" => return from_bool(value.is_string()),
        // Spec: https://spec.graphql.org/June2018/#sec-Int
        "Int" => return from_bool(value.is_valid_int_input()),
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
        .ok_or(InvalidValue)?;
    match (type_def, value) {
        // Custom scalar: accept any JSON value
        (schema::ExtendedType::Scalar(_), _) => Ok(()),

        // TODO: check enum value?
        // (schema::ExtendedType::Enum(def), Value::String(s)) => {
        //     from_bool(def.values.contains_key(s))
        // },
        (schema::ExtendedType::Enum(_), _) => Ok(()),

        (schema::ExtendedType::InputObject(def), Value::Object(obj)) => {
            // TODO: check keys in `obj` but not in `def.fields`?
            def.fields
                .values()
                .try_for_each(|field| match obj.get(field.name.as_str()) {
                    Some(&Value::Null) | None => {
                        let default = field
                            .default_value
                            .as_ref()
                            .and_then(|v| parse_hir_value(v))
                            .unwrap_or(Value::Null);
                        validate_input_value(&field.ty, &default, schema)
                    }
                    Some(value) => validate_input_value(&field.ty, value, schema),
                })
        }
        _ => Err(InvalidValue),
    }
}

impl FieldType {
    pub(crate) fn new_named(name: ast::Name) -> Self {
        Self(schema::Type::Named(name))
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

    pub(crate) fn is_non_null(&self) -> bool {
        self.0.is_non_null()
    }
}

impl From<&'_ schema::Type> for FieldType {
    fn from(ty: &'_ schema::Type) -> Self {
        Self(ty.clone())
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
