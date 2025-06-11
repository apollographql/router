#[cfg(test)]
mod tests;

use std::fmt::Debug;

/// A trait for types that can be stored in the context.
/// Any type that is Clone, Send, Sync and 'static can be stored in the context.
pub trait ExtensionValue: Clone + Send + Sync + 'static {}

impl<T: Clone + Send + Sync + 'static> ExtensionValue for T {}

/// A thread-safe context that stores values by type, wrapping `http::Extensions`.
///
/// Extensions provides a simple wrapper around `http::Extensions` for type-safe
/// storage and retrieval of values during request processing.
///
/// # Basic Usage
///
/// ```rust
/// use apollo_router_core::Extensions;
///
/// let mut extensions = Extensions::default();
/// extensions.insert("test_value".to_string());
/// extensions.insert(42i32);
///
/// assert_eq!(extensions.get::<String>(), Some("test_value".to_string()));
/// assert_eq!(extensions.get::<i32>(), Some(42));
/// ```
///
/// # Cloning Extensions
///
/// Extensions can be cloned to create independent copies:
///
/// ```rust
/// use apollo_router_core::Extensions;
///
/// let mut original = Extensions::default();
/// original.insert("original_value".to_string());
///
/// let mut copy = original.clone();
/// copy.insert(42i32); // Add to copy
///
/// // Both have independent values
/// assert_eq!(original.get::<String>(), Some("original_value".to_string()));
/// assert_eq!(original.get::<i32>(), None); // Copy's value not in original
///
/// assert_eq!(copy.get::<String>(), Some("original_value".to_string())); // Cloned value
/// assert_eq!(copy.get::<i32>(), Some(42)); // Copy's own value
/// ```
///
/// # Conversion to/from http::Extensions
///
/// Extensions can be converted to and from `http::Extensions` for interoperability:
///
/// ```rust
/// use apollo_router_core::Extensions;
///
/// let mut extensions = Extensions::default();
/// extensions.insert("test".to_string());
///
/// // Convert to http::Extensions
/// let http_extensions: http::Extensions = extensions.into();
///
/// // Convert back to Extensions
/// let extensions: Extensions = http_extensions.into();
/// assert_eq!(extensions.get::<String>(), Some("test".to_string()));
/// ```
///
/// # Performance Considerations
///
/// Values are cloned when retrieved from the context. For types that are expensive to clone,
/// consider wrapping them in an `Arc` before storing them in the context:
///
/// ```rust
/// use apollo_router_core::Extensions;
/// use std::sync::Arc;
///
/// let mut extensions = Extensions::default();
///
/// // For expensive types, wrap in Arc
/// let expensive_data = Arc::new(vec![0u8; 1000]); // Large vector
/// extensions.insert(expensive_data.clone());
///
/// // Retrieving is now cheap (cloning Arc, not the vector)
/// let retrieved = extensions.get::<Arc<Vec<u8>>>().unwrap();
/// assert_eq!(retrieved.len(), 1000);
/// ```
#[derive(Clone)]
pub struct Extensions {
    inner: http::Extensions,
}

impl Default for Extensions {
    fn default() -> Self {
        Self::new()
    }
}

impl Extensions {
    /// Creates a new empty Extensions.
    pub fn new() -> Self {
        Self {
            inner: http::Extensions::new(),
        }
    }

    /// Gets a value from the context by type.
    /// The value is cloned when retrieved.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Extensions;
    ///
    /// let mut extensions = Extensions::default();
    /// extensions.insert(42);
    /// assert_eq!(extensions.get::<i32>(), Some(42));
    /// ```
    pub fn get<T: ExtensionValue>(&self) -> Option<T> {
        self.inner.get::<T>().cloned()
    }

    /// Inserts a value into the Extensions.
    /// If a value of the same type already exists, it will be overwritten.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Extensions;
    /// use std::sync::Arc;
    ///
    /// let mut extensions = Extensions::default();
    ///
    /// // Simple value
    /// extensions.insert(42);
    ///
    /// // Expensive to clone value (wrap in Arc)
    /// let expensive = Arc::new(vec![0u8; 1000]); // Large vector
    /// extensions.insert(expensive.clone());
    ///
    /// // Verify both values can be retrieved
    /// assert_eq!(extensions.get::<i32>(), Some(42));
    /// assert_eq!(extensions.get::<Arc<Vec<u8>>>().unwrap().len(), 1000);
    /// ```
    pub fn insert<T: ExtensionValue>(&mut self, value: T) {
        self.inner.insert(value);
    }

    /// Removes a value from the Extensions by type.
    /// Returns the removed value if it existed.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Extensions;
    ///
    /// let mut extensions = Extensions::default();
    /// extensions.insert(42i32);
    /// extensions.insert("test".to_string());
    ///
    /// // Remove the integer value
    /// let removed = extensions.remove::<i32>();
    /// assert_eq!(removed, Some(42));
    ///
    /// // Value is no longer available
    /// assert_eq!(extensions.get::<i32>(), None);
    ///
    /// // String value is still there
    /// assert_eq!(extensions.get::<String>(), Some("test".to_string()));
    /// ```
    pub fn remove<T: ExtensionValue>(&mut self) -> Option<T> {
        self.inner.remove::<T>()
    }
}

impl From<Extensions> for http::Extensions {
    /// Convert Extensions to http::Extensions.
    /// Extracts the inner http::Extensions.
    fn from(extensions: Extensions) -> Self {
        extensions.inner
    }
}

impl From<http::Extensions> for Extensions {
    /// Convert http::Extensions to Extensions.
    /// Wraps the http::Extensions in a new Extensions.
    fn from(http_ext: http::Extensions) -> Self {
        Self { inner: http_ext }
    }
}

impl Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extensions")
            .field("inner", &"http::Extensions")
            .finish()
    }
}
