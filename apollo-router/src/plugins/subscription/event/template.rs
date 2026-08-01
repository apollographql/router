use std::collections::HashMap;

use apollo_compiler::executable;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use super::EventError;

pub(super) fn render_destinations(
    templates: &[String],
    field: &apollo_compiler::Node<executable::Field>,
    variables: &Map<ByteString, Value>,
) -> Result<Vec<String>, EventError> {
    let mut arguments = HashMap::new();
    for argument in &field.arguments {
        arguments.insert(
            argument.name.to_string(),
            argument_value_to_json(&argument.value, variables)?,
        );
    }
    for definition in &field.definition.arguments {
        if !arguments.contains_key(definition.name.as_str())
            && let Some(default) = &definition.default_value
        {
            arguments.insert(
                definition.name.to_string(),
                argument_value_to_json(default, variables)?,
            );
        }
    }

    templates
        .iter()
        .map(|template| render_destination(template, &arguments))
        .collect()
}

fn render_destination(
    template: &str,
    arguments: &HashMap<String, Value>,
) -> Result<String, EventError> {
    let mut rendered = String::with_capacity(template.len());
    let mut remainder = template;
    while let Some(start) = remainder.find("{{") {
        rendered.push_str(&remainder[..start]);
        let after_open = &remainder[start + 2..];
        let Some(end) = after_open.find("}}") else {
            return Err(EventError::new(format!(
                "event destination template '{template}' has an unclosed expression"
            )));
        };
        let expression = after_open[..end].trim();
        let Some(path) = expression.strip_prefix("args.") else {
            return Err(EventError::new(format!(
                "event destination expression '{{{{ {expression} }}}}' must start with 'args.'"
            )));
        };
        let mut segments = path.split('.');
        let argument_name = segments.next().unwrap_or_default();
        let mut value = arguments.get(argument_name).ok_or_else(|| {
            EventError::new(format!(
                "event destination references unknown argument '{argument_name}'"
            ))
        })?;
        for segment in segments {
            value = value.get(segment).ok_or_else(|| {
                EventError::new(format!(
                    "event destination cannot resolve argument path '{path}'"
                ))
            })?;
        }
        match value {
            Value::String(value) => rendered.push_str(value.as_str()),
            Value::Number(value) => rendered.push_str(&value.to_string()),
            Value::Bool(value) => rendered.push_str(if *value { "true" } else { "false" }),
            Value::Null | Value::Array(_) | Value::Object(_) => {
                return Err(EventError::new(format!(
                    "event destination argument path '{path}' must resolve to a scalar"
                )));
            }
        }
        remainder = &after_open[end + 2..];
    }
    rendered.push_str(remainder);
    if rendered.is_empty() {
        return Err(EventError::new(
            "event destination must not render to an empty string",
        ));
    }
    Ok(rendered)
}

fn argument_value_to_json(
    value: &apollo_compiler::ast::Value,
    variables: &Map<ByteString, Value>,
) -> Result<Value, EventError> {
    use apollo_compiler::ast::Value as CompilerValue;

    match value {
        CompilerValue::Null => Ok(Value::Null),
        CompilerValue::Enum(value) => Ok(Value::String(value.as_str().into())),
        CompilerValue::Variable(name) => {
            Ok(variables.get(name.as_str()).cloned().unwrap_or(Value::Null))
        }
        CompilerValue::String(value) => Ok(Value::String(value.as_str().into())),
        CompilerValue::Float(value) => value
            .try_to_f64()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| EventError::new("event argument contains an invalid float")),
        CompilerValue::Int(value) => value
            .try_to_i32()
            .map(|value| Value::Number(value.into()))
            .map_err(|_| EventError::new("event argument contains an invalid integer")),
        CompilerValue::Boolean(value) => Ok(Value::Bool(*value)),
        CompilerValue::List(values) => values
            .iter()
            .map(|value| argument_value_to_json(value, variables))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        CompilerValue::Object(values) => values
            .iter()
            .map(|(name, value)| {
                argument_value_to_json(value, variables)
                    .map(|value| (ByteString::from(name.as_str()), value))
            })
            .collect::<Result<Map<_, _>, _>>()
            .map(Value::Object),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_destination_argument_templates() {
        let arguments = HashMap::from([
            ("id".to_string(), Value::Number(42.into())),
            (
                "tenant".to_string(),
                serde_json_bytes::json!({"slug": "acme"}),
            ),
        ]);
        assert_eq!(
            render_destination(
                "tenant.{{ args.tenant.slug }}.product.{{ args.id }}",
                &arguments
            )
            .unwrap(),
            "tenant.acme.product.42"
        );
    }

    #[test]
    fn rejects_non_scalar_destination_arguments() {
        let arguments = HashMap::from([("ids".to_string(), serde_json_bytes::json!([1, 2]))]);
        assert!(render_destination("products.{{ args.ids }}", &arguments).is_err());
    }
}
