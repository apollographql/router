use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::executable::Field;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::parser::SourceSpan;
use apollo_compiler::response::GraphQLError;
use apollo_compiler::response::JsonMap;
use apollo_compiler::response::JsonValue;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::validation::Valid;

use super::engine::LinkedPath;
use super::engine::PropagateNull;
use super::engine::field_error;
use super::validation::SuspectedValidationBug;

#[derive(Debug, Clone)]
pub(crate) enum InputCoercionError {
    SuspectedValidationBug(SuspectedValidationBug),
    // TODO: split into more structured variants?
    ValueError {
        message: String,
        location: Option<SourceSpan>,
    },
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
                        errors.push(field_error(
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
                errors.push(field_error(
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
            errors.push(field_error(
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
            errors.push(field_error(
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
                errors.push(field_error(
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
            errors.push(field_error(
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
                    errors.push(field_error(
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
                        errors.push(field_error(
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
    errors.push(field_error(
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
            Self::ValueError { message, location } => field_error(message, path, location, sources),
        }
    }
}
