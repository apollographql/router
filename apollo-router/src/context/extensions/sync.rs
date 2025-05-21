use std::sync::Arc;

/// You can use `Extensions` to pass data between plugins that is not serializable. Such data is not accessible from Rhai or co-processoers.
///
/// This can be accessed at any point in the request lifecycle and is useful for passing data between services.
/// Extensions are thread safe, and must be locked for mutation.
///
/// For example:
/// `context.extensions().lock().insert::<MyData>(data);`
#[derive(Default, Clone, Debug)]
pub struct ExtensionsMutex {
    extensions: Arc<parking_lot::Mutex<super::Extensions>>,
}

impl ExtensionsMutex {
    /// Locks the extensions for interaction.
    ///
    /// The lock will be dropped once the closure completes.
    pub fn with_lock<T, F: FnOnce(&mut super::Extensions) -> T>(&self, func: F) -> T {
        let mut locked = self.extensions.lock();
        func(&mut locked)
    }
}
