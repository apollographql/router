use std::{
  cell::{Cell, Ref, RefCell, RefMut},
  fmt,
  mem,
  sync::atomic::Ordering,
};

/// A !Send flavor of `ArcSwap` with an interface similar to std::sync::atomic types.
pub struct RefSwap<T> {
  inner: RefCell<T>,
}

impl<T> RefSwap<T> {
  pub fn new(val: T) -> Self {
    RefSwap {
      inner: RefCell::new(val),
    }
  }

  pub fn swap(&self, other: T) -> T {
    mem::replace(&mut self.inner.borrow_mut(), other)
  }

  pub fn store(&self, other: T) {
    self.swap(other);
  }

  pub fn load(&self) -> Ref<'_, T> {
    self.inner.borrow()
  }
}

#[cfg(feature = "dynamic-pool")]
/// A !Send flavor of `ArcSwapOption` with an interface similar to std::sync::atomic types.
pub type RefSwapOption<T> = RefSwap<Option<T>>;

/// A !Send flavor of `AtomicUsize`, with the same interface.
#[derive(Debug)]
pub struct AtomicUsize {
  inner: Cell<usize>,
}

impl AtomicUsize {
  pub fn new(val: usize) -> Self {
    AtomicUsize { inner: Cell::new(val) }
  }

  pub fn fetch_add(&self, val: usize, _: Ordering) -> usize {
    let tmp = self.inner.get().saturating_add(val);
    self.inner.replace(tmp);
    tmp
  }

  pub fn fetch_sub(&self, val: usize, _: Ordering) -> usize {
    let tmp = self.inner.get().saturating_sub(val);
    self.inner.replace(tmp);
    tmp
  }

  pub fn load(&self, _: Ordering) -> usize {
    self.inner.get()
  }

  pub fn swap(&self, val: usize, _: Ordering) -> usize {
    self.inner.replace(val)
  }
}

/// A !Send flavor of `AtomicBool`, with the same interface.
#[derive(Debug)]
pub struct AtomicBool {
  inner: Cell<bool>,
}

impl AtomicBool {
  pub fn new(val: bool) -> Self {
    AtomicBool { inner: Cell::new(val) }
  }

  pub fn load(&self, _: Ordering) -> bool {
    self.inner.get()
  }

  pub fn swap(&self, val: bool, _: Ordering) -> bool {
    self.inner.replace(val)
  }
}

pub type MutexGuard<'a, T> = RefMut<'a, T>;

pub struct Mutex<T> {
  inner: RefCell<T>,
}

impl<T: fmt::Debug> fmt::Debug for Mutex<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{:?}", self.inner)
  }
}

impl<T> Mutex<T> {
  pub fn new(val: T) -> Self {
    Mutex {
      inner: RefCell::new(val),
    }
  }

  pub fn lock(&self) -> MutexGuard<T> {
    self.inner.borrow_mut()
  }
}

pub type RwLockReadGuard<'a, T> = Ref<'a, T>;
pub type RwLockWriteGuard<'a, T> = RefMut<'a, T>;

pub struct RwLock<T> {
  inner: RefCell<T>,
}

impl<T: fmt::Debug> fmt::Debug for RwLock<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{:?}", self.inner)
  }
}

impl<T> RwLock<T> {
  pub fn new(val: T) -> Self {
    RwLock {
      inner: RefCell::new(val),
    }
  }

  pub fn read(&self) -> RwLockReadGuard<T> {
    self.inner.borrow()
  }

  pub fn write(&self) -> RwLockWriteGuard<T> {
    self.inner.borrow_mut()
  }
}
