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
#[derive(Clone, Debug)]
pub struct SharedPath<T> {
    head: Option<Arc<Node<T>>>,
    len: usize,
}

#[derive(Debug)]
struct Node<T> {
    value: T,
    next: Option<Arc<Node<T>>>,
}

impl<T> Drop for SharedPath<T> {
    /// Iterative drop to avoid stack overflow on long spines.
    fn drop(&mut self) {
        let mut current = self.head.take();
        while let Some(arc) = current {
            // into_inner guarantees exactly one caller wins the value, unlike try_unwrap
            if let Some(mut node) = Arc::into_inner(arc) {
                current = node.next.take()
            } else {
                // someone else owns the tail; they are in charge of dropping it.
                break;
            }
        }
    }
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
    ///
    /// Allocates a Vec of node references (O(n)). Suitable for
    /// finalization boundaries; avoid calling per-entry in hot loops.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> {
        let mut nodes = Vec::with_capacity(self.len);
        let mut current = &self.head;
        while let Some(node) = current {
            nodes.push(node.as_ref());
            current = &node.next;
        }
        nodes.reverse();
        nodes.into_iter().map(|node| &node.value)
    }

    pub fn from_vec(v: Vec<T>) -> Self {
        let mut path = Self::new();
        for item in v {
            path = path.pushed(item);
        }
        path
    }
}

impl<T: Clone> SharedPath<T> {
    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }
}

// Using #[derive] bounds T: Default, so we implement manually to handle
// shared paths of non-Default elements.
impl<T> Default for SharedPath<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoDefault;

    #[test]
    fn default_works_without_t_default() {
        let path: SharedPath<NoDefault> = Default::default();
        assert!(path.is_empty());
    }

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
    fn drop_of_long_spine_does_not_overflow_stack() {
        let mut path = SharedPath::new();
        for i in 0..1_000_000u32 {
            path = path.pushed(i);
        }
        drop(path);
    }

    /// Two owners of one long spine dropping simultaneously. If the loser
    /// of the head-ownership race takes the derived recursive drop instead
    /// of handing off the iterative teardown, the dropping thread's small
    /// stack overflows and aborts the process. The race window is
    /// instruction-scale, so this spin-aligns the two drops and retries;
    /// each spine is long enough that a single recursive teardown
    /// overflows the 64KiB thread stack.
    #[test]
    fn concurrent_drop_of_shared_spine_does_not_overflow_stack() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;

        for _ in 0..512 {
            let mut path = SharedPath::new();
            for i in 0..20_000u32 {
                path = path.pushed(i);
            }
            let clone = path.clone();
            let ready = Arc::new(AtomicUsize::new(0));
            let spawn_dropper = |p: SharedPath<u32>, ready: Arc<AtomicUsize>| {
                std::thread::Builder::new()
                    .stack_size(64 * 1024)
                    .spawn(move || {
                        ready.fetch_add(1, Ordering::SeqCst);
                        while ready.load(Ordering::SeqCst) < 2 {
                            std::hint::spin_loop();
                        }
                        drop(p);
                    })
                    .expect("spawn dropper")
            };
            let t1 = spawn_dropper(path, Arc::clone(&ready));
            let t2 = spawn_dropper(clone, ready);
            t1.join().expect("first dropper");
            t2.join().expect("second dropper");
        }
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
