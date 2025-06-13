use serde_json_bytes::{ByteString, Map, Value};

#[derive(Debug, Default)]
pub struct RawError {
    pub message: String,
    pub code: Option<ByteString>,
    pub extensions: Map<ByteString, Value>,
    pub path: Option<ByteString>,
}

impl RawError {
    pub fn extension_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into().into());
        self
    }

    pub fn extension<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<ByteString>,
        V: Into<Value>,
    {
        self.extensions.insert(key.into(), value.into());
        self
    }
}
