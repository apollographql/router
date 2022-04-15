//! Provide a [`Context`] for the plugin chain of responsibilities.
//!
//! Router plugins accept a mutable [`Context`] when invoked and this contains a DashMap which
//! allows additional data to be passed back and forth along the request invocation pipeline.

use crate::prelude::graphql::*;
use dashmap::DashMap;
use serde::Serialize;
use std::sync::Arc;
use tower::BoxError;

/// Holds [`Context`] entries.
pub type Entries = Arc<DashMap<String, Value>>;

/// Context for a [`http_compat::Request`]
#[derive(Clone, Debug)]
pub struct Context {
    // Allows adding custom entries to the context.
    // This should be private, the only reason it's public for now (and should disappear) is for the RHAI plugin.
    // Please do not use Entries directly, but use public api.
    pub entries: Entries,
}

impl Context {
    pub fn new() -> Self {
        Context {
            entries: Default::default(),
        }
    }
}

impl Context {
    pub fn get<K, V>(&self, key: K) -> Result<Option<V>, BoxError>
    where
        K: Into<String>,
        V: for<'de> serde::Deserialize<'de>,
    {
        self.entries
            .get(&key.into())
            .map(|v| serde_json_bytes::from_value(v.value().clone()))
            .transpose()
            .map_err(|e| e.into())
    }

    pub fn insert<K, V>(&self, key: K, value: V) -> Result<Option<V>, BoxError>
    where
        K: Into<String>,
        V: for<'de> serde::Deserialize<'de> + Serialize,
    {
        match serde_json_bytes::to_value(value) {
            Ok(value) => self
                .entries
                .insert(key.into(), value)
                .map(|v| serde_json_bytes::from_value(v))
                .transpose()
                .map_err(|e| e.into()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn upsert<K, V>(
        &self,
        key: K,
        upsert: impl Fn(V) -> V,
        default: impl Fn() -> V,
    ) -> Result<(), BoxError>
    where
        K: Into<String>,
        V: for<'de> serde::Deserialize<'de> + Serialize,
    {
        let key = key.into();
        self.entries
            .entry(key.clone())
            .or_try_insert_with(|| serde_json_bytes::to_value((default)()))?;
        let mut result = Ok(());
        self.entries
            .alter(&key, |_, v| match serde_json_bytes::from_value(v.clone()) {
                Ok(value) => match serde_json_bytes::to_value((upsert)(value)) {
                    Ok(value) => value,
                    Err(e) => {
                        result = Err(e);
                        v
                    }
                },
                Err(e) => {
                    result = Err(e);
                    v
                }
            });
        result.map_err(|e| e.into())
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod test {
    use crate::Context;

    #[test]
    fn test_context_insert() {
        let c = Context::new();
        assert!(c.insert("key1", 1).is_ok());
        assert_eq!(c.get("key1").unwrap(), Some(1));
    }

    #[test]
    fn test_context_overwrite() {
        let c = Context::new();
        assert!(c.insert("overwrite", 2).is_ok());
        assert!(c.insert("overwrite", 3).is_ok());
        assert_eq!(c.get("overwrite").unwrap(), Some(3));
    }

    #[test]
    fn test_context_upsert() {
        let c = Context::new();
        assert!(c.insert("present", 1).is_ok());
        assert!(c.upsert("present", |v| v + 1, || 0).is_ok());
        assert_eq!(c.get("present").unwrap(), Some(2));
        assert!(c.upsert("not_present", |v| v + 1, || 0).is_ok());
        assert_eq!(c.get("not_present").unwrap(), Some(1));
    }

    #[test]
    fn test_context_marshall_errors() {
        let c = Context::new();
        assert!(c.insert("string", "Some value".to_string()).is_ok());
        assert!(c.upsert("string", |v| v + 1, || 0).is_err());
    }
}
