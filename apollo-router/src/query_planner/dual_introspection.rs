use std::cmp::Ordering;

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
                json_sort_arrays(js_data);
                json_sort_arrays(rust_data);
            }
            is_matched = js_response.data == rust_response.data;
            if is_matched {
                tracing::trace!("Introspection query:\n{query}");
                tracing::debug!("Introspection match! 🎉")
            } else {
                tracing::debug!("Introspection mismatch");
                tracing::trace!("Introspection query:\n{query}");
                tracing::trace!("Introspection diff:\n{}", {
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

fn json_sort_arrays(value: &mut Value) {
    match value {
        Value::Array(array) => {
            for item in array.iter_mut() {
                json_sort_arrays(item)
            }
            array.sort_by(json_compare)
        }
        Value::Object(object) => {
            for (_key, value) in object {
                json_sort_arrays(value)
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
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
