//! Provide a [`Context`] for the plugin chain of responsibilities.
//!
//! Router plugins accept a mutable [`Context`] when invoked and this contains a DashMap which
//! allows additional data to be passed back and forth along the request invocation pipeline.

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use apollo_compiler::ExecutableDocument;
use dashmap::mapref::multiple::RefMulti;
use dashmap::mapref::multiple::RefMutMulti;
use dashmap::DashMap;
use derivative::Derivative;
use extensions::sync::ExtensionsMutex;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

use crate::json_ext::Value;
use crate::services::layers::query_analysis::ParsedDocument;

pub(crate) mod extensions;

/// The key of the resolved operation name. This is subject to change and should not be relied on.
pub(crate) const OPERATION_NAME: &str = "operation_name";
/// The key of the resolved operation kind. This is subject to change and should not be relied on.
pub(crate) const OPERATION_KIND: &str = "operation_kind";

/// Holds [`Context`] entries.
pub(crate) type Entries = Arc<DashMap<String, Value>>;

/// A map of arbitrary JSON values, for use by plugins.
///
/// Context makes use of [`DashMap`] under the hood which tries to handle concurrency
/// by allowing concurrency across threads without requiring locking. This is great
/// for usability but could lead to surprises when updates are highly contested.
///
/// Within the router, contention is likely to be highest within plugins which
/// provide [`crate::services::SubgraphRequest`] or
/// [`crate::services::SubgraphResponse`] processing. At such times,
/// plugins should restrict themselves to the [`Context::get`] and [`Context::upsert`]
/// functions to minimise the possibility of mis-sequenced updates.
#[derive(Clone, Deserialize, Serialize, Derivative)]
#[serde(default)]
#[derivative(Debug)]
pub struct Context {
    // Allows adding custom entries to the context.
    entries: Entries,

    #[serde(skip)]
    extensions: ExtensionsMutex,

    /// Creation time
    #[serde(skip)]
    pub(crate) created_at: Instant,

    #[serde(skip)]
    #[derivative(Debug = "ignore")]
    busy_timer: Arc<Mutex<BusyTimer>>,

    #[serde(skip)]
    pub(crate) id: String,
}

impl Context {
    /// Create a new context.
    pub fn new() -> Self {
        let id = uuid::Uuid::new_v4()
            .as_hyphenated()
            .encode_lower(&mut uuid::Uuid::encode_buffer())
            .to_string();
        Context {
            entries: Default::default(),
            extensions: ExtensionsMutex::default(),
            created_at: Instant::now(),
            busy_timer: Arc::new(Mutex::new(BusyTimer::new())),
            id,
        }
    }
}

impl Context {
    /// Returns extensions of the context.
    ///
    /// You can use `Extensions` to pass data between plugins that is not serializable. Such data is not accessible from Rhai or co-processoers.
    ///
    /// It is CRITICAL to avoid holding on to the mutex guard for too long, particularly across async calls.
    /// Doing so may cause performance degradation or even deadlocks.
    ///
    /// See related clippy lint for examples: <https://rust-lang.github.io/rust-clippy/master/index.html#/await_holding_lock>
    pub fn extensions(&self) -> &ExtensionsMutex {
        &self.extensions
    }

    /// Returns true if the context contains a value for the specified key.
    pub fn contains_key<K>(&self, key: K) -> bool
    where
        K: Into<String>,
    {
        self.entries.contains_key(&key.into())
    }

    /// Get a value from the context using the provided key.
    ///
    /// Semantics:
    ///  - If the operation fails, that's because we can't deserialize the value.
    ///  - If the operation succeeds, the value is an [`Option`].
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

    /// Insert a value int the context using the provided key and value.
    ///
    /// Semantics:
    ///  - If the operation fails, then the pair has not been inserted.
    ///  - If the operation succeeds, the result is the old value as an [`Option`].
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

    /// Insert a value in the context using the provided key and value.
    ///
    /// Semantics: the result is the old value as an [`Option`].
    pub fn insert_json_value<K>(&self, key: K, value: Value) -> Option<Value>
    where
        K: Into<String>,
    {
        self.entries.insert(key.into(), value)
    }

    /// Get a json value from the context using the provided key.
    pub fn get_json_value<K>(&self, key: K) -> Option<Value>
    where
        K: Into<String>,
    {
        self.entries.get(&key.into()).map(|v| v.value().clone())
    }

