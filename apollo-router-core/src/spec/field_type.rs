use crate::*;
use apollo_parser::ast;

#[derive(Debug)]
pub(crate) struct InvalidValue;

// Primitives are taken from scalars: https://spec.graphql.org/draft/#sec-Scalars
#[derive(Debug)]
pub(crate) enum FieldType {
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
            (FieldType::Named(name), _) if schema.custom_scalars.contains(name) => Ok(()),
            (FieldType::Named(name), value) if value.is_object() => {
                if let Some(object_ty) = schema.object_types.get(name) {
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

    pub(crate) fn filter_errors(
        &self,
        value: &mut Value,
        schema: &Schema,
    ) -> Result<(), InvalidValue> {
        println!(
            "will test type {:?} for value {:?}",
            self,
            serde_json::to_string(&value).unwrap()
        );
        // for every type, if we have an invalid value, we will replace it with null
        // and return Ok(()), because values are optional by default
        // here we match only on the field type and not the value, to avoid a move of
        // the &mut Value, because we would not be able to set it to null afterwards
        let res = match self {
            // for non null types, we validate with the inner type, then if we get an InvalidValue
            // we set it to null and immediately return an error instead of Ok(()), because we
            // want the error to go up until the next nullable parent
            FieldType::NonNull(inner_type) => {
                if value.is_null() {
                    println!("passed null to NonNull, returning invalidvalue");
                    return Err(InvalidValue);
                } else {
                    match inner_type.filter_errors(value, schema) {
                        Ok(()) => {
                            return if value.is_null() {
                                Err(InvalidValue)
                            } else {
                                Ok(())
                            };
                        }
                        Err(InvalidValue) => {
                            println!("nonnull got invalid value");
                            *value = Value::Null;
                            return Err(InvalidValue);
                        }
                    }
                }
            }
            FieldType::List(inner_type) => {
                value.as_array_mut().ok_or(InvalidValue).and_then(|vec| {
                    // if the list contains nonnullable types, we will receive a Err(InvalidValue)
                    // and should replace the entire list with null
                    // if the types are nullable, the inner call to filter_errors will take care
                    // of setting the current entry to null
                    vec.iter_mut()
                        .try_for_each(|x| inner_type.filter_errors(x, schema))
                })
            }

            FieldType::Named(name) => {
                /*if let Some(o) = value.as_object_mut() {
                    let v =
                } else {
                    Err(InvalidValue)
                }*/

                value
                    .as_object_mut()
                    .ok_or(InvalidValue)
                    .and_then(|object| {
                        if let Some(object_type) = schema.object_types.get(name) {
                            let r = object_type
                                .filter_errors(object, schema)
                                .map_err(|_| InvalidValue);

                            println!("res for object {}: {:?}", name, r);
                            r
                        } else {
                            Err(InvalidValue)
                        }
                    })
            }

            // the rest of the possible types just need to validate the expected value
            FieldType::Int => {
                let opt = if value.is_i64() {
                    value.as_i64().and_then(|i| i32::try_from(i).ok())
                } else if value.is_u64() {
                    value.as_i64().and_then(|i| i32::try_from(i).ok())
                } else {
                    None
                };
                opt.map(|_| ()).ok_or(InvalidValue)
            }
            FieldType::Float => value.as_f64().map(|_| ()).ok_or(InvalidValue),
            FieldType::Boolean => value.as_bool().map(|_| ()).ok_or(InvalidValue),
            FieldType::String => value.as_str().map(|_| ()).ok_or(InvalidValue),
            FieldType::Id => {
                if value.is_string() || value.is_i64() || value.is_u64() || value.is_f64() {
                    Ok(())
                } else {
                    Err(InvalidValue)
                }
            }
        };

        println!("res: {:?}", res);
        if res.is_err() {
            *value = Value::Null;
        }

        Ok(())
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
