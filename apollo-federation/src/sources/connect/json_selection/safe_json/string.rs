use std::{borrow::Borrow, ops::Add};

use percent_encoding::percent_encode;
use serde_json_bytes::ByteString;

#[derive(Debug, Clone, Eq, PartialOrd, Ord)]
pub enum SafeString {
    Safe(ByteString),
    Unsafe(ByteString),
}

/*
safe + unsafe = safe(self + encode(1))
unsafe + safe = safe(encode(self) + 1)
*/

fn escape(s: &str) -> String {
    percent_encode(s.as_bytes(), percent_encoding::NON_ALPHANUMERIC).to_string()
}

impl SafeString {
    pub(crate) fn escape(self) -> ByteString {
        match self {
            SafeString::Unsafe(s) => percent_encode(s.inner(), percent_encoding::NON_ALPHANUMERIC)
                .to_string()
                .into(),
            SafeString::Safe(safe) => safe,
        }
    }
}

impl Add<&SafeString> for SafeString {
    type Output = Self;

    fn add(self, other: &Self) -> Self::Output {
        use SafeString::*;
        match (self, other) {
            (Safe(s1), Safe(s2)) => {
                let s = s1.as_str().to_string() + s2.as_str();
                Safe(s.into())
            }
            (Unsafe(s1), Unsafe(s2)) => {
                let s = s1.as_str().to_string() + s2.as_str();
                Unsafe(s.into())
            }
            (Safe(s1), Unsafe(s2)) => {
                let s = s1.as_str().to_string() + &escape(s2.as_str());
                Safe(s.into())
            }
            (Unsafe(s1), Safe(s2)) => {
                let s = escape(s1.as_str()) + s2.as_str();
                Safe(s.into())
            }
        }
    }
}

impl PartialEq for SafeString {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (SafeString::Safe(s1), SafeString::Safe(s2)) => s1 == s2,
            (SafeString::Unsafe(s1), SafeString::Unsafe(s2)) => s1 == s2,
            (SafeString::Safe(s1), SafeString::Unsafe(s2)) => s1 == s2,
            (SafeString::Unsafe(s1), SafeString::Safe(s2)) => s1 == s2,
        }
    }
}

impl std::hash::Hash for SafeString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            SafeString::Safe(s) => s.hash(state),
            SafeString::Unsafe(s) => s.hash(state),
        }
    }
}

impl Borrow<str> for SafeString {
    fn borrow(&self) -> &str {
        match self {
            SafeString::Safe(s) => s.as_str(),
            SafeString::Unsafe(s) => s.as_str(),
        }
    }
}

impl From<String> for SafeString {
    fn from(s: String) -> Self {
        Self::Unsafe(s.into())
    }
}

impl From<&str> for SafeString {
    fn from(s: &str) -> Self {
        Self::Unsafe(s.into())
    }
}

pub trait Join {
    fn join(self, separator: &SafeString) -> SafeString;
}

impl<I> Join for I
where
    I: IntoIterator<Item = SafeString>,
{
    fn join(self, separator: &SafeString) -> SafeString {
        let mut iter = self.into_iter();
        let first = match iter.next() {
            Some(s) => s,
            None => return SafeString::Unsafe("".into()),
        };

        let mut result = first;
        for s in iter {
            result = result + separator;
            result = result + &s;
        }
        result
    }
}
