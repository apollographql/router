use std::cmp::Ordering;

use apollo_compiler::ast;
use serde_json_bytes::Value;

use crate::error::QueryPlannerError;
use crate::graphql;

pub(crate) fn compare_introspection_responses(
    query: &str,
    mut js_result: Result<graphql::Response, QueryPlannerError>,
    mut rust_result: Result<graphql::Response, QueryPlannerError>,
) {
    let is_matched;
    match (&mut js_result, &mut rust_result) {
        (Err(_), Err(_)) => {
            is_matched = true;
        }
        (Err(err), Ok(_)) => {
            is_matched = false;
            tracing::warn!("JS introspection error: {err}")
        }
        (Ok(_), Err(err)) => {
            is_matched = false;
            tracing::warn!("Rust introspection error: {err}")
        }
        (Ok(js_response), Ok(rust_response)) => {
            if let (Some(js_data), Some(rust_data)) =
                (&mut js_response.data, &mut rust_response.data)
            {
                normalize_response(js_data);
                normalize_response(rust_data);
            }
            is_matched = js_response.data == rust_response.data;
            if is_matched {
                tracing::trace!("Introspection match! 🎉")
            } else {
                tracing::debug!("Introspection mismatch");
                tracing::trace!("Introspection query:\n{query}");
                tracing::debug!("Introspection diff:\n{}", {
                    let rust = rust_response
                        .data
                        .as_ref()
                        .map(|d| serde_json::to_string_pretty(&d).unwrap())
                        .unwrap_or_default();
                    let js = js_response
                        .data
                        .as_ref()
                        .map(|d| serde_json::to_string_pretty(&d).unwrap())
                        .unwrap_or_default();
                    let diff = similar::TextDiff::from_lines(&js, &rust);
                    diff.unified_diff()
                        .context_radius(10)
                        .header("JS", "Rust")
                        .to_string()
                })
            }
        }
    }

    u64_counter!(
        "apollo.router.operations.introspection.both",
        "Comparing JS v.s. Rust introspection",
        1,
        "generation.is_matched" = is_matched,
        "generation.js_error" = js_result.is_err(),
        "generation.rust_error" = rust_result.is_err()
    );
}

fn normalize_response(value: &mut Value) {
    match value {
        Value::Array(array) => {
            for item in array.iter_mut() {
                normalize_response(item)
            }
            array.sort_by(json_compare)
        }
        Value::Object(object) => {
            for (key, value) in object {
                if let Some(new_value) = normalize_default_value(key.as_str(), value) {
                    *value = new_value
                } else {
                    normalize_response(value)
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// When a default value is an input object, graphql-js seems to sort its fields by name
fn normalize_default_value(key: &str, value: &Value) -> Option<Value> {
    if key != "defaultValue" {
        return None;
    }
    let default_value = value.as_str()?;
    // We don’t have a parser entry point for a standalone GraphQL `Value`,
    // so mint a document that contains that value.
    let doc = format!("{{ field(arg: {default_value}) }}");
    let doc = ast::Document::parse(doc, "").ok()?;
    let parsed_default_value = &doc
        .definitions
        .first()?
        .as_operation_definition()?
        .selection_set
        .first()?
        .as_field()?
        .arguments
        .first()?
        .value;
    match parsed_default_value.as_ref() {
        ast::Value::List(_) | ast::Value::Object(_) => {
            let normalized = normalize_parsed_default_value(parsed_default_value);
            Some(normalized.serialize().no_indent().to_string().into())
        }
        ast::Value::Null
        | ast::Value::Enum(_)
        | ast::Value::Variable(_)
        | ast::Value::String(_)
        | ast::Value::Float(_)
        | ast::Value::Int(_)
        | ast::Value::Boolean(_) => None,
    }
}

fn normalize_parsed_default_value(value: &ast::Value) -> ast::Value {
    match value {
        ast::Value::List(items) => ast::Value::List(
            items
                .iter()
                .map(|item| normalize_parsed_default_value(item).into())
                .collect(),
        ),
        ast::Value::Object(fields) => {
            let mut new_fields: Vec<_> = fields
                .iter()
                .map(|(name, value)| (name.clone(), normalize_parsed_default_value(value).into()))
                .collect();
            new_fields.sort_by(|(name_1, _value_1), (name_2, _value_2)| name_1.cmp(name_2));
            ast::Value::Object(new_fields)
        }
        v => v.clone(),
    }
}

fn json_compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a.as_f64().unwrap().total_cmp(&b.as_f64().unwrap()),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Array(a), Value::Array(b)) => iter_cmp(a, b, json_compare),
        (Value::Object(a), Value::Object(b)) => {
            iter_cmp(a, b, |(key_a, a), (key_b, b)| {
                debug_assert_eq!(key_a, key_b); // Response object keys are in selection set order
                json_compare(a, b)
            })
        }
        _ => json_discriminant(a).cmp(&json_discriminant(b)),
    }
}

// TODO: use `Iterator::cmp_by` when available:
// https://doc.rust-lang.org/std/iter/trait.Iterator.html#method.cmp_by
// https://github.com/rust-lang/rust/issues/64295
fn iter_cmp<T>(
    a: impl IntoIterator<Item = T>,
    b: impl IntoIterator<Item = T>,
    cmp: impl Fn(T, T) -> Ordering,
) -> Ordering {
    use itertools::Itertools;
    for either_or_both in a.into_iter().zip_longest(b) {
        match either_or_both {
            itertools::EitherOrBoth::Both(a, b) => {
                let ordering = cmp(a, b);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            itertools::EitherOrBoth::Left(_) => return Ordering::Less,
            itertools::EitherOrBoth::Right(_) => return Ordering::Greater,
        }
    }
    Ordering::Equal
}

fn json_discriminant(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}
