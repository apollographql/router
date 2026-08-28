//! Persistent cons-list for paths carried through BULB search.
//!
//! `SharedPath<T>` gives O(1) `push`, `clone`, and `last` at the cost of
//! O(n) random access and iteration. This is the right trade-off: paths are
//! extended and forked thousands of times per probe but materialized only
//! once, when the winning plan is finalized.

use std::sync::Arc;

/// A shared, immutable, singly-linked path: O(1) clone (Arc bump) and
/// extend (`pushed`). Elements are stored newest-first; iteration yields
/// them oldest-first (root → leaf).
///
/// # Example
///
/// Sibling selections at `a.b` extend the same path without copying it:
/// `pushed` allocates one node pointing at the shared prefix, so recording
/// a selection at `a.b.c` and another at `a.b.d` shares the `a.b` spine.
///
/// ```
/// use apollo_federation::query_plan::incremental_planner::shared_path::SharedPath;
///
/// let prefix = SharedPath::new().pushed("a").pushed("b");
///
/// // O(1) per sibling: one new head node each, prefix untouched.
/// let c = prefix.pushed("c");
/// let d = prefix.pushed("d");
///
/// assert_eq!(c.to_vec(), vec!["a", "b", "c"]);
/// assert_eq!(d.to_vec(), vec!["a", "b", "d"]);
/// // The persistent prefix is unchanged and still shared.
/// assert_eq!(prefix.to_vec(), vec!["a", "b"]);
/// assert_eq!(c.parent().to_vec(), prefix.to_vec());
/// ```
#[derive(Debug)]
pub struct SharedPath<T> {
    head: Option<Arc<Node<T>>>,
    len: usize,
}

#[derive(Debug)]
struct Node<T> {
    value: T,
    next: Option<Arc<Node<T>>>,
}

impl<T> SharedPath<T> {
    pub fn new() -> Self {
        Self { head: None, len: 0 }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn last(&self) -> Option<&T> {
        self.head.as_ref().map(|n| &n.value)
    }

    /// Return a new path with `value` appended. O(1).
    pub fn pushed(&self, value: T) -> Self {
        Self {
            head: Some(Arc::new(Node {
                value,
                next: self.head.clone(),
            })),
            len: self.len + 1,
        }
    }

    /// The path without its last (newest) element. Returns just the tail in O(1).
    pub fn parent(&self) -> Self {
        Self {
            head: self.head.as_ref().and_then(|n| n.next.clone()),
            len: self.len.saturating_sub(1),
        }
    }

    /// Iterate from root to tip (oldest to newest).
    pub fn iter(&self) -> Iter<'_, T> {
        let mut nodes = Vec::with_capacity(self.len);
        let mut current = &self.head;
        while let Some(node) = current {
            nodes.push(node.as_ref());
            current = &node.next;
        }
        nodes.reverse();
        Iter { nodes, pos: 0 }
    }
}

impl<T: Clone> SharedPath<T> {
    /// Materialize into a Vec. O(n). Used at finalization boundaries.
    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }

    /// Build from a Vec.
    pub fn from_vec(v: Vec<T>) -> Self {
        let mut path = Self::new();
        for item in v {
            path = path.pushed(item);
        }
        path
    }
}

impl<T> Clone for SharedPath<T> {
    fn clone(&self) -> Self {
        Self {
            head: self.head.clone(),
            len: self.len,
        }
    }
}

impl<T> Default for SharedPath<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator yielding elements root-to-tip (oldest first).
pub struct Iter<'a, T> {
    nodes: Vec<&'a Node<T>>,
    pos: usize,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos < self.nodes.len() {
            let val = &self.nodes[self.pos].value;
            self.pos += 1;
            Some(val)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.nodes.len() - self.pos;
        (remaining, Some(remaining))
    }
}

impl<'a, T> ExactSizeIterator for Iter<'a, T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_removes_newest_element() {
        let path = SharedPath::from_vec(vec![1, 2, 3]);
        let parent = path.parent();
        assert_eq!(parent.len(), 2);
        assert_eq!(parent.to_vec(), vec![1, 2]);
        assert_eq!(parent.last(), Some(&2));
        // Persistent: the original path is untouched.
        assert_eq!(path.to_vec(), vec![1, 2, 3]);
        assert_eq!(path.len(), 3);
    }

    #[test]
    fn parent_of_single_element_is_empty() {
        let path = SharedPath::new().pushed("only");
        let parent = path.parent();
        assert!(parent.is_empty());
        assert_eq!(parent.len(), 0);
        assert_eq!(parent.last(), None);
    }

    #[test]
    fn parent_of_empty_is_empty() {
        let path: SharedPath<u8> = SharedPath::new();
        let parent = path.parent();
        assert!(parent.is_empty());
        assert_eq!(parent.len(), 0);
        assert_eq!(parent.to_vec(), Vec::<u8>::new());
    }
}
