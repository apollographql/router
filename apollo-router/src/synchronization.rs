//! Re-exports of synchronous `Mutex` and `RwLock` structures to simplify switching between
//! different implementations.

#[cfg(not(feature = "no_deadlocks"))]
pub(crate) use parking_lot::{Mutex, MutexGuard, RwLock};

#[cfg(feature = "no_deadlocks")]
pub(crate) use no_deadlocks::{Mutex, MutexGuard, RwLock};
