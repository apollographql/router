use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use super::*;
use crate::Extensions;

// Simple value type for basic operations
#[derive(Debug, PartialEq, Clone)]
#[allow(dead_code)]
struct TestValue {
    value: String,
}

// Expensive to clone type that should be wrapped in Arc
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ExpensiveType {
    data: Vec<u8>,
    #[allow(dead_code)]
    count: Arc<AtomicUsize>,
}

impl ExpensiveType {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[test]
fn test_basic_operations() {
    let mut extensions = Extensions::default();

    // Insert and get simple values
    extensions.insert(42);
    extensions.insert("hello".to_string());

    assert_eq!(extensions.get::<i32>(), Some(42));
    assert_eq!(extensions.get::<String>(), Some("hello".to_string()));
}

#[test]
fn test_expensive_type_with_arc() {
    let mut extensions = Extensions::default();

    // Create an expensive type and wrap it in Arc
    let expensive = Arc::new(ExpensiveType::new(1000));
    extensions.insert(expensive.clone());

    // Retrieving is cheap since we're just cloning the Arc
    let retrieved = extensions.get::<Arc<ExpensiveType>>().unwrap();
    assert_eq!(retrieved.data.len(), 1000);

    // Both Arcs point to the same data
    assert!(Arc::ptr_eq(&expensive, &retrieved));
}

#[test]
fn test_multiple_types() {
    let mut extensions = Extensions::default();

    // Store different types
    extensions.insert(42);
    extensions.insert("hello".to_string());
    extensions.insert(ExpensiveType::new(100));

    // Retrieve and verify each type
    assert_eq!(extensions.get::<i32>(), Some(42));
    assert_eq!(extensions.get::<String>(), Some("hello".to_string()));
    assert_eq!(extensions.get::<ExpensiveType>().unwrap().data.len(), 100);
}

#[test]
fn test_overwrite_values() {
    let mut extensions = Extensions::default();

    // Insert initial values
    extensions.insert(42);
    extensions.insert("hello".to_string());

    // Overwrite values
    extensions.insert(43);
    extensions.insert("world".to_string());

    // Verify new values
    assert_eq!(extensions.get::<i32>(), Some(43));
    assert_eq!(extensions.get::<String>(), Some("world".to_string()));
}

#[test]
fn test_cloned_extensions() {
    let mut original = Extensions::default();
    original.insert("original_value".to_string());
    original.insert(100i32);

    let mut copy = original.clone();
    copy.insert(42i32); // Override cloned value
    copy.insert(true); // Add a new value (new type)

    // Copy has overridden and new values
    assert_eq!(copy.get::<i32>(), Some(42)); // Overridden value
    assert_eq!(copy.get::<String>(), Some("original_value".to_string())); // Cloned value
    assert_eq!(copy.get::<bool>(), Some(true)); // Copy's own value

    // Original is not affected by copy's changes
    assert_eq!(original.get::<i32>(), Some(100)); // Original value unchanged
    assert_eq!(original.get::<String>(), Some("original_value".to_string()));
    assert_eq!(original.get::<bool>(), None); // Not available in original
}

#[test]
fn test_multiple_independent_extensions() {
    let mut root = Extensions::default();
    root.insert("root".to_string());

    let mut level1 = root.clone();
    level1.insert(1i32);

    let mut level2 = level1.clone();
    level2.insert(2i16);
    level2.insert(100i32); // Override cloned value

    let mut level3 = level2.clone();
    level3.insert(3i8);
    level3.insert("level3".to_string()); // Override cloned value

    // Level3 has its own values plus cloned values it hasn't overridden
    assert_eq!(level3.get::<String>(), Some("level3".to_string())); // Level3's override
    assert_eq!(level3.get::<i32>(), Some(100)); // Cloned from level2
    assert_eq!(level3.get::<i16>(), Some(2)); // Cloned from level2
    assert_eq!(level3.get::<i8>(), Some(3)); // Level3's own value

    // Level2 has its own values plus cloned values it hasn't overridden
    assert_eq!(level2.get::<String>(), Some("root".to_string())); // Cloned from root
    assert_eq!(level2.get::<i32>(), Some(100)); // Its own override
    assert_eq!(level2.get::<i16>(), Some(2)); // Its own value
    assert_eq!(level2.get::<i8>(), None); // Cannot see level3's value

    // Level1 has its own values plus cloned values
    assert_eq!(level1.get::<String>(), Some("root".to_string())); // Cloned from root
    assert_eq!(level1.get::<i32>(), Some(1)); // Its own value
    assert_eq!(level1.get::<i16>(), None); // Cannot see level2's value
    assert_eq!(level1.get::<i8>(), None); // Cannot see level3's value

    assert_eq!(root.get::<String>(), Some("root".to_string())); // Its own value
    assert_eq!(root.get::<i32>(), None); // Cannot see descendants' values
    assert_eq!(root.get::<i16>(), None);
    assert_eq!(root.get::<i8>(), None);
}

#[test]
fn test_independent_extensions() {
    let mut parent = Extensions::default();
    parent.insert("parent_value".to_string());

    let mut child = parent.clone();
    child.insert("child_value".to_string()); // Override cloned value

    // Child has the overridden value
    assert_eq!(child.get::<String>(), Some("child_value".to_string()));

    // Parent is unaffected and only has its own value
    assert_eq!(parent.get::<String>(), Some("parent_value".to_string()));
}

#[test]
fn test_shared_values_with_sync_primitives() {
    use std::sync::Arc;
    use std::sync::Mutex;

    let mut parent = Extensions::default();
    let counter = Arc::new(Mutex::new(0));
    parent.insert(counter.clone());

    let mut child = parent.clone();
    // Override parent's counter with child's counter
    let child_counter = Arc::new(Mutex::new(10));
    child.insert(child_counter.clone());

    // Modify child's counter
    *child_counter.lock().unwrap() += 5;

    // Each extension has its own independent values after cloning and overriding
    assert_eq!(*parent.get::<Arc<Mutex<i32>>>().unwrap().lock().unwrap(), 0); // Parent's original counter
    assert_eq!(*child.get::<Arc<Mutex<i32>>>().unwrap().lock().unwrap(), 15); // Child's overridden counter (10 + 5)

    // Parent doesn't see child's modifications
    assert_eq!(parent.get::<Arc<Mutex<i32>>>().is_some(), true); // Parent's counter
    assert_eq!(child.get::<Arc<Mutex<i32>>>().is_some(), true); // Child's overridden counter
}

#[test]
fn test_conversion_to_http_extensions() {
    let mut extensions = Extensions::default();
    extensions.insert("test_value".to_string());
    extensions.insert(42i32);

    // Convert to http::Extensions
    let http_ext: http::Extensions = extensions.into();

    // The http::Extensions should contain our values directly (current layer only)
    assert_eq!(http_ext.get::<String>(), Some(&"test_value".to_string()));
    assert_eq!(http_ext.get::<i32>(), Some(&42));
}

#[test]
fn test_conversion_to_http_extensions_with_hierarchy() {
    // Test that conversion only includes current layer, not parent layers
    let mut parent = Extensions::default();
    parent.insert("parent_value".to_string());

    let mut child = parent.clone();
    child.insert(42i32);

    // Convert child to http::Extensions
    let http_ext: http::Extensions = child.into();

    // All values from cloned Extensions should be present
    assert_eq!(http_ext.get::<i32>(), Some(&42)); // Child value present
    assert_eq!(http_ext.get::<String>(), Some(&"parent_value".to_string())); // Parent value also present
}

#[test]
fn test_conversion_from_http_extensions() {
    // Create Extensions and convert to http::Extensions
    let mut original_extensions = Extensions::default();
    original_extensions.insert("original_value".to_string());
    original_extensions.insert(123i32);

    let http_ext: http::Extensions = original_extensions.into();

    // Convert back to Extensions
    let recovered_extensions: Extensions = http_ext.into();

    // Verify values are preserved
    assert_eq!(
        recovered_extensions.get::<String>(),
        Some("original_value".to_string())
    );
    assert_eq!(recovered_extensions.get::<i32>(), Some(123));
}

#[test]
fn test_conversion_from_plain_http_extensions() {
    // Create a plain http::Extensions with some values
    let mut http_ext = http::Extensions::new();
    http_ext.insert("http_value".to_string());
    http_ext.insert(456i32);

    // Convert to Extensions
    let extensions: Extensions = http_ext.into();

    // Verify values are accessible
    assert_eq!(extensions.get::<String>(), Some("http_value".to_string()));
    assert_eq!(extensions.get::<i32>(), Some(456));
}

#[test]
fn test_http_wrapped_extensions_can_be_mutated() {
    // Create a plain http::Extensions
    let mut http_ext = http::Extensions::new();
    http_ext.insert("initial_value".to_string());

    // Convert to Extensions (should be HttpWrapped variant)
    let mut extensions: Extensions = http_ext.into();

    // Should be able to insert into the wrapped http::Extensions
    extensions.insert(789i32);

    // Verify both old and new values are accessible
    assert_eq!(
        extensions.get::<String>(),
        Some("initial_value".to_string())
    );
    assert_eq!(extensions.get::<i32>(), Some(789));
}

#[test]
fn test_extensions_from_http_extensions() {
    // Create a http-wrapped Extensions
    let mut http_ext = http::Extensions::new();
    http_ext.insert("http_parent".to_string());
    let parent: Extensions = http_ext.into();

    // Clone creates a copy with parent values
    let mut child = parent.clone();
    child.insert(999i32);

    // Child has its own values plus cloned parent values
    assert_eq!(child.get::<String>(), Some("http_parent".to_string())); // Cloned from parent
    assert_eq!(child.get::<i32>(), Some(999));
    assert_eq!(parent.get::<String>(), Some("http_parent".to_string())); // Parent retains its value
    assert_eq!(parent.get::<i32>(), None); // Parent can't see child values
}

#[test]
fn test_remove_method() {
    let mut extensions = Extensions::default();

    // Insert some values
    extensions.insert(42i32);
    extensions.insert("test_string".to_string());
    extensions.insert(true);

    // Verify values are present
    assert_eq!(extensions.get::<i32>(), Some(42));
    assert_eq!(extensions.get::<String>(), Some("test_string".to_string()));
    assert_eq!(extensions.get::<bool>(), Some(true));

    // Remove the integer value
    let removed = extensions.remove::<i32>();
    assert_eq!(removed, Some(42));

    // Verify it's no longer accessible
    assert_eq!(extensions.get::<i32>(), None);

    // Other values should still be present
    assert_eq!(extensions.get::<String>(), Some("test_string".to_string()));
    assert_eq!(extensions.get::<bool>(), Some(true));

    // Removing a non-existent type should return None
    let not_found = extensions.remove::<f64>();
    assert_eq!(not_found, None);
}

#[test]
fn test_remove_independent_extensions() {
    let mut parent = Extensions::default();
    parent.insert("parent_value".to_string());
    parent.insert(100i32);

    let mut child = parent.clone();
    child.insert(42i32); // Override parent's value
    child.insert(true); // New type in child

    // Verify child has overridden and new values
    assert_eq!(child.get::<i32>(), Some(42)); // Child's overridden value
    assert_eq!(child.get::<String>(), Some("parent_value".to_string())); // Cloned from parent
    assert_eq!(child.get::<bool>(), Some(true)); // Child's value

    // Remove value from child
    let removed = child.remove::<i32>();
    assert_eq!(removed, Some(42)); // Child's own value is removed

    // Child no longer has the integer value
    assert_eq!(child.get::<i32>(), None); // No longer available

    // Remove child's boolean value
    let removed_bool = child.remove::<bool>();
    assert_eq!(removed_bool, Some(true));
    assert_eq!(child.get::<bool>(), None); // No longer available

    // Parent values should be unaffected
    assert_eq!(parent.get::<i32>(), Some(100));
    assert_eq!(parent.get::<String>(), Some("parent_value".to_string()));
    assert_eq!(parent.get::<bool>(), None); // Was never in parent
}

#[test]
fn test_remove_from_http_wrapped() {
    // Create a http-wrapped Extensions
    let mut http_ext = http::Extensions::new();
    http_ext.insert("initial_value".to_string());
    http_ext.insert(123i32);

    let mut extensions: Extensions = http_ext.into();

    // Verify values are present
    assert_eq!(
        extensions.get::<String>(),
        Some("initial_value".to_string())
    );
    assert_eq!(extensions.get::<i32>(), Some(123));

    // Remove a value from the HTTP-wrapped Extensions
    let removed = extensions.remove::<i32>();
    assert_eq!(removed, Some(123));

    // Verify it's no longer accessible
    assert_eq!(extensions.get::<i32>(), None);
    assert_eq!(
        extensions.get::<String>(),
        Some("initial_value".to_string())
    );
}
