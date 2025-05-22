use std::{fmt, ops::Deref};

use percent_encoding::percent_encode;
use serde::{Deserialize, Serialize};
use serde_json_bytes::ByteString;

pub(crate) mod map;
pub(crate) mod string;
pub(crate) mod value;

use map::Map;
pub use string::SafeString;
use value::Value;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SafeBuffer(String);

impl SafeBuffer {
    pub fn new(s: ByteString) -> Self {
        Self(percent_encode(s.inner(), percent_encoding::NON_ALPHANUMERIC).to_string())
    }

    fn new_from_string(s: String) -> Self {
        Self(percent_encode(s.as_bytes(), percent_encoding::NON_ALPHANUMERIC).to_string())
    }

    pub fn new_url_safe(s: ByteString) -> Self {
        Self(s.as_str().to_string())
    }

    pub fn from_str(s: &str) -> Self {
        Self(percent_encode(s.as_bytes(), percent_encoding::NON_ALPHANUMERIC).to_string())
    }

    pub fn from_str_url_safe(s: &str) -> Self {
        Self(s.to_string())
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn push_str(&mut self, s: &str) {
        self.0.push_str(
            &percent_encode(s.as_bytes(), percent_encoding::NON_ALPHANUMERIC).to_string(),
        );
    }

    pub fn push_str_url_safe(&mut self, s: &str) {
        self.0.push_str(s);
    }

    // pub fn push(&mut self, ch: char) {
    //     self.0
    //         .push_str(&percent_encode(&[ch], &percent_encoding::NON_ALPHANUMERIC).to_string());
    // }

    pub fn push_url_safe(&mut self, ch: char) {
        self.0.push(ch);
    }
}

pub trait Join {
    fn join(self, separator: &SafeBuffer) -> SafeBuffer;
}

impl<I> Join for I
where
    I: IntoIterator<Item = SafeBuffer>,
{
    fn join(self, separator: &SafeBuffer) -> SafeBuffer {
        let mut iter = self.into_iter();
        let first = match iter.next() {
            Some(s) => s,
            None => return SafeBuffer::new_from_string(String::new()),
        };

        let mut result = first.0;
        for s in iter {
            result.push_str(separator.as_str());
            result.push_str(s.as_str());
        }
        SafeBuffer(result)
    }
}

pub trait SafeJoin {
    fn join(self, separator: &str) -> SafeBuffer;
}

impl<I> SafeJoin for I
where
    I: IntoIterator<Item = SafeBuffer>,
{
    fn join(self, separator: &str) -> SafeBuffer {
        let mut iter = self.into_iter();
        let first = match iter.next() {
            Some(s) => s,
            None => return SafeBuffer::new_from_string(String::new()),
        };

        let mut result = first.0;
        for s in iter {
            result.push_str(separator);
            result.push_str(
                percent_encode(s.as_bytes(), percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
                    .as_str(),
            );
        }
        SafeBuffer(result)
    }
}

impl Deref for SafeBuffer {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for SafeBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<ByteString> for SafeBuffer {
    fn from(s: ByteString) -> Self {
        Self::new(s)
    }
}

impl From<&str> for SafeBuffer {
    fn from(s: &str) -> Self {
        Self::from_str(s)
    }
}

impl From<SafeBuffer> for ByteString {
    fn from(s: SafeBuffer) -> ByteString {
        s.0.into()
    }
}

impl From<&ByteString> for SafeBuffer {
    fn from(s: &ByteString) -> Self {
        Self::from(s.as_str())
    }
}