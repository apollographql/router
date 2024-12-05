//! This module contains various tools that help the ergonomics of this crate.

mod fallible_iterator;
pub(crate) mod logging;
pub(crate) mod serde_bridge;

pub(crate) use fallible_iterator::*;

/// If the `iter` yields a single element, return it. Else return `None`.
pub(crate) fn iter_into_single_item<T>(mut iter: impl Iterator<Item = T>) -> Option<T> {
    let item = iter.next()?;
    if iter.next().is_none() {
        Some(item)
    } else {
        None
    }
}
