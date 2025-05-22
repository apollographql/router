use std::fmt::Display;

use super::{Map, SafeString};

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(SafeString),
    Array(Vec<Value>),
    Object(Map<SafeString, Value>),
}

impl Value {
    pub(crate) fn render(self) -> serde_json_bytes::Value {
        match self {
            Value::Null => serde_json_bytes::Value::Null,
            Value::Bool(b) => serde_json_bytes::Value::Bool(b),
            Value::Number(n) => serde_json_bytes::Value::Number(n),
            Value::String(s) => serde_json_bytes::Value::String(s.escape()),
            Value::Array(arr) => {
                serde_json_bytes::Value::Array(arr.into_iter().map(Value::render).collect())
            }
            Value::Object(obj) => serde_json_bytes::Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (k.escape(), v.render()))
                    .collect(),
            ),
        }
    }

    pub(crate) fn get_key(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(obj) => obj.get(key),
            _ => None,
        }
    }
}

impl Display for SafeString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SafeString::Safe(s) => write!(f, "{}", s.as_str()),
            SafeString::Unsafe(s) => write!(f, "{}", s.as_str()),
        }
    }
}

impl From<serde_json_bytes::Value> for Value {
    fn from(value: serde_json_bytes::Value) -> Self {
        match value {
            serde_json_bytes::Value::Null => Value::Null,
            serde_json_bytes::Value::Bool(b) => Value::Bool(b),
            serde_json_bytes::Value::Number(n) => Value::Number(n),
            serde_json_bytes::Value::String(s) => Value::String(SafeString::Unsafe(s)),
            serde_json_bytes::Value::Array(arr) => {
                Value::Array(arr.into_iter().map(Value::from).collect())
            }
            serde_json_bytes::Value::Object(obj) => Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (SafeString::Unsafe(k), Value::from(v)))
                    .collect(),
            ),
        }
    }
}

impl From<&serde_json_bytes::Value> for Value {
    fn from(value: &serde_json_bytes::Value) -> Self {
        match value {
            serde_json_bytes::Value::Null => Value::Null,
            serde_json_bytes::Value::Bool(b) => Value::Bool(*b),
            serde_json_bytes::Value::Number(n) => Value::Number(n.clone()),
            serde_json_bytes::Value::String(s) => Value::String(SafeString::Unsafe(s.clone())),
            serde_json_bytes::Value::Array(arr) => {
                Value::Array(arr.iter().map(Value::from).collect())
            }
            serde_json_bytes::Value::Object(obj) => Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (SafeString::Unsafe(k.clone()), Value::from(v)))
                    .collect(),
            ),
        }
    }
}
