#[cfg(test)]
mod tests;

use std::fmt::Debug;
use std::sync::Arc;

/// A trait for types that can be stored in the context.
/// Any type that is Clone, Send, Sync and 'static can be stored in the context.
pub trait ExtensionValue: Clone + Send + Sync + 'static {}

impl<T: Clone + Send + Sync + 'static> ExtensionValue for T {}

/// A thread-safe context that stores values by type with hierarchical inheritance.
///
/// Extensions provides a layered context system where each layer can add new values
/// while inheriting from its parent layer. Values are looked up in parent layers
/// first, then in the current layer if not found. This ensures upstream decisions
/// take precedence and cannot be accidentally overridden.
///
/// # Hierarchical Structure and Value Precedence
///
/// Extensions can be extended to create new layers. Parent values always take
/// precedence over child values:
///
/// ```rust
/// use apollo_router_core::Extensions;
///
/// let mut root = Extensions::default();
/// root.insert("upstream_value".to_string());
///
/// let mut child = root.extend();
/// child.insert(42i32); // New type, allowed
/// child.insert("downstream_attempt".to_string()); // Same type as parent
///
/// // Parent values always win for existing types
/// assert_eq!(child.get::<String>(), Some("upstream_value".to_string()));
/// // New types in child are accessible
/// assert_eq!(child.get::<i32>(), Some(42));
///
/// // Parent only has its own values
/// assert_eq!(root.get::<String>(), Some("upstream_value".to_string()));
/// assert_eq!(root.get::<i32>(), None);
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
/// let mut context = Extensions::default();
///
/// // For expensive types, wrap in Arc
/// let expensive_data = Arc::new(vec![0u8; 1000]); // Large vector
/// context.insert(expensive_data.clone());
///
/// // Retrieving is now cheap (cloning Arc, not the vector)
/// let retrieved = context.get::<Arc<Vec<u8>>>().unwrap();
/// assert_eq!(retrieved.len(), 1000);
/// ```
#[derive(Clone)]
pub struct Extensions {
    inner: ExtensionsInner,
    parent: Option<Arc<Extensions>>,
}

#[derive(Clone)]
enum ExtensionsInner {
    /// Native http::Extensions storage
    Native(http::Extensions),
    /// Wrapped http::Extensions (when converted from external http::Extensions)
    HttpWrapped(http::Extensions),
}

impl ExtensionsInner {
    fn get<T: ExtensionValue>(&self) -> Option<T> {
        match self {
            ExtensionsInner::Native(ext) => ext.get::<T>().cloned(),
            ExtensionsInner::HttpWrapped(ext) => ext.get::<T>().cloned(),
        }
    }
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
            inner: ExtensionsInner::Native(http::Extensions::new()),
            parent: None,
        }
    }

    /// Creates a new Extensions layer that extends this one.
    /// The new layer inherits values from this Extensions while allowing
    /// new values to be added without affecting the parent. Parent values
    /// take precedence over child values for the same type.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Extensions;
    ///
    /// let mut parent = Extensions::default();
    /// parent.insert("parent_value".to_string());
    ///
    /// let mut child = parent.extend();
    /// child.insert(42i32); // New type, will be accessible
    /// child.insert("child_attempt".to_string()); // Same type, parent wins
    ///
    /// assert_eq!(child.get::<String>(), Some("parent_value".to_string())); // Parent value
    /// assert_eq!(child.get::<i32>(), Some(42)); // Child value (new type)
    /// assert_eq!(parent.get::<i32>(), None); // Parent doesn't see child values
    /// ```
    pub fn extend(&self) -> Self {
        Self {
            inner: ExtensionsInner::Native(http::Extensions::new()),
            parent: Some(Arc::new(self.clone())),
        }
    }

    /// Gets a value from the context by type.
    /// Searches parent layers first, then the current layer if not found.
    /// This ensures upstream decisions take precedence and cannot be overridden.
    /// The value is cloned when retrieved.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Extensions;
    ///
    /// let mut context = Extensions::default();
    /// context.insert(42);
    /// assert_eq!(context.get::<i32>(), Some(42));
    /// ```
    pub fn get<T: ExtensionValue>(&self) -> Option<T> {
        // First check parent layers (upstream takes precedence)
        if let Some(parent) = &self.parent {
            if let Some(value) = parent.get::<T>() {
                return Some(value);
            }
        }

        // If not found in parents, check current layer
        self.inner.get::<T>()
    }

    /// Inserts a value into the current Extensions layer.
    /// If a value of the same type already exists in this layer, it will be overwritten.
    /// Parent layer values are not affected.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Extensions;
    /// use std::sync::Arc;
    ///
    /// let mut context = Extensions::default();
    ///
    /// // Simple value
    /// context.insert(42);
    ///
    /// // Expensive to clone value (wrap in Arc)
    /// let expensive = Arc::new(vec![0u8; 1000]); // Large vector
    /// context.insert(expensive.clone());
    ///
    /// // Verify both values can be retrieved
    /// assert_eq!(context.get::<i32>(), Some(42));
    /// assert_eq!(context.get::<Arc<Vec<u8>>>().unwrap().len(), 1000);
    /// ```
    pub fn insert<T: ExtensionValue>(&mut self, value: T) {
        match &mut self.inner {
            ExtensionsInner::Native(ext) => {
                ext.insert(value);
            }
            ExtensionsInner::HttpWrapped(ext) => {
                ext.insert(value);
            }
        }
    }
}

impl From<Extensions> for http::Extensions {
    /// Convert Extensions to http::Extensions.
    /// For Native variant, extract the inner http::Extensions.
    /// For HttpWrapped variant, return the wrapped http::Extensions.
    fn from(extensions: Extensions) -> Self {
        match extensions.inner {
            ExtensionsInner::Native(ext) => ext,
            ExtensionsInner::HttpWrapped(ext) => ext,
        }
    }
}

impl From<http::Extensions> for Extensions {
    /// Convert http::Extensions to Extensions.
    /// Creates a new Extensions with HttpWrapped variant.
    fn from(http_ext: http::Extensions) -> Self {
        Self {
            inner: ExtensionsInner::HttpWrapped(http_ext),
            parent: None,
        }
    }
}

impl Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug_struct = f.debug_struct("Extensions");

        // Show current layer type info
        match &self.inner {
            ExtensionsInner::Native(_) => {
                debug_struct.field("layer_type", &"Native");
            }
            ExtensionsInner::HttpWrapped(_) => {
                debug_struct.field("layer_type", &"HttpWrapped");
            }
        }

        // Show if there's a parent
        debug_struct.field("has_parent", &self.parent.is_some());

        debug_struct.finish()
    }
}
