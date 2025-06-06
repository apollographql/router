#[cfg(test)]
mod test {
    use super::*;
    use crate::Extensions;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Simple value type for basic operations
    #[derive(Debug, PartialEq, Clone)]
    struct TestValue {
        value: String,
    }

    // Expensive to clone type that should be wrapped in Arc
    #[derive(Debug, Clone)]
    struct ExpensiveType {
        data: Vec<u8>,
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
        let extensions = Extensions::default();

        // Insert and get simple values
        extensions.insert(42);
        extensions.insert("hello".to_string());

        assert_eq!(extensions.get::<i32>(), Some(42));
        assert_eq!(extensions.get::<String>(), Some("hello".to_string()));

        // Remove values
        extensions.remove::<i32>();
        assert!(extensions.get::<i32>().is_none());
        assert_eq!(extensions.get::<String>(), Some("hello".to_string()));
    }

    #[test]
    fn test_expensive_type_with_arc() {
        let extensions = Extensions::default();

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
        let extensions = Extensions::default();

        // Store different types
        extensions.insert(42);
        extensions.insert("hello".to_string());
        extensions.insert(ExpensiveType::new(100).clone());

        // Retrieve and verify each type
        assert_eq!(extensions.get::<i32>(), Some(42));
        assert_eq!(extensions.get::<String>(), Some("hello".to_string()));
        assert_eq!(extensions.get::<ExpensiveType>().unwrap().data.len(), 100);
    }

    #[test]
    fn test_concurrent_access() {
        use std::thread;
        let extensions = Extensions::default();

        // Create a shared counter using Arc
        let counter = Arc::new(AtomicUsize::new(0));
        extensions.insert(counter.clone());

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let extensions = extensions.clone();
                let counter = counter.clone();
                thread::spawn(move || {
                    for _ in 0..100 {
                        // Get the current counter
                        let current = extensions.get::<Arc<AtomicUsize>>().unwrap();
                        current.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // Verify the final count
        let final_counter = extensions.get::<Arc<AtomicUsize>>().unwrap();
        assert_eq!(final_counter.load(Ordering::SeqCst), 500);
    }

    #[test]
    fn test_overwrite_values() {
        let extensions = Extensions::default();

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
    fn test_remove_nonexistent() {
        let extensions = Extensions::default();

        // Removing non-existent values should not panic
        extensions.remove::<i32>();
        extensions.remove::<String>();
        extensions.remove::<ExpensiveType>();

        assert!(extensions.get::<i32>().is_none());
        assert!(extensions.get::<String>().is_none());
        assert!(extensions.get::<ExpensiveType>().is_none());
    }
}
