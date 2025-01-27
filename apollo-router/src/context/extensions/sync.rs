use std::ops::Deref;
use std::ops::DerefMut;
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
    pub fn with_lock<'a, T, F: FnOnce(ExtensionsGuard<'a>) -> T>(&'a self, func: F) -> T {
        let locked = ExtensionsGuard::new(&self.extensions);
        func(locked)
    }
}

pub struct ExtensionsGuard<'a> {
    guard: parking_lot::MutexGuard<'a, super::Extensions>,
}
impl<'a> ExtensionsGuard<'a> {
    fn new(guard: &'a parking_lot::Mutex<super::Extensions>) -> Self {
        // IMPORTANT: Rust fields are constructed in the order that in which you write the fields in the initializer
        // The guard MUST be initialized first otherwise time waiting for a lock is included in this time.
        Self {
            guard: guard.lock(),
        }
    }
}

impl Deref for ExtensionsGuard<'_> {
    type Target = super::Extensions;

    fn deref(&self) -> &super::Extensions {
        &self.guard
    }
}

impl DerefMut for ExtensionsGuard<'_> {
    fn deref_mut(&mut self) -> &mut super::Extensions {
        &mut self.guard
    }
}
