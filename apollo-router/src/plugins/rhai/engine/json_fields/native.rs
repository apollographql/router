//! Direct conversion between Router's JSON values and native Rhai values.
//!
//! This produces exactly what Rhai's serde support produces for the same values, without
//! driving a serializer or deserializer:
//!
//! | JSON | Rhai |
//! | --- | --- |
//! | `null` | `()` |
//! | boolean | `bool` |
//! | integer within `i64` | `i64` |
//! | integer above `i64::MAX`, or float | `f64` |
//! | string | `string` |
//! | array | `array` |
//! | object | `map`, whose keys are sorted |
//!
//! In the other direction `char` also becomes a string and a non-finite float becomes `null`.
//! Any other Rhai value, including a shared (captured) one, goes through Rhai's serde support,
//! which either converts it or reports the same error released Routers report.

use rhai::Array;
use rhai::Dynamic;
use rhai::EvalAltResult;
use rhai::ImmutableString;
use rhai::Map;
use rhai::serde::from_dynamic;
use serde_json::Number;

use crate::json_ext::Object;
use crate::json_ext::Value;

pub(super) fn value_to_rhai(value: &Value) -> Dynamic {
    match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(value) => Dynamic::from_bool(*value),
        Value::Number(number) => number_to_rhai(number),
        Value::String(string) => ImmutableString::from(string.as_str()).into(),
        Value::Array(array) => Dynamic::from_array(array.iter().map(value_to_rhai).collect()),
        Value::Object(object) => Dynamic::from_map(object_to_map(object)),
    }
}

pub(super) fn object_to_map(object: &Object) -> Map {
    // Inserting avoids the temporary sorting buffer `collect` allocates for a `BTreeMap`.
    let mut map = Map::new();
    for (key, value) in object {
        map.insert(key.as_str().into(), value_to_rhai(value));
    }
    map
}

fn number_to_rhai(number: &Number) -> Dynamic {
    match (number.as_i64(), number.as_f64()) {
        (Some(integer), _) => Dynamic::from_int(integer),
        // Floats, and unsigned integers above `i64::MAX`, which do not fit Rhai's integer type.
        (None, Some(float)) => Dynamic::from_float(float),
        // `as_f64` fails only with serde_json's `arbitrary_precision` feature, which Router does
        // not enable.
        (None, None) => unreachable!("JSON number is neither i64 nor f64: {number}"),
    }
}

pub(super) fn value_from_rhai(value: &Dynamic) -> Result<Value, Box<EvalAltResult>> {
    // Checked first: the accessors below would read through a shared value, which Rhai's
    // serde support rejects.
    if value.is_shared() {
        return from_dynamic(value);
    }
    if let Ok(map) = value.as_map_ref() {
        return map_to_object(&map).map(Value::Object);
    }
    if let Ok(array) = value.as_array_ref() {
        return array_to_json(&array).map(Value::Array);
    }
    if let Ok(string) = value.as_immutable_string_ref() {
        return Ok(Value::String(string.as_str().into()));
    }
    if let Ok(integer) = value.as_int() {
        return Ok(Value::Number(integer.into()));
    }
    if let Ok(float) = value.as_float() {
        return Ok(Number::from_f64(float).map_or(Value::Null, Value::Number));
    }
    if let Ok(boolean) = value.as_bool() {
        return Ok(Value::Bool(boolean));
    }
    if value.is_unit() {
        return Ok(Value::Null);
    }
    if let Ok(character) = value.as_char() {
        return Ok(Value::String(character.to_string().into()));
    }
    from_dynamic(value)
}

/// Convert the value a setter for a JSON object received. Rhai only calls those setters with a
/// map, which is never shared.
pub(super) fn object_from_rhai(value: &Dynamic) -> Result<Object, Box<EvalAltResult>> {
    if !value.is_shared()
        && let Ok(map) = value.as_map_ref()
    {
        return map_to_object(&map);
    }
    from_dynamic(value)
}

fn map_to_object(map: &Map) -> Result<Object, Box<EvalAltResult>> {
    map.iter()
        .map(|(key, value)| Ok((key.as_str().into(), value_from_rhai(value)?)))
        .collect()
}

