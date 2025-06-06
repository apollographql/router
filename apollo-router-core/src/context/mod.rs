#[cfg(test)]
mod tests;

use quick_cache::sync::Cache;
use std::any::{Any, TypeId};
use std::ops::Deref;
use std::sync::Arc;

/// A trait for types that can be stored in the context.
/// Any type that is Clone, Send, Sync and 'static can be stored in the context.
trait ContextValue: Clone + Send + Sync + 'static {}

impl<T: Clone + Send + Sync + 'static> ContextValue for T {}

/// A thread-safe context that stores values by type.
///
/// The context provides a simple way to store and retrieve values by their type.
/// It's designed to be used across multiple threads and is safe for concurrent access.
///
/// # Performance Considerations
///
/// Values are cloned when retrieved from the context. For types that are expensive to clone,
/// consider wrapping them in an `Arc` before storing them in the context:
///
/// ```rust
/// use apollo_router_core::Context;
/// use std::sync::Arc;
///
/// let context = Context::new();
///
/// // For expensive types, wrap in Arc
/// let expensive_data = Arc::new(ExpensiveType::new());
/// context.insert(expensive_data.clone());
///
/// // Retrieving is now cheap
/// let retrieved = context.get::<Arc<ExpensiveType>>().unwrap();
/// ```
///
/// # Examples
///
/// ```rust
/// use apollo_router_core::Context;
///
/// let context = Context::new();
///
/// // Store simple values
/// context.insert(42);
/// context.insert("hello".to_string());
///
/// // Retrieve values
/// assert_eq!(context.get::<i32>(), Some(42));
/// assert_eq!(context.get::<String>(), Some("hello".to_string()));
///
/// // Remove values
/// context.remove::<i32>();
/// assert!(context.get::<i32>().is_none());
/// ```
#[derive(Clone)]
pub struct Context {
    cache: Arc<Cache<TypeId, Arc<dyn Any + Send + Sync + 'static>>>,
}

impl Context {
    /// Creates a new empty context.
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Cache::new(1000)),
        }
    }

    /// Gets a value from the context by type.
    /// The value is cloned when retrieved.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Context;
    ///
    /// let context = Context::new();
    /// context.insert(42);
    /// assert_eq!(context.get::<i32>(), Some(42));
    /// ```
    pub fn get<T: ContextValue>(&self) -> Option<T> {
        let type_id = TypeId::of::<T>();
        self.cache.get(&type_id).map(|value| {
            value
                .downcast::<T>()
                .expect("Value is keyed by type id, qed")
                .deref()
                .clone()
        })
    }

    /// Inserts a value into the context.
    /// If a value of the same type already exists, it will be overwritten.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Context;
    /// use std::sync::Arc;
    ///
    /// let context = Context::new();
    ///
    /// // Simple value
    /// context.insert(42);
    ///
    /// // Expensive to clone value
    /// let expensive = Arc::new(ExpensiveType::new());
    /// context.insert(expensive.clone());
    /// ```
    pub fn insert<T: ContextValue>(&self, value: T) {
        let type_id = TypeId::of::<T>();
        let value = Arc::new(value);
        self.cache.insert(type_id, value);
    }

    /// Removes a value from the context.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Context;
    ///
    /// let context = Context::new();
    /// context.insert(42);
    /// context.remove::<i32>();
    /// assert!(context.get::<i32>().is_none());
    /// ```
    pub fn remove<T: ContextValue>(&self) {
        let type_id = TypeId::of::<T>();
        self.cache.remove(&type_id);
    }
}
