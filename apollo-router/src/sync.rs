//! Re-exports of synchronous `Mutex` and `RwLock` structures to simplify switching between
//! different implementations.

#[cfg(feature = "no_deadlocks")]
pub(crate) use no_deadlocks::Mutex;
#[cfg(feature = "no_deadlocks")]
pub(crate) use no_deadlocks::MutexGuard;
#[cfg(feature = "no_deadlocks")]
pub(crate) use no_deadlocks::RwLock;
#[cfg(not(feature = "no_deadlocks"))]
pub(crate) use parking_lot::Mutex;
#[cfg(not(feature = "no_deadlocks"))]
pub(crate) use parking_lot::MutexGuard;
#[cfg(not(feature = "no_deadlocks"))]
pub(crate) use parking_lot::RwLock;