fn array_to_json(array: &Array) -> Result<Vec<Value>, Box<EvalAltResult>> {
    array.iter().map(value_from_rhai).collect()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use rhai::Dynamic;
    use rhai::FnPtr;
    use rhai::Map;
    use rhai::serde::from_dynamic;
    use rhai::serde::to_dynamic;
    use serde_json_bytes::json;

    use super::*;

    /// Values whose conversion to Rhai could differ from Rhai's serde support.
    fn json_values() -> Vec<Value> {
        vec![
            json!(null),
            json!(true),
            json!(0),
            json!(-1),
            json!(i64::MIN),
            json!(i64::MAX),
            json!(i64::MAX as u64 + 1),
            json!(u64::MAX),
            json!(1.0),
            json!(-0.0),
            json!(1e300),
            json!(""),
            json!("é ☃ \u{0}"),
            json!([]),
            json!({}),
            json!([1, [2, [3]], {"a": []}]),
            json!({"z": 1, "a": {"é": 2, "": 3, "B": 4}, "m": [null]}),
            (0..200).fold(json!("deep"), |inner, _| json!({ "x": [inner] })),
        ]
    }

    #[test]
    fn values_convert_to_rhai_like_serde() {
        for value in json_values() {
            let expected = to_dynamic(&value).unwrap();
            let actual = value_to_rhai(&value);
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"), "{value}");
            assert_eq!(actual.type_name(), expected.type_name(), "{value}");
        }
    }

    #[test]
    fn json_values_round_trip_like_serde() {
        for value in json_values() {
            let rhai = value_to_rhai(&value);
            let expected = from_dynamic::<Value>(&rhai).unwrap();
            assert_eq!(value_from_rhai(&rhai).unwrap(), expected, "{value}");
        }
    }

    #[derive(Clone)]
    struct Custom;

    /// Rhai values a script can store in a JSON field, including ones JSON cannot hold.
    fn rhai_values() -> Vec<(&'static str, Dynamic)> {
        vec![
            ("unit", Dynamic::UNIT),
            ("char", Dynamic::from_char('c')),
            ("nan", Dynamic::from_float(f64::NAN)),
            ("infinity", Dynamic::from_float(f64::INFINITY)),
            ("float", Dynamic::from_float(0.1)),
            ("int", Dynamic::from_int(-3)),
            ("i32", Dynamic::from(7_i32)),
            ("u64", Dynamic::from(u64::MAX)),
            ("blob", Dynamic::from_blob(vec![1, 2])),
            ("function", Dynamic::from(FnPtr::new("f").unwrap())),
            ("timestamp", Dynamic::from_timestamp(Instant::now())),
            ("custom", Dynamic::from(Custom)),
            ("shared", Dynamic::from_int(1).into_shared()),
            (
                "shared_map",
                Dynamic::from_map(Map::from([("a".into(), Dynamic::from_int(1))])).into_shared(),
            ),
            (
                "nested",
                Dynamic::from_array(vec![
                    Dynamic::from_char('x'),
                    Dynamic::from(FnPtr::new("g").unwrap()),
                ]),
            ),
        ]
    }

    fn outcome<T>(result: Result<T, Box<EvalAltResult>>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    #[test]
    fn rhai_values_convert_to_json_like_serde() {
        for (name, value) in rhai_values() {
            let map = Dynamic::from_map(Map::from([("value".into(), value.clone())]));
            assert_eq!(
                outcome(object_from_rhai(&map)),
                outcome(from_dynamic::<Object>(&map)),
                "{name} in an object"
            );
            assert_eq!(
                outcome(value_from_rhai(&value)),
                outcome(from_dynamic::<Value>(&value)),
                "{name}"
            );
        }
    }

    #[test]
    fn setters_receiving_other_types_fail_like_serde() {
        for (name, value) in rhai_values() {
            assert_eq!(
                outcome(object_from_rhai(&value)),
                outcome(from_dynamic::<Object>(&value)),
                "{name}"
            );
        }
    }
}
