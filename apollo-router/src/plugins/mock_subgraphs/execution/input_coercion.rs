use crate::ast::Type;
use crate::ast::Value;
use crate::collections::HashMap;
use crate::executable::Field;
use crate::executable::Operation;
use crate::execution::engine::LinkedPath;
use crate::execution::engine::PropagateNull;
use crate::parser::SourceMap;
use crate::parser::SourceSpan;
use crate::response::GraphQLError;
use crate::response::JsonMap;
use crate::response::JsonValue;
use crate::schema::ExtendedType;
use crate::schema::FieldDefinition;
use crate::validation::SuspectedValidationBug;
use crate::validation::Valid;
use crate::ExecutableDocument;
use crate::Node;
use crate::Schema;

#[derive(Debug, Clone)]
pub(crate) enum InputCoercionError {
    SuspectedValidationBug(SuspectedValidationBug),
    // TODO: split into more structured variants?
    ValueError {
        message: String,
        location: Option<SourceSpan>,
    },
}

// Documented in `src/request.rs`
pub(crate) fn coerce_variable_values(
    schema: &Valid<Schema>,
    operation: &Operation,
    values: &JsonMap,
) -> Result<Valid<JsonMap>, InputCoercionError> {
    let mut coerced_values = JsonMap::new();
    for variable_def in &operation.variables {
        let name = variable_def.name.as_str();
        if let Some((key, value)) = values.get_key_value(name) {
            let value =
                coerce_variable_value(schema, "variable", "", "", name, &variable_def.ty, value)?;
            coerced_values.insert(key.clone(), value);
        } else if let Some(default) = &variable_def.default_value {
            let value = graphql_value_to_json("variable default value", "", "", name, default)?;
            coerced_values.insert(name, value);
        } else if variable_def.ty.is_non_null() {
            return Err(InputCoercionError::ValueError {
                message: format!("missing value for non-null variable '{name}'"),
                location: variable_def.location(),
            });
        } else {
            // Nullable variable with no provided value nor explicit default.
            // Spec says nothing for this case, but for the similar case in input objects:
            //
            // > there is a semantic difference between the explicitly provided value null
            // > versus having not provided a value
        }
    }
    Ok(Valid(coerced_values))
}

