use super::*;
use crate::Extensions;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

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
fn test_hierarchical_extensions() {
    let mut root = Extensions::default();
    root.insert("upstream_value".to_string());
    root.insert(100i32);

    let mut child = root.extend();
    child.insert(42i32); // Attempt to override, but parent takes precedence
    child.insert(true); // Add a new value (new type)

    // Child accesses parent values first, then its own
    assert_eq!(child.get::<i32>(), Some(100)); // Parent's value takes precedence
    assert_eq!(child.get::<String>(), Some("upstream_value".to_string())); // Inherited from parent
    assert_eq!(child.get::<bool>(), Some(true)); // Child's own value (new type)

    // Parent only has its own values and is not affected by child
    assert_eq!(root.get::<i32>(), Some(100)); // Original parent value
    assert_eq!(root.get::<String>(), Some("upstream_value".to_string()));
    assert_eq!(root.get::<bool>(), None); // Not available in parent
}

#[test]
fn test_deep_hierarchy() {
    let mut root = Extensions::default();
    root.insert("root".to_string());

    let mut level1 = root.extend();
    level1.insert(1i32);

    let mut level2 = level1.extend();
    level2.insert(2i16);
    level2.insert(100i32); // Attempt to override level1's i32, but level1 wins

    let mut level3 = level2.extend();
    level3.insert(3i8);
    level3.insert("level3".to_string()); // Attempt to override root's string, but root wins

    // Level 3 can access all values from the hierarchy, with parents taking precedence
    assert_eq!(level3.get::<String>(), Some("root".to_string())); // Root's value wins
    assert_eq!(level3.get::<i32>(), Some(1)); // Level1's value wins over level2's attempt
    assert_eq!(level3.get::<i16>(), Some(2)); // Level2's value (no conflict)
    assert_eq!(level3.get::<i8>(), Some(3)); // Level3's own value

    // Earlier levels cannot access values from later levels
    assert_eq!(level2.get::<String>(), Some("root".to_string())); // Root's value
    assert_eq!(level2.get::<i32>(), Some(1)); // Level1's value wins over its own attempt
    assert_eq!(level2.get::<i16>(), Some(2)); // Its own value
    assert_eq!(level2.get::<i8>(), None); // Cannot see level3's value

    assert_eq!(level1.get::<String>(), Some("root".to_string())); // Root's value
    assert_eq!(level1.get::<i32>(), Some(1)); // Its own value
    assert_eq!(level1.get::<i16>(), None); // Cannot see level2's value
    assert_eq!(level1.get::<i8>(), None); // Cannot see level3's value

    assert_eq!(root.get::<String>(), Some("root".to_string())); // Its own value
    assert_eq!(root.get::<i32>(), None); // Cannot see descendants' values
    assert_eq!(root.get::<i16>(), None);
    assert_eq!(root.get::<i8>(), None);
}

#[test]
fn test_parent_precedence() {
    let mut parent = Extensions::default();
    parent.insert("parent_value".to_string());

    let mut child = parent.extend();
    child.insert("child_attempt".to_string()); // Attempt to override, but parent wins

    // Child sees parent value, not its own attempt
    assert_eq!(child.get::<String>(), Some("parent_value".to_string()));

    // Parent is unaffected
    assert_eq!(parent.get::<String>(), Some("parent_value".to_string()));
}

#[test]
fn test_mutable_values_with_sync_primitives() {
    use std::sync::{Arc, Mutex};

    let mut parent = Extensions::default();
    let counter = Arc::new(Mutex::new(0));
    parent.insert(counter.clone());

    let child = parent.extend();
    // Downstream can modify the shared value
    let counter_ref = child.get::<Arc<Mutex<i32>>>().unwrap();
    *counter_ref.lock().unwrap() += 5;

    // Both layers see the updated value
    assert_eq!(*parent.get::<Arc<Mutex<i32>>>().unwrap().lock().unwrap(), 5);
    assert_eq!(*child.get::<Arc<Mutex<i32>>>().unwrap().lock().unwrap(), 5);

    // Child can also add its own counter type, but parent wins due to precedence
    let mut mutable_child = child;
    let child_counter = Arc::new(Mutex::new(10));
    mutable_child.insert(child_counter);

    // Parent doesn't see child's additions
    assert_eq!(parent.get::<Arc<Mutex<i32>>>().is_some(), true); // Original counter
    // But both have the same type, so parent wins
    assert_eq!(*mutable_child.get::<Arc<Mutex<i32>>>().unwrap().lock().unwrap(), 5); // Parent's counter
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
    
    let mut child = parent.extend();
    child.insert(42i32);

    // Convert child to http::Extensions
    let http_ext: http::Extensions = child.into();

    // Only current layer values should be present
    assert_eq!(http_ext.get::<i32>(), Some(&42)); // Child value present
    assert_eq!(http_ext.get::<String>(), None);   // Parent value NOT present
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
    assert_eq!(recovered_extensions.get::<String>(), Some("original_value".to_string()));
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
    assert_eq!(extensions.get::<String>(), Some("initial_value".to_string()));
    assert_eq!(extensions.get::<i32>(), Some(789));
}

#[test]
fn test_hierarchical_extensions_with_http_wrapped() {
    // Create a http-wrapped Extensions
    let mut http_ext = http::Extensions::new();
    http_ext.insert("http_parent".to_string());
    let parent: Extensions = http_ext.into();

    // Extend it (child should be Native)
    let mut child = parent.extend();
    child.insert(999i32);

    // Verify hierarchy works with mixed types
    assert_eq!(child.get::<String>(), Some("http_parent".to_string()));
    assert_eq!(child.get::<i32>(), Some(999));
    assert_eq!(parent.get::<i32>(), None); // Parent can't see child values
}
