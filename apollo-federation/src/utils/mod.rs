//! This module contains various tools that help the ergonomics of this crate.

mod fallible_iterator;
pub(crate) mod human_readable;
pub(crate) mod logging;
pub(crate) mod multi_index_map;
pub mod normalize_schema;
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

/// An alternative to Itertools' `max_by_key` which breaks ties by returning the first element with
/// the maximum key, rather than the last.
pub(crate) fn first_max_by_key<T, O: Ord>(
    iter: impl Iterator<Item = T>,
    f: impl Fn(&T) -> O,
) -> Option<T> {
    let mut iter = iter.peekable();
    let first = iter.next()?;
    let mut max_item = first;
    let mut max_key = f(&max_item);

    for item in iter {
        let key = f(&item);
        if key > max_key {
            max_key = key;
            max_item = item;
        }
    }

    Some(max_item)
}