#[allow(clippy::too_many_arguments)] // yes it’s not a nice API but it’s internal
fn coerce_variable_value(
    schema: &Valid<Schema>,
    kind: &str,
    parent: &str,
    sep: &str,
    name: &str,
    ty: &Type,
    value: &JsonValue,
) -> Result<JsonValue, InputCoercionError> {
    if value.is_null() {
        if ty.is_non_null() {
            return Err(InputCoercionError::ValueError {
                message: format!("null value for {kind} {parent}{sep}{name} of non-null type {ty}"),
                location: None,
            });
        } else {
            return Ok(JsonValue::Null);
        }
    }
    let ty_name = match ty {
        Type::List(inner) | Type::NonNullList(inner) => {
            // https://spec.graphql.org/October2021/#sec-List.Input-Coercion
            return value
                .as_array()
                .map(Vec::as_slice)
                // If not an array, treat the value as an array of size one:
                .unwrap_or(std::slice::from_ref(value))
                .iter()
                .map(|item| coerce_variable_value(schema, kind, parent, sep, name, inner, item))
                .collect();
        }
        Type::Named(ty_name) | Type::NonNullNamed(ty_name) => ty_name,
    };
    let Some(ty_def) = schema.types.get(ty_name) else {
        Err(SuspectedValidationBug {
            message: format!("Undefined type {ty_name} for {kind} {parent}{sep}{name}"),
            location: ty_name.location(),
        })?
    };
    match ty_def {
        ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_) => {
            Err(SuspectedValidationBug {
                message: format!("Non-input type {ty_name} for {kind} {parent}{sep}{name}."),
                location: ty_name.location(),
            })?
        }
        ExtendedType::Scalar(_) => match ty_name.as_str() {
            "Int" => {
                // https://spec.graphql.org/October2021/#sec-Int.Input-Coercion
                if value
                    .as_i64()
                    .is_some_and(|value| i32::try_from(value).is_ok())
                {
                    return Ok(value.clone());
                }
            }
            "Float" => {
                // https://spec.graphql.org/October2021/#sec-Float.Input-Coercion
                if value.is_f64() {
                    return Ok(value.clone());
                }
            }
            "String" => {
                // https://spec.graphql.org/October2021/#sec-String.Input-Coercion
                if value.is_string() {
                    return Ok(value.clone());
                }
            }
            "Boolean" => {
                // https://spec.graphql.org/October2021/#sec-Boolean.Input-Coercion
                if value.is_boolean() {
                    return Ok(value.clone());
                }
            }
            "ID" => {
                // https://spec.graphql.org/October2021/#sec-ID.Input-Coercion
                if value.is_string() || value.is_i64() {
                    return Ok(value.clone());
                }
            }
            _ => {
                // Custom scalar
                // TODO: have a hook for coercion of custom scalars?
                return Ok(value.clone());
            }
        },
        ExtendedType::Enum(ty_def) => {
            // https://spec.graphql.org/October2021/#sec-Enums.Input-Coercion
            if let Some(str) = value.as_str() {
                if ty_def.values.keys().any(|value_name| value_name == str) {
                    return Ok(value.clone());
                }
            }
        }
        ExtendedType::InputObject(ty_def) => {
            // https://spec.graphql.org/October2021/#sec-Input-Objects.Input-Coercion
            if let Some(object) = value.as_object() {
                if let Some(key) = object
                    .keys()
                    .find(|key| !ty_def.fields.contains_key(key.as_str()))
                {
                    return Err(InputCoercionError::ValueError {
                        message: format!(
                            "Input object has key {} not in type {ty_name}",
                            key.as_str()
                        ),
                        location: None,
                    });
                }
                let mut object = object.clone();
                for (field_name, field_def) in &ty_def.fields {
                    if let Some(field_value) = object.get_mut(field_name.as_str()) {
                        *field_value = coerce_variable_value(
                            schema,
                            "input field",
                            ty_name,
                            ".",
                            field_name,
                            &field_def.ty,
                            field_value,
                        )?
                    } else if let Some(default) = &field_def.default_value {
                        let default = graphql_value_to_json(
                            "input field",
                            ty_name,
                            ".",
                            field_name,
                            default,
                        )?;
                        object.insert(field_name.as_str(), default);
                    } else if field_def.ty.is_non_null() {
                        return Err(InputCoercionError::ValueError {
                            message: format!("Missing value for non-null input object field {ty_name}.{field_name}"),
                            location: None,
                        });
                    } else {
                        // Field not required
                    }
                }
                return Ok(object.into());
            }
        }
    }
    Err(InputCoercionError::ValueError {
        message: format!("Could not coerce {kind} {parent}{sep}{name}: {value} to type {ty_name}"),
        location: None,
    })
}

fn graphql_value_to_json(
    kind: &str,
    parent: &str,
    sep: &str,
    name: &str,
    value: &Node<Value>,
) -> Result<JsonValue, InputCoercionError> {
    match value.as_ref() {
        Value::Null => Ok(JsonValue::Null),
        Value::Variable(_) => {
            // TODO: separate `ContValue` enum without this variant?
            Err(InputCoercionError::SuspectedValidationBug(
                SuspectedValidationBug {
                    message: format!("Variable in default value of {kind} {parent}{sep}{name}."),
                    location: value.location(),
                },
            ))
        }
        Value::Enum(value) => Ok(value.as_str().into()),
        Value::String(value) => Ok(value.as_str().into()),
        Value::Boolean(value) => Ok((*value).into()),
        // Rely on `serde_json::Number`’s own parser to use whatever precision it supports
        Value::Int(i) => Ok(JsonValue::Number(i.as_str().parse().map_err(|_| {
            InputCoercionError::ValueError {
                message: format!("IntValue overflow in {kind} {parent}{sep}{name}"),
                location: value.location(),
            }
        })?)),
        Value::Float(f) => Ok(JsonValue::Number(f.as_str().parse().map_err(|_| {
            InputCoercionError::ValueError {
                message: format!("FloatValue overflow in {kind} {parent}{sep}{name}"),
                location: value.location(),
            }
        })?)),
        Value::List(value) => value
            .iter()
            .map(|value| graphql_value_to_json(kind, parent, sep, name, value))
            .collect(),
        Value::Object(value) => value
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.as_str(),
                    graphql_value_to_json(kind, parent, sep, name, value)?,
                ))
            })
            .collect(),
    }
}

