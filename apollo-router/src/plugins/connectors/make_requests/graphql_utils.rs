use apollo_compiler::executable::Field;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::schema::Value;
use apollo_compiler::Node;
use serde_json::Number;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value as JSONValue;
use tower::BoxError;

use super::MakeRequestError;
use super::ENTITIES;

pub(super) fn field_arguments_map(
    field: &Node<Field>,
    variables: &Map<ByteString, JSONValue>,
) -> Result<Map<ByteString, JSONValue>, BoxError> {
    let mut arguments = Map::new();

    for argument in field.arguments.iter() {
        match &*argument.value {
            apollo_compiler::schema::Value::Variable(name) => {
                if let Some(value) = variables.get(name.as_str()) {
                    arguments.insert(argument.name.as_str(), value.clone());
                }
            }
            _ => {
                arguments.insert(
                    argument.name.as_str(),
                    argument_value_to_json(&argument.value)?,
                );
            }
        }
    }

    for argument_def in field.definition.arguments.iter() {
        if let Some(value) = argument_def.default_value.as_ref() {
            arguments
                .entry(argument_def.name.as_str())
                .or_insert_with(|| {
                    argument_value_to_json(value).unwrap_or_else(|err| {
                        tracing::warn!("failed to convert default value to json: {}", err);
                        JSONValue::Null
                    })
                });
        }
    }

    Ok(arguments)
}

pub(super) fn argument_value_to_json(
    value: &apollo_compiler::ast::Value,
) -> Result<JSONValue, BoxError> {
    match value {
        Value::Null => Ok(JSONValue::Null),
        Value::Enum(e) => Ok(JSONValue::String(e.as_str().into())),
        Value::Variable(_) => Err(BoxError::from("variables not supported")),
        Value::String(s) => Ok(JSONValue::String(s.as_str().into())),
        Value::Float(f) => Ok(JSONValue::Number(
            Number::from_f64(
                f.try_to_f64()
                    .map_err(|_| BoxError::from("try_to_f64 failed"))?,
            )
            .ok_or_else(|| BoxError::from("Number::from_f64 failed"))?,
        )),
        Value::Int(i) => Ok(JSONValue::Number(Number::from(
            i.try_to_i32().map_err(|_| "invalid int")?,
        ))),
        Value::Boolean(b) => Ok(JSONValue::Bool(*b)),
        Value::List(l) => Ok(JSONValue::Array(
            l.iter()
                .map(|v| argument_value_to_json(v))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Value::Object(o) => Ok(JSONValue::Object(
            o.iter()
                .map(|(k, v)| argument_value_to_json(v).map(|v| (k.as_str().into(), v)))
                .collect::<Result<Map<_, _>, _>>()?,
        )),
    }
}

pub(super) fn get_entity_fields(
    op: &Node<Operation>,
) -> Result<(&Node<Field>, bool), MakeRequestError> {
    use MakeRequestError::*;

    let root_field = op
        .selection_set
        .selections
        .iter()
        .find_map(|s| match s {
            Selection::Field(f) if f.name == ENTITIES => Some(f),
            _ => None,
        })
        .ok_or_else(|| InvalidOperation("missing entities root field".into()))?;

    let mut typename_requested = false;

    for selection in root_field.selection_set.selections.iter() {
        match selection {
            Selection::Field(f) => {
                if f.name == "__typename" {
                    typename_requested = true;
                }
            }
            Selection::FragmentSpread(_) => {
                return Err(UnsupportedOperation("fragment spread not supported".into()))
            }
            Selection::InlineFragment(f) => {
                for selection in f.selection_set.selections.iter() {
                    match selection {
                        Selection::Field(f) => {
                            if f.name == "__typename" {
                                typename_requested = true;
                            }
                        }
                        Selection::FragmentSpread(_) | Selection::InlineFragment(_) => {
                            return Err(UnsupportedOperation(
                                "fragment spread not supported".into(),
                            ))
                        }
                    }
                }
            }
        }
    }

    Ok((root_field, typename_requested))
}
