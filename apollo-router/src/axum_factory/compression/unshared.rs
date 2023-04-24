// All code from this module is extracted from https://github.com/Nemo157/async-compression and is under MIT or Apache-2 licence
// it will be removed when we find a long lasting solution to https://github.com/Nemo157/async-compression/issues/154
#![allow(dead_code)] // unused without any features

use core::fmt::Debug;
use core::fmt::{self};

/// Wraps a type and only allows unique borrowing, the main usecase is to wrap a `!Sync` type and
/// implement `Sync` for it as this type blocks having multiple shared references to the inner
/// value.
///
/// # Safety
///
/// We must be careful when accessing `inner`, there must be no way to create a shared reference to
/// it from a shared reference to an `Unshared`, as that would allow creating shared references on
/// multiple threads.
///
/// As an example deriving or implementing `Clone` is impossible, two threads could attempt to
/// clone a shared `Unshared<T>` reference which would result in accessing the same inner value
/// concurrently.
pub(crate) struct Unshared<T> {
    inner: T,
}

impl<T> Unshared<T> {
    pub(crate) fn new(inner: T) -> Self {
        Unshared { inner }
    }

    pub(crate) fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

/// Safety: See comments on main docs for `Unshared`
unsafe impl<T> Sync for Unshared<T> {}

impl<T> Debug for Unshared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(core::any::type_name::<T>()).finish()
    }
}