/// <https://spec.graphql.org/October2021/#sec-Coercing-Field-Arguments>
pub(crate) fn coerce_argument_values(
    schema: &Schema,
    document: &Valid<ExecutableDocument>,
    variable_values: &Valid<JsonMap>,
    errors: &mut Vec<GraphQLError>,
    path: LinkedPath<'_>,
    field_def: &FieldDefinition,
    field: &Field,
) -> Result<JsonMap, PropagateNull> {
    let mut coerced_values = JsonMap::new();
    for arg_def in &field_def.arguments {
        let arg_name = &arg_def.name;
        if let Some(arg) = field.arguments.iter().find(|arg| arg.name == *arg_name) {
            if let Value::Variable(var_name) = arg.value.as_ref() {
                if let Some(var_value) = variable_values.get(var_name.as_str()) {
                    if var_value.is_null() && arg_def.ty.is_non_null() {
                        errors.push(GraphQLError::field_error(
                            format!("null value for non-nullable argument {arg_name}"),
                            path,
                            arg_def.location(),
                            &document.sources,
                        ));
                        return Err(PropagateNull);
                    } else {
                        coerced_values.insert(arg_name.as_str(), var_value.clone());
                        continue;
                    }
                }
            } else if arg.value.is_null() && arg_def.ty.is_non_null() {
                errors.push(GraphQLError::field_error(
                    format!("null value for non-nullable argument {arg_name}"),
                    path,
                    arg_def.location(),
                    &document.sources,
                ));
                return Err(PropagateNull);
            } else {
                let coerced_value = coerce_argument_value(
                    schema,
                    document,
                    variable_values,
                    errors,
                    path,
                    "argument",
                    "",
                    "",
                    arg_name,
                    &arg_def.ty,
                    &arg.value,
                )?;
                coerced_values.insert(arg_name.as_str(), coerced_value);
                continue;
            }
        }
        if let Some(default) = &arg_def.default_value {
            let value =
                graphql_value_to_json("argument", "", "", arg_name, default).map_err(|err| {
                    errors.push(err.into_field_error(path, &document.sources));
                    PropagateNull
                })?;
            coerced_values.insert(arg_def.name.as_str(), value);
            continue;
        }
        if arg_def.ty.is_non_null() {
            errors.push(GraphQLError::field_error(
                format!("missing value for required argument {arg_name}"),
                path,
                arg_def.location(),
                &document.sources,
            ));
            return Err(PropagateNull);
        }
    }
    Ok(coerced_values)
}