    /// Upsert a value in the context using the provided key and resolving
    /// function.
    ///
    /// The resolving function must yield a value to be used in the context. It
    /// is provided with the current value to use in evaluating which value to
    /// yield.
    ///
    /// Semantics:
    ///  - If the operation fails, then the pair has not been inserted (or a current
    ///    value updated).
    ///  - If the operation succeeds, the pair have either updated an existing value
    ///    or been inserted.
    pub fn upsert<K, V>(&self, key: K, upsert: impl FnOnce(V) -> V) -> Result<(), BoxError>
    where
        K: Into<String>,
        V: for<'de> serde::Deserialize<'de> + Serialize + Default,
    {
        let key = key.into();
        self.entries
            .entry(key.clone())
            .or_try_insert_with(|| serde_json_bytes::to_value::<V>(Default::default()))?;
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

    /// Upsert a JSON value in the context using the provided key and resolving
    /// function.
    ///
    /// The resolving function must yield a value to be used in the context. It
    /// is provided with the current value to use in evaluating which value to
    /// yield.
    pub(crate) fn upsert_json_value<K>(&self, key: K, upsert: impl FnOnce(Value) -> Value)
    where
        K: Into<String>,
    {
        let key = key.into();
        self.entries.entry(key.clone()).or_insert(Value::Null);
        self.entries.alter(&key, |_, v| upsert(v));
    }

    /// Convert the context into an iterator.
    pub(crate) fn try_into_iter(
        self,
    ) -> Result<impl IntoIterator<Item = (String, Value)>, BoxError> {
        Ok(Arc::try_unwrap(self.entries)
            .map_err(|_e| anyhow::anyhow!("cannot take ownership of dashmap"))?
            .into_iter())
    }

    /// Iterate over the entries.
    pub fn iter(&self) -> impl Iterator<Item = RefMulti<'_, String, Value>> + '_ {
        self.entries.iter()
    }

    /// Iterate mutably over the entries.
    pub fn iter_mut(&self) -> impl Iterator<Item = RefMutMulti<'_, String, Value>> + '_ {
        self.entries.iter_mut()
    }

    /// Notify the busy timer that we're waiting on a network request
    pub(crate) fn enter_active_request(&self) -> BusyTimerGuard {
        self.busy_timer.lock().increment_active_requests();
        BusyTimerGuard {
            busy_timer: self.busy_timer.clone(),
        }
    }

    /// How much time was spent working on the request
    pub(crate) fn busy_time(&self) -> Duration {
        self.busy_timer.lock().current()
    }

    pub(crate) fn extend(&self, other: &Context) {
        for kv in other.entries.iter() {
            self.entries.insert(kv.key().clone(), kv.value().clone());
        }
    }

    /// Read only access to the executable document. This is UNSTABLE and may be changed or removed in future router releases.
    /// In addition, ExecutableDocument is UNSTABLE, and may be changed or removed in future apollo-rs releases.
    #[doc(hidden)]
    pub fn unsupported_executable_document(&self) -> Option<Arc<ExecutableDocument>> {
        self.extensions()
            .lock()
            .get::<ParsedDocument>()
            .map(|d| d.executable.clone())
    }
}

pub(crate) struct BusyTimerGuard {
    busy_timer: Arc<Mutex<BusyTimer>>,
}

impl Drop for BusyTimerGuard {
    fn drop(&mut self) {
        self.busy_timer.lock().decrement_active_requests()
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

/// Measures the total overhead of the router
///
/// This works by measuring the time spent executing when there is no active subgraph request.
/// This is still not a perfect solution, there are cases where preprocessing a subgraph request
/// happens while another one is running and still shifts the end of the span, but for now this
/// should serve as a reasonable solution without complex post processing of spans
pub(crate) struct BusyTimer {
    active_requests: u32,
    busy_ns: Duration,
    start: Option<Instant>,
}

impl BusyTimer {
    pub(crate) fn new() -> Self {
        BusyTimer::default()
    }

    pub(crate) fn increment_active_requests(&mut self) {
        if self.active_requests == 0 {
            if let Some(start) = self.start.take() {
                self.busy_ns += start.elapsed();
            }
            self.start = None;
        }

        self.active_requests += 1;
    }

    pub(crate) fn decrement_active_requests(&mut self) {
        self.active_requests -= 1;

        if self.active_requests == 0 {
            self.start = Some(Instant::now());
        }
    }

    pub(crate) fn current(&mut self) -> Duration {
        if let Some(start) = self.start {
            self.busy_ns + start.elapsed()
        } else {
            self.busy_ns
        }
    }
}

impl Default for BusyTimer {
    fn default() -> Self {
        Self {
            active_requests: 0,
            busy_ns: Duration::new(0, 0),
            start: Some(Instant::now()),
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

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
        assert!(c.upsert("present", |v: usize| v + 1).is_ok());
        assert_eq!(c.get("present").unwrap(), Some(2));
        assert!(c.upsert("not_present", |v: usize| v + 1).is_ok());
        assert_eq!(c.get("not_present").unwrap(), Some(1));
    }

    #[test]
    fn test_context_marshall_errors() {
        let c = Context::new();
        assert!(c.insert("string", "Some value".to_string()).is_ok());
        assert!(c.upsert("string", |v: usize| v + 1).is_err());
    }

    #[test]
    fn it_iterates_over_context() {
        let c = Context::new();
        assert!(c.insert("one", 1).is_ok());
        assert!(c.insert("two", 2).is_ok());
        assert_eq!(c.iter().count(), 2);
        assert_eq!(
            c.iter()
                // Fiddly because of the conversion from bytes to usize, but ...
                .map(|r| serde_json_bytes::from_value::<usize>(r.value().clone()).unwrap())
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn it_iterates_mutably_over_context() {
        let c = Context::new();
        assert!(c.insert("one", 1usize).is_ok());
        assert!(c.insert("two", 2usize).is_ok());
        assert_eq!(c.iter().count(), 2);
        c.iter_mut().for_each(|mut r| {
            // Fiddly because of the conversion from bytes to usize, but ...
            let new: usize = serde_json_bytes::from_value::<usize>(r.value().clone()).unwrap() + 1;
            *r = new.into();
        });
        assert_eq!(c.get("one").unwrap(), Some(2));
        assert_eq!(c.get("two").unwrap(), Some(3));
    }

    #[test]
    fn context_extensions() {
        // This is mostly tested in the extensions module.
        let c = Context::new();
        let mut extensions = c.extensions().lock();
        extensions.insert(1usize);
        let v = extensions.get::<usize>();
        assert_eq!(v, Some(&1usize));
    }

    #[test]
    fn test_executable_document_access() {
        let c = Context::new();
        assert!(c.unsupported_executable_document().is_none());
        c.extensions().lock().insert(Arc::new(
            crate::services::layers::query_analysis::ParsedDocumentInner {
                ast: Default::default(),
                executable: Default::default(),
                parse_errors: Default::default(),
                validation_errors: Default::default(),
            },
        ));
        assert!(c.unsupported_executable_document().is_some());
    }
}
