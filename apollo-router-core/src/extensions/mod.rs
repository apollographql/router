#[cfg(test)]
mod tests;

use quick_cache::sync::Cache;
use std::any::{Any, TypeId};
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::Arc;

/// A trait for types that can be stored in the context.
/// Any type that is Clone, Send, Sync and 'static can be stored in the context.
trait ExtensionValue: Clone + Send + Sync + 'static {}

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
/// let root = Extensions::default();
/// root.insert("upstream_value".to_string());
///
/// let child = root.extend();
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
/// # Mutable Values
///
/// If you need values that can be modified by downstream services, use explicit
/// synchronization primitives rather than relying on layer precedence:
///
/// ```rust
/// use apollo_router_core::Extensions;
/// use std::sync::{Arc, Mutex};
///
/// let root = Extensions::default();
/// let mutable_counter = Arc::new(Mutex::new(0));
/// root.insert(mutable_counter.clone());
///
/// let child = root.extend();
/// // Downstream can modify the shared value
/// let counter = child.get::<Arc<Mutex<i32>>>().unwrap();
/// *counter.lock().unwrap() += 1;
///
/// // Both layers see the updated value
/// assert_eq!(*root.get::<Arc<Mutex<i32>>>().unwrap().lock().unwrap(), 1);
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
/// let context = Extensions::default();
///
/// // For expensive types, wrap in Arc
/// let expensive_data = Arc::new(ExpensiveType::new());
/// context.insert(expensive_data.clone());
///
/// // Retrieving is now cheap
/// let retrieved = context.get::<Arc<ExpensiveType>>().unwrap();
/// ```
#[derive(Clone)]
pub struct Extensions {
    inner: Arc<ExtensionsInner>,
}

struct ExtensionsInner {
    cache: Cache<TypeId, Arc<dyn Any + Send + Sync + 'static>>,
    parent: Option<Arc<ExtensionsInner>>,
}

impl Default for Extensions {
    fn default() -> Self {
        Self::new(100)
    }
}

impl Extensions {
    /// Creates a new empty context.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(ExtensionsInner {
                cache: Cache::new(capacity),
                parent: None,
            }),
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
    /// let parent = Extensions::default();
    /// parent.insert("parent_value".to_string());
    ///
    /// let child = parent.extend();
    /// child.insert(42i32); // New type, will be accessible
    /// child.insert("child_attempt".to_string()); // Same type, parent wins
    ///
    /// assert_eq!(child.get::<String>(), Some("parent_value".to_string())); // Parent value
    /// assert_eq!(child.get::<i32>(), Some(42)); // Child value (new type)
    /// assert_eq!(parent.get::<i32>(), None); // Parent doesn't see child values
    /// ```
    pub fn extend(&self) -> Self {
        Self {
            inner: Arc::new(ExtensionsInner {
                cache: Cache::new(100), // Default capacity for extended layers
                parent: Some(self.inner.clone()),
            }),
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
    /// let context = Extensions::default();
    /// context.insert(42);
    /// assert_eq!(context.get::<i32>(), Some(42));
    /// ```
    pub fn get<T: ExtensionValue>(&self) -> Option<T> {
        let type_id = TypeId::of::<T>();
        self.get_from_layer(&self.inner, type_id)
    }

    fn get_from_layer<T: ExtensionValue>(
        &self,
        layer: &ExtensionsInner,
        type_id: TypeId,
    ) -> Option<T> {
        // First check parent layers (upstream takes precedence)
        if let Some(parent) = &layer.parent {
            if let Some(value) = self.get_from_layer::<T>(parent, type_id) {
                return Some(value);
            }
        }

        // If not found in parents, check current layer
        if let Some(value) = layer.cache.get(&type_id) {
            Some(
                value
                    .downcast::<T>()
                    .expect("Value is keyed by type id, qed")
                    .deref()
                    .clone(),
            )
        } else {
            None
        }
    }

    /// Inserts a value into the current context layer.
    /// If a value of the same type already exists in this layer, it will be overwritten.
    /// Parent layer values are not affected.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use apollo_router_core::Extensions;
    /// use std::sync::Arc;
    ///
    /// let context = Extensions::default();
    ///
    /// // Simple value
    /// context.insert(42);
    ///
    /// // Expensive to clone value
    /// let expensive = Arc::new(ExpensiveType::new());
    /// context.insert(expensive.clone());
    /// ```
    pub fn insert<T: ExtensionValue>(&self, value: T) {
        let type_id = TypeId::of::<T>();
        let value = Arc::new(value);
        self.inner.cache.insert(type_id, value);
    }
}

impl Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut type_names = Vec::new();
        self.collect_type_names(&self.inner, &mut type_names);
        type_names.sort();
        type_names.dedup(); // Remove duplicates in case of shadowing

        f.debug_struct("Extensions")
            .field("types", &type_names)
            .finish()
    }
}

impl Extensions {
    fn collect_type_names(&self, layer: &ExtensionsInner, type_names: &mut Vec<&'static str>) {
        // Collect types from current layer
        for (_, value) in layer.cache.iter() {
            type_names.push(std::any::type_name_of_val(value.as_ref()));
        }

        // Collect types from parent layer
        if let Some(parent) = &layer.parent {
            self.collect_type_names(parent, type_names);
        }
    }
}
