use std::fmt;

use bytes::Bytes;
use serde::{
    de::{Error, Visitor},
    Deserialize, Deserializer,
};

#[derive(Clone, Eq, PartialEq)]
pub struct ByteString(Bytes);

/// read only string backed by a `Bytes` buffer
impl ByteString {
    /// will panic if `string` is not contained in `origin`
    pub fn new(origin: &Bytes, string: &str) -> Self {
        ByteString(origin.slice_ref(string.as_bytes()))
    }

    pub fn as_str(&self) -> &str {
        // `ByteString` can only be created from a valid `&str`
        unsafe { std::str::from_utf8_unchecked(&self.0) }
    }

    pub fn inner(&self) -> &Bytes {
        &self.0
    }
}

impl From<String> for ByteString {
    fn from(s: String) -> Self {
        ByteString(s.into())
    }
}

impl From<&str> for ByteString {
    fn from(s: &str) -> Self {
        ByteString(s.to_string().into())
    }
}

impl PartialEq<ByteString> for String {
    fn eq(&self, other: &ByteString) -> bool {
        self.as_bytes() == other.0
    }
}

struct ByteStringVisitor;

impl<'de> Visitor<'de> for ByteStringVisitor {
    type Value = ByteString;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(v.into())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(v.into())
    }
}

impl<'de> Deserialize<'de> for ByteString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(ByteStringVisitor)
    }
}
