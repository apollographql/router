use std::{
    borrow::Borrow,
    fmt::{self, Display},
    ops::Add,
};

use percent_encoding::percent_encode;
use serde::Serialize;
use serde_json_bytes::ByteString;

#[derive(Debug, Clone, Eq, PartialOrd, Ord)]
pub enum SafeString {
    Trusted(ByteString),
    AutoEncoded(ByteString),
}

/*
safe + unsafe = safe(self + encode(1))
unsafe + safe = safe(encode(self) + 1)
*/

fn escape(s: &str) -> String {
    percent_encode(s.as_bytes(), percent_encoding::NON_ALPHANUMERIC).to_string()
}

impl SafeString {
    pub fn as_str(&self) -> &str {
        match self {
            SafeString::Trusted(byte_string) => byte_string.as_str(),
            SafeString::AutoEncoded(byte_string) => byte_string.as_str(),
        }
    }

    pub(crate) fn escape(self) -> ByteString {
        match self {
            SafeString::AutoEncoded(s) => {
                percent_encode(s.inner(), percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
                    .into()
            }
            SafeString::Trusted(safe) => safe,
        }
    }
}

impl Add<&SafeString> for SafeString {
    type Output = Self;

    fn add(self, other: &Self) -> Self::Output {
        use SafeString::*;
        match (self, other) {
            (Trusted(s1), Trusted(s2)) => {
                let s = s1.as_str().to_string() + s2.as_str();
                Trusted(s.into())
            }
            (AutoEncoded(s1), AutoEncoded(s2)) => {
                let s = s1.as_str().to_string() + s2.as_str();
                AutoEncoded(s.into())
            }
            (Trusted(s1), AutoEncoded(s2)) => {
                let s = s1.as_str().to_string() + &escape(s2.as_str());
                Trusted(s.into())
            }
            (AutoEncoded(s1), Trusted(s2)) => {
                let s = escape(s1.as_str()) + s2.as_str();
                Trusted(s.into())
            }
        }
    }
}

impl Default for SafeString {
    fn default() -> Self {
        "".into()
    }
}

impl PartialEq for SafeString {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (SafeString::Trusted(s1), SafeString::Trusted(s2)) => s1 == s2,
            (SafeString::AutoEncoded(s1), SafeString::AutoEncoded(s2)) => s1 == s2,
            (SafeString::Trusted(s1), SafeString::AutoEncoded(s2)) => s1 == s2,
            (SafeString::AutoEncoded(s1), SafeString::Trusted(s2)) => s1 == s2,
        }
    }
}

impl PartialEq<ByteString> for SafeString {
    fn eq(&self, other: &ByteString) -> bool {
        match self {
            Self::Trusted(safe) => safe == other,
            Self::AutoEncoded(encoded) => encoded == other,
        }
    }
}

impl std::hash::Hash for SafeString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            SafeString::Trusted(s) => s.hash(state),
            SafeString::AutoEncoded(s) => s.hash(state),
        }
    }
}

impl Borrow<str> for SafeString {
    fn borrow(&self) -> &str {
        match self {
            SafeString::Trusted(s) => s.as_str(),
            SafeString::AutoEncoded(s) => s.as_str(),
        }
    }
}

impl From<String> for SafeString {
    fn from(s: String) -> Self {
        Self::AutoEncoded(s.into())
    }
}

impl From<&str> for SafeString {
    fn from(s: &str) -> Self {
        Self::AutoEncoded(s.into())
    }
}

impl Display for SafeString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                SafeString::Trusted(byte_string) => byte_string.as_str(),
                SafeString::AutoEncoded(byte_string) => byte_string.as_str(),
            }
        )
    }
}

impl Serialize for SafeString {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ::serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}
