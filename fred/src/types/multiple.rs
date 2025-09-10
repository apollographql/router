use crate::types::{Key, Value};
use std::{collections::VecDeque, iter::FromIterator};

/// Convenience struct for commands that take 1 or more keys.
///
/// **Note: this can also be used to represent an empty array of keys by passing `None` to any function that takes
/// `Into<MultipleKeys>`.** This is mostly useful for `EVAL` and `EVALSHA`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultipleKeys {
  pub(crate) keys: Vec<Key>,
}

impl MultipleKeys {
  pub fn new() -> MultipleKeys {
    MultipleKeys { keys: Vec::new() }
  }

  pub fn inner(self) -> Vec<Key> {
    self.keys
  }

  pub fn into_values(self) -> Vec<Value> {
    self.keys.into_iter().map(|k| k.into()).collect()
  }

  pub fn len(&self) -> usize {
    self.keys.len()
  }
}

impl From<Option<Key>> for MultipleKeys {
  fn from(key: Option<Key>) -> Self {
    let keys = if let Some(key) = key { vec![key] } else { vec![] };
    MultipleKeys { keys }
  }
}

impl<T> From<T> for MultipleKeys
where
  T: Into<Key>,
{
  fn from(d: T) -> Self {
    MultipleKeys { keys: vec![d.into()] }
  }
}

impl<T> FromIterator<T> for MultipleKeys
where
  T: Into<Key>,
{
  fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
    MultipleKeys {
      keys: iter.into_iter().map(|k| k.into()).collect(),
    }
  }
}

impl<'a, K, const N: usize> From<&'a [K; N]> for MultipleKeys
where
  K: Into<Key> + Clone,
{
  fn from(value: &'a [K; N]) -> Self {
    MultipleKeys {
      keys: value.iter().map(|k| k.clone().into()).collect(),
    }
  }
}

impl<T> From<Vec<T>> for MultipleKeys
where
  T: Into<Key>,
{
  fn from(d: Vec<T>) -> Self {
    MultipleKeys {
      keys: d.into_iter().map(|k| k.into()).collect(),
    }
  }
}

impl<T> From<VecDeque<T>> for MultipleKeys
where
  T: Into<Key>,
{
  fn from(d: VecDeque<T>) -> Self {
    MultipleKeys {
      keys: d.into_iter().map(|k| k.into()).collect(),
    }
  }
}

impl From<()> for MultipleKeys {
  fn from(_: ()) -> Self {
    MultipleKeys { keys: Vec::new() }
  }
}

/// Convenience interface for commands that take 1 or more strings.
pub type MultipleStrings = MultipleKeys;

/// Convenience interface for commands that take 1 or more values.
pub type MultipleValues = Value;

/// A convenience struct for functions that take one or more hash slot values.
pub struct MultipleHashSlots {
  inner: Vec<u16>,
}

impl MultipleHashSlots {
  pub fn inner(self) -> Vec<u16> {
    self.inner
  }

  pub fn len(&self) -> usize {
    self.inner.len()
  }
}

impl From<u16> for MultipleHashSlots {
  fn from(d: u16) -> Self {
    MultipleHashSlots { inner: vec![d] }
  }
}

impl From<Vec<u16>> for MultipleHashSlots {
  fn from(d: Vec<u16>) -> Self {
    MultipleHashSlots { inner: d }
  }
}

impl<'a> From<&'a [u16]> for MultipleHashSlots {
  fn from(d: &'a [u16]) -> Self {
    MultipleHashSlots { inner: d.to_vec() }
  }
}

impl From<VecDeque<u16>> for MultipleHashSlots {
  fn from(d: VecDeque<u16>) -> Self {
    MultipleHashSlots {
      inner: d.into_iter().collect(),
    }
  }
}

impl FromIterator<u16> for MultipleHashSlots {
  fn from_iter<I: IntoIterator<Item = u16>>(iter: I) -> Self {
    MultipleHashSlots {
      inner: iter.into_iter().collect(),
    }
  }
}
