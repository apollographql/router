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

impl AsRef<Value> for Value {
    fn as_ref(&self) -> &Value {
        &self
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
            serde_json_bytes::Value::Number(n) => Value::Number(n.to_owned()),
            serde_json_bytes::Value::String(s) => Value::String(SafeString::Unsafe(s.to_owned())),
            serde_json_bytes::Value::Array(arr) => {
                Value::Array(arr.iter().map(Value::from).collect())
            }
            serde_json_bytes::Value::Object(obj) => Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (SafeString::Unsafe(k.to_owned()), Value::from(v)))
                    .collect(),
            ),
        }
    }
}

impl From<Value> for serde_json_bytes::Value {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => serde_json_bytes::Value::Null,
            Value::Bool(b) => serde_json_bytes::Value::Bool(b),
            Value::Number(n) => serde_json_bytes::Value::Number(n),
            Value::String(s) => serde_json_bytes::Value::String(s.escape()),
            Value::Array(arr) => {
                serde_json_bytes::Value::Array(arr.into_iter().map(serde_json_bytes::Value::from).collect())
            }
            Value::Object(obj) => serde_json_bytes::Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (k.escape(), serde_json_bytes::Value::from(v)))
                    .collect(),
            ),
        }
    }
}

impl Index for usize {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        if let Value::Array(array) = v {
            array.get(*self)
        } else {
            None
        }
    }
}

impl Index for &str {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        if let Value::Object(obj) = v {
            let key: SafeString = (*self).into();
            obj.get(&key)
        } else {
            None
        }
    }
}

impl Index for String {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        if let Value::Object(obj) = v {
            let key: SafeString = (self.to_owned()).into();
            obj.get(&key)
        } else {
            None
        }
    }
}


pub trait Index: private::Sealed {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value>;
}

// Prevent users from implementing the Index trait.
mod private {
    use super::Value;

    pub(super) trait Sealed {}
    impl Sealed for usize {}
    impl Sealed for str {}
    impl Sealed for String {}
    impl Sealed for Value {}
    impl<T> Sealed for &T where T: ?Sized + Sealed {}
}