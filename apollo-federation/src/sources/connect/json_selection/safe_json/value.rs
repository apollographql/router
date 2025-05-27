use std::fmt;

use super::{Map, SafeString};

pub(crate) mod ser;

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

    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Value::Number(ref n) => n.as_i64(),
            _ => None,
        }
    }
}

impl AsRef<Value> for Value {
    fn as_ref(&self) -> &Value {
        self
    }
}

impl PartialEq<serde_json_bytes::Value> for Value {
    fn eq(&self, other: &serde_json_bytes::Value) -> bool {
        match (self, other) {
            (Value::Null, serde_json_bytes::Value::Null) => true,
            (Value::Bool(b1), serde_json_bytes::Value::Bool(b2)) => b1 == b2,
            (Value::Number(number1), serde_json_bytes::Value::Number(number2)) => {
                number1 == number2
            }
            (Value::String(safe_string), serde_json_bytes::Value::String(byte_string)) => {
                safe_string == byte_string
            }
            (Value::Array(values1), serde_json_bytes::Value::Array(values2)) => {
                values1.len() == values2.len()
                    && values1.iter().zip(values2).all(|(v1, v2)| v1 == v2)
            }
            (Value::Object(map1), serde_json_bytes::Value::Object(map2)) => {
                map1.len() == map2.len()
                    && map1
                        .iter()
                        .zip(map2)
                        .all(|((k1, v1), (k2, v2))| k1 == k2 && v1 == v2)
            }
            _ => false,
        }
    }
}

impl fmt::Display for Value {
    /// Display a JSON value as a string.
    ///
    /// ```
    /// # use serde_json::json;
    /// #
    /// let json = json!({ "city": "London", "street": "10 Downing Street" });
    ///
    /// // Compact format:
    /// //
    /// // {"city":"London","street":"10 Downing Street"}
    /// let compact = format!("{}", json);
    /// assert_eq!(compact,
    ///     "{\"city\":\"London\",\"street\":\"10 Downing Street\"}");
    ///
    /// // Pretty format:
    /// //
    /// // {
    /// //   "city": "London",
    /// //   "street": "10 Downing Street"
    /// // }
    /// let pretty = format!("{:#}", json);
    /// assert_eq!(pretty,
    ///     "{\n  \"city\": \"London\",\n  \"street\": \"10 Downing Street\"\n}");
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        struct WriterFormatter<'a, 'b: 'a> {
            inner: &'a mut fmt::Formatter<'b>,
        }

        impl std::io::Write for WriterFormatter<'_, '_> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                // Safety: the serializer below only emits valid utf8 when using
                // the default formatter.
                let s = unsafe { std::str::from_utf8_unchecked(buf) };
                self.inner.write_str(s).map_err(io_error)?;
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        fn io_error(_: fmt::Error) -> std::io::Error {
            // Error value does not matter because Display impl just maps it
            // back to fmt::Error.
            std::io::Error::new(std::io::ErrorKind::Other, "fmt error")
        }

        let alternate = f.alternate();
        let mut wr = WriterFormatter { inner: f };
        if alternate {
            // {:#}
            serde_json::to_writer_pretty(&mut wr, self).map_err(|_| fmt::Error)
        } else {
            // {}
            serde_json::to_writer(&mut wr, self).map_err(|_| fmt::Error)
        }
    }
}

impl From<serde_json_bytes::Value> for Value {
    fn from(value: serde_json_bytes::Value) -> Self {
        match value {
            serde_json_bytes::Value::Null => Value::Null,
            serde_json_bytes::Value::Bool(b) => Value::Bool(b),
            serde_json_bytes::Value::Number(n) => Value::Number(n),
            serde_json_bytes::Value::String(s) => Value::String(SafeString::AutoEncoded(s)),
            serde_json_bytes::Value::Array(arr) => {
                Value::Array(arr.into_iter().map(Value::from).collect())
            }
            serde_json_bytes::Value::Object(obj) => Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (SafeString::AutoEncoded(k), Value::from(v)))
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
            serde_json_bytes::Value::String(s) => {
                Value::String(SafeString::AutoEncoded(s.to_owned()))
            }
            serde_json_bytes::Value::Array(arr) => {
                Value::Array(arr.iter().map(Value::from).collect())
            }
            serde_json_bytes::Value::Object(obj) => Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (SafeString::AutoEncoded(k.to_owned()), Value::from(v)))
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
            Value::Array(arr) => serde_json_bytes::Value::Array(
                arr.into_iter().map(serde_json_bytes::Value::from).collect(),
            ),
            Value::Object(obj) => serde_json_bytes::Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (k.escape(), serde_json_bytes::Value::from(v)))
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
impl From<&Value> for serde_json_bytes::Value {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => serde_json_bytes::Value::Null,
            Value::Bool(b) => serde_json_bytes::Value::Bool(*b),
            Value::Number(n) => serde_json_bytes::Value::Number(n.clone()),
            Value::String(s) => serde_json_bytes::Value::String(s.clone().escape()),
            Value::Array(arr) => serde_json_bytes::Value::Array(
                arr.iter().map(serde_json_bytes::Value::from).collect(),
            ),
            Value::Object(obj) => serde_json_bytes::Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (k.clone().escape(), serde_json_bytes::Value::from(v)))
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

#[expect(dead_code, private_bounds)]
pub(crate) trait Index: private::Sealed {
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
