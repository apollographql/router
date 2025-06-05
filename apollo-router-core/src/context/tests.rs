#[cfg(test)]
mod test {
    use super::*;
    use crate::Context;
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
        let context = Context::new();
        
        // Insert and get simple values
        context.insert(42);
        context.insert("hello".to_string());
        
        assert_eq!(context.get::<i32>(), Some(42));
        assert_eq!(context.get::<String>(), Some("hello".to_string()));
        
        // Remove values
        context.remove::<i32>();
        assert!(context.get::<i32>().is_none());
        assert_eq!(context.get::<String>(), Some("hello".to_string()));
    }

    #[test]
    fn test_expensive_type_with_arc() {
        let context = Context::new();
        
        // Create an expensive type and wrap it in Arc
        let expensive = Arc::new(ExpensiveType::new(1000));
        context.insert(expensive.clone());
        
        // Retrieving is cheap since we're just cloning the Arc
        let retrieved = context.get::<Arc<ExpensiveType>>().unwrap();
        assert_eq!(retrieved.data.len(), 1000);
        
        // Both Arcs point to the same data
        assert!(Arc::ptr_eq(&expensive, &retrieved));
    }

    #[test]
    fn test_multiple_types() {
        let context = Context::new();
        
        // Store different types
        context.insert(42);
        context.insert("hello".to_string());
        context.insert(ExpensiveType::new(100).clone());
        
        // Retrieve and verify each type
        assert_eq!(context.get::<i32>(), Some(42));
        assert_eq!(context.get::<String>(), Some("hello".to_string()));
        assert_eq!(context.get::<ExpensiveType>().unwrap().data.len(), 100);
    }

    #[test]
    fn test_concurrent_access() {
        use std::thread;
        let context = Context::new();
        
        // Create a shared counter using Arc
        let counter = Arc::new(AtomicUsize::new(0));
        context.insert(counter.clone());
        
        let handles: Vec<_> = (0..5)
            .map(|_| {
                let context = context.clone();
                let counter = counter.clone();
                thread::spawn(move || {
                    for _ in 0..100 {
                        // Get the current counter
                        let current = context.get::<Arc<AtomicUsize>>().unwrap();
                        current.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
            
        for handle in handles {
            handle.join().unwrap();
        }
        
        // Verify the final count
        let final_counter = context.get::<Arc<AtomicUsize>>().unwrap();
        assert_eq!(final_counter.load(Ordering::SeqCst), 500);
    }

    #[test]
    fn test_overwrite_values() {
        let context = Context::new();
        
        // Insert initial values
        context.insert(42);
        context.insert("hello".to_string());
        
        // Overwrite values
        context.insert(43);
        context.insert("world".to_string());
        
        // Verify new values
        assert_eq!(context.get::<i32>(), Some(43));
        assert_eq!(context.get::<String>(), Some("world".to_string()));
    }

    #[test]
    fn test_remove_nonexistent() {
        let context = Context::new();
        
        // Removing non-existent values should not panic
        context.remove::<i32>();
        context.remove::<String>();
        context.remove::<ExpensiveType>();
        
        assert!(context.get::<i32>().is_none());
        assert!(context.get::<String>().is_none());
        assert!(context.get::<ExpensiveType>().is_none());
    }
}