#[allow(clippy::too_many_arguments)] // yes it’s not a nice API but it’s internal
fn coerce_argument_value(
    schema: &Schema,
    document: &Valid<ExecutableDocument>,
    variable_values: &Valid<JsonMap>,
    errors: &mut Vec<GraphQLError>,
    path: LinkedPath<'_>,
    kind: &str,
    parent: &str,
    sep: &str,
    name: &str,
    ty: &Type,
    value: &Node<Value>,
) -> Result<JsonValue, PropagateNull> {
    if value.is_null() {
        if ty.is_non_null() {
            errors.push(GraphQLError::field_error(
                format!("null value for non-null {kind} {parent}{sep}{name}"),
                path,
                value.location(),
                &document.sources,
            ));
            return Err(PropagateNull);
        } else {
            return Ok(JsonValue::Null);
        }
    }
    if let Some(var_name) = value.as_variable() {
        if let Some(var_value) = variable_values.get(var_name.as_str()) {
            if var_value.is_null() && ty.is_non_null() {
                errors.push(GraphQLError::field_error(
                    format!("null variable value for non-null {kind} {parent}{sep}{name}"),
                    path,
                    value.location(),
                    &document.sources,
                ));
                return Err(PropagateNull);
            } else {
                return Ok(var_value.clone());
            }
        } else if ty.is_non_null() {
            errors.push(GraphQLError::field_error(
                format!("missing variable for non-null {kind} {parent}{sep}{name}"),
                path,
                value.location(),
                &document.sources,
            ));
            return Err(PropagateNull);
        } else {
            return Ok(JsonValue::Null);
        }
    }
    let ty_name = match ty {
        Type::List(inner_ty) | Type::NonNullList(inner_ty) => {
            // https://spec.graphql.org/October2021/#sec-List.Input-Coercion
            return value
                .as_list()
                // If not an array, treat the value as an array of size one:
                .unwrap_or(std::slice::from_ref(value))
                .iter()
                .map(|item| {
                    coerce_argument_value(
                        schema,
                        document,
                        variable_values,
                        errors,
                        path,
                        kind,
                        parent,
                        sep,
                        name,
                        inner_ty,
                        item,
                    )
                })
                .collect();
        }
        Type::Named(ty_name) | Type::NonNullNamed(ty_name) => ty_name,
    };
    let Some(ty_def) = schema.types.get(ty_name) else {
        errors.push(
            SuspectedValidationBug {
                message: format!("Undefined type {ty_name} for {kind} {parent}{sep}{name}"),
                location: value.location(),
            }
            .into_field_error(&document.sources, path),
        );
        return Err(PropagateNull);
    };
    match ty_def {
        ExtendedType::InputObject(ty_def) => {
            // https://spec.graphql.org/October2021/#sec-Input-Objects.Input-Coercion
            if let Some(object) = value.as_object() {
                if let Some((key, _value)) = object
                    .iter()
                    .find(|(key, _value)| !ty_def.fields.contains_key(key))
                {
                    errors.push(GraphQLError::field_error(
                        format!("Input object has key {key} not in type {ty_name}",),
                        path,
                        value.location(),
                        &document.sources,
                    ));
                    return Err(PropagateNull);
                }
                #[allow(clippy::map_identity)] // `map` converts `&(k, v)` to `(&k, &v)`
                let object: HashMap<_, _> = object.iter().map(|(k, v)| (k, v)).collect();
                let mut coerced_object = JsonMap::new();
                for (field_name, field_def) in &ty_def.fields {
                    if let Some(field_value) = object.get(field_name) {
                        let coerced_value = coerce_argument_value(
                            schema,
                            document,
                            variable_values,
                            errors,
                            path,
                            "input field",
                            ty_name,
                            ".",
                            field_name,
                            &field_def.ty,
                            field_value,
                        )?;
                        coerced_object.insert(field_name.as_str(), coerced_value);
                    } else if let Some(default) = &field_def.default_value {
                        let default =
                            graphql_value_to_json("input field", ty_name, ".", field_name, default)
                                .map_err(|err| {
                                    errors.push(err.into_field_error(path, &document.sources));
                                    PropagateNull
                                })?;
                        coerced_object.insert(field_name.as_str(), default);
                    } else if field_def.ty.is_non_null() {
                        errors.push(GraphQLError::field_error(
                            format!(
                                "Missing value for non-null input object field {ty_name}.{field_name}"
                            ),
                            path,
                            value.location(),
                            &document.sources,
                        ));
                        return Err(PropagateNull);
                    } else {
                        // Field not required
                    }
                }
                return Ok(coerced_object.into());
            }
        }
        _ => {
            // For scalar and enums, rely and validation and just convert between Rust types
            return graphql_value_to_json(kind, parent, sep, name, value).map_err(|err| {
                errors.push(err.into_field_error(path, &document.sources));
                PropagateNull
            });
        }
    }
    errors.push(GraphQLError::field_error(
        format!("Could not coerce {kind} {parent}{sep}{name}: {value} to type {ty_name}"),
        path,
        value.location(),
        &document.sources,
    ));
    Err(PropagateNull)
}

impl From<SuspectedValidationBug> for InputCoercionError {
    fn from(value: SuspectedValidationBug) -> Self {
        Self::SuspectedValidationBug(value)
    }
}

impl InputCoercionError {
    pub(crate) fn into_field_error(
        self,
        path: LinkedPath<'_>,
        sources: &SourceMap,
    ) -> GraphQLError {
        match self {
            Self::SuspectedValidationBug(s) => s.into_field_error(sources, path),
            Self::ValueError { message, location } => {
                GraphQLError::field_error(message, path, location, sources)
            }
        }
    }
}
