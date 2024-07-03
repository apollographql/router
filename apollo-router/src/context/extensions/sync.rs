use std::ops::Deref;
use std::ops::DerefMut;
use std::sync::Arc;
#[cfg(debug_assertions)]
use std::time::Duration;
#[cfg(debug_assertions)]
use std::time::Instant;

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
    /// Locks the extensions for mutation.
    ///
    /// It is CRITICAL to avoid holding on to the mutex guard for too long, particularly across async calls.
    /// Doing so may cause performance degradation or even deadlocks.
    ///
    /// DEPRECATED: prefer with_lock()
    ///
    /// See related clippy lint for examples: <https://rust-lang.github.io/rust-clippy/master/index.html#/await_holding_lock>
    #[deprecated]
    pub fn lock(&self) -> ExtensionsGuard {
        ExtensionsGuard::new(&self.extensions)
    }

    /// Locks the extensions for interaction.
    ///
    /// The lock will be dropped once the closure completes.
    pub fn with_lock<'a, T, F: FnOnce(ExtensionsGuard<'a>) -> T>(&'a self, func: F) -> T {
        let locked = ExtensionsGuard::new(&self.extensions);
        func(locked)
    }
}

pub struct ExtensionsGuard<'a> {
    #[cfg(debug_assertions)]
    start: Instant,
    guard: parking_lot::MutexGuard<'a, super::Extensions>,
}
impl<'a> ExtensionsGuard<'a> {
    fn new(guard: &'a parking_lot::Mutex<super::Extensions>) -> Self {
        // IMPORTANT: Rust fields are constructed in the order that in which you write the fields in the initializer
        // The guard MUST be initialized first otherwise time waiting for a lock is included in this time.
        Self {
            guard: guard.lock(),
            #[cfg(debug_assertions)]
            start: Instant::now(),
        }
    }
}

impl<'a> Deref for ExtensionsGuard<'a> {
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

#[cfg(debug_assertions)]
impl Drop for ExtensionsGuard<'_> {
    fn drop(&mut self) {
        // In debug builds we check that extensions is never held for too long.
        // We  only check if the current runtime is multi-threaded, because a bunch of unit tests fail the assertion and these need to be investigated separately.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                let elapsed = self.start.elapsed();
                if elapsed > Duration::from_millis(10) {
                    panic!("ExtensionsGuard held for {}ms. This is probably a bug that will stall the Router and cause performance problems. Run with `RUST_BACKTRACE=1` environment variable to display a backtrace", elapsed.as_millis());
                }
            }
        }
    }
}
