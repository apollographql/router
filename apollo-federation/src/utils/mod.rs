//! This module contains various tools that help the ergonomics of this crate.

mod fallible_iterator;
pub(crate) mod human_readable;
pub(crate) mod logging;
pub(crate) mod multi_index_map;
pub(crate) mod normalize_schema;
pub(crate) mod serde_bridge;

// Re-exports
pub(crate) use fallible_iterator::*;
pub(crate) use multi_index_map::MultiIndexMap;

/// If the `iter` yields a single element, return it. Else return `None`.
pub(crate) fn iter_into_single_item<T>(mut iter: impl Iterator<Item = T>) -> Option<T> {
    let item = iter.next()?;
    if iter.next().is_none() {
        Some(item)
    } else {
        None
    }
}
