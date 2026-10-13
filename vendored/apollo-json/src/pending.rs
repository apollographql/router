//! Pending trees held in one buffer.
//!
//! A [`NewValue`] tree gives every pending object and array a `Vec` of its
//! own, so a caller assembling a document of many small containers pays one
//! heap allocation per container before anything reaches an arena. A
//! [`PendingTree`] holds every container's members in a single buffer
//! instead, each container threading its members through the buffer as a
//! linked list, so assembling the document costs the buffer's growth however
//! many containers it has. Containers stay open until the tree is sealed: a
//! caller can come back to an object it already wrote and add to it.

use std::borrow::Cow;

use crate::builder::{NewValue, ValueBuilder};
use crate::document::Value;

/// A slot in a [`PendingTree`]: a root, an object member or an array element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingId(pub(crate) u32);

/// No slot: the end of a member list.
pub(crate) const NONE: u32 = u32::MAX;

/// The `next` of a root slot, in no container yet.
const DETACHED: u32 = u32::MAX - 1;

pub(crate) struct PendingSlot<'a> {
    /// The member's key; empty for roots and array elements.
    pub(crate) key: Cow<'a, str>,
    pub(crate) value: Pending<'a>,
    /// The next member of the same container, [`NONE`] for the last one,
    /// or [`DETACHED`] for a root.
    pub(crate) next: u32,
}

pub(crate) enum Pending<'a> {
    /// Any value other than a container of this tree, written as a
    /// [`NewValue`] would be.
    Value(NewValue<'a>),
    Object {
        first: u32,
        last: u32,
    },
    Array {
        first: u32,
        last: u32,
    },
}

/// Pending objects and arrays assembled in one buffer and written into an
/// arena in one pass by [`PendingTree::seal`].
///
/// Slots are addressed by [`PendingId`]. Overwriting a container's slot
/// abandons its members: they stay in the buffer, unreachable, until the tree
/// is dropped. Object keys are unique by construction, since a member is only
/// added through [`PendingTree::member_slot`] when its key is absent.
///
/// # Example
/// ```
/// use apollo_json::PendingTree;
///
/// let mut tree = PendingTree::new();
/// let root = tree.object();
/// let people = tree.member_slot(root, "people");
/// tree.set_array(people);
/// let bob = tree.push(people);
/// tree.set_object(bob);
/// let name = tree.member_slot(bob, "name");
/// tree.set(name, "bob");
/// let age = tree.member_slot(bob, "age");
/// tree.set(age, 19i64);
/// assert_eq!(
///     tree.seal(root).to_string(),
///     r#"{"people":[{"name":"bob","age":19}]}"#
/// );
/// ```
pub struct PendingTree<'a> {
    /// Slots in segments that never move: segment `k` holds
    /// `FIRST_SEGMENT << k` slots, allocated when the one before it is full.
    /// A growing `Vec` would copy every slot on each reallocation, and its
    /// reallocations together allocate about twice its final size.
    segments: [Vec<PendingSlot<'a>>; SEGMENTS],
    len: u32,
}

/// Slots in the first segment.
const FIRST_SEGMENT: usize = 32;

/// Enough segments for every id below [`DETACHED`].
const SEGMENTS: usize = 27;

/// The segment holding slot `id`, and the slot's index in it.
fn locate(id: u32) -> (usize, usize) {
    let id = id as usize;
    let segment = (usize::BITS - 1 - (id / FIRST_SEGMENT + 1).leading_zeros()) as usize;
    (segment, id - FIRST_SEGMENT * ((1 << segment) - 1))
}

impl Default for PendingTree<'_> {
    fn default() -> Self {
        PendingTree {
            segments: [const { Vec::new() }; SEGMENTS],
            len: 0,
        }
    }
}

impl<'a> PendingTree<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn slot(&self, id: PendingId) -> &PendingSlot<'a> {
        let (segment, index) = locate(id.0);
        &self.segments[segment][index]
    }

    pub(crate) fn slot_mut(&mut self, id: PendingId) -> &mut PendingSlot<'a> {
        let (segment, index) = locate(id.0);
        &mut self.segments[segment][index]
    }

    fn add(&mut self, key: Cow<'a, str>, value: Pending<'a>) -> PendingId {
        let id = self.len;
        let (segment, index) = locate(id);
        assert!(
            segment < SEGMENTS,
            "a pending tree holds under 2^32 - 32 slots"
        );
        let slots = &mut self.segments[segment];
        if index == 0 {
            *slots = Vec::with_capacity(FIRST_SEGMENT << segment);
        }
        slots.push(PendingSlot {
            key,
            value,
            next: DETACHED,
        });
        self.len += 1;
        PendingId(id)
    }

    /// A new root slot holding `null`, to be written and later sealed,
    /// appended to an array with [`PendingTree::append`], or merged into
    /// another object with [`PendingTree::merge_members`].
    pub fn root(&mut self) -> PendingId {
        self.add(Cow::Borrowed(""), Pending::Value(NewValue::Null))
    }

    /// A new root slot holding an empty object.
    pub fn object(&mut self) -> PendingId {
        let id = self.root();
        self.set_object(id);
        id
    }

    /// A new root slot holding an empty array.
    pub fn array(&mut self) -> PendingId {
        let id = self.root();
        self.set_array(id);
        id
    }

    /// Writes `value` at `id`, abandoning anything the slot held.
    pub fn set(&mut self, id: PendingId, value: impl Into<NewValue<'a>>) {
        self.slot_mut(id).value = Pending::Value(value.into());
    }

    /// Writes an empty object at `id`.
    pub fn set_object(&mut self, id: PendingId) {
        self.slot_mut(id).value = Pending::Object {
            first: NONE,
            last: NONE,
        };
    }

    /// Writes an empty array at `id`.
    pub fn set_array(&mut self, id: PendingId) {
        self.slot_mut(id).value = Pending::Array {
            first: NONE,
            last: NONE,
        };
    }

    /// The value at `id` when it is not a container of this tree.
    pub fn get(&self, id: PendingId) -> Option<&NewValue<'a>> {
        match &self.slot(id).value {
            Pending::Value(value) => Some(value),
            _ => None,
        }
    }

    /// Whether `id` holds a JSON `null`, written as [`NewValue::Null`] or
    /// adopted as a handle to one.
    pub fn is_null(&self, id: PendingId) -> bool {
        match self.get(id) {
            Some(NewValue::Null) => true,
            Some(NewValue::Node(handle)) => handle.is_null(),
            _ => false,
        }
    }

    pub fn is_object(&self, id: PendingId) -> bool {
        matches!(self.slot(id).value, Pending::Object { .. })
    }

    pub fn is_array(&self, id: PendingId) -> bool {
        matches!(self.slot(id).value, Pending::Array { .. })
    }

    fn first_last(&self, id: PendingId) -> Option<(u32, u32)> {
        match self.slot(id).value {
            Pending::Object { first, last } | Pending::Array { first, last } => Some((first, last)),
            Pending::Value(_) => None,
        }
    }

    /// The first member or element of the container at `id`, or `None` when
    /// it is empty or not a container.
    pub fn first(&self, id: PendingId) -> Option<PendingId> {
        let (first, _) = self.first_last(id)?;
        (first != NONE).then_some(PendingId(first))
    }

    /// The member or element after `id` in its container.
    pub fn next(&self, id: PendingId) -> Option<PendingId> {
        let next = self.slot(id).next;
        (next < DETACHED).then_some(PendingId(next))
    }

    /// The members or elements of the container at `id`, in order.
    pub fn children(&self, id: PendingId) -> impl Iterator<Item = PendingId> + '_ {
        std::iter::successors(self.first(id), |&child| self.next(child))
    }

    /// The key of the object member at `id`.
    pub fn key(&self, id: PendingId) -> &str {
        &self.slot(id).key
    }

    /// The member at `key` of the object at `object`.
    pub fn member(&self, object: PendingId, key: &str) -> Option<PendingId> {
        self.children(object)
            .find(|&member| self.key(member) == key)
    }

    /// Appends `child` to the container at `container`.
    fn link(&mut self, container: PendingId, child: u32) {
        self.slot_mut(PendingId(child)).next = NONE;
        let (Pending::Object { first, last } | Pending::Array { first, last }) =
            &mut self.slot_mut(container).value
        else {
            panic!("not a pending container");
        };
        let previous = std::mem::replace(last, child);
        if previous == NONE {
            *first = child;
        } else {
            self.slot_mut(PendingId(previous)).next = child;
        }
    }

    /// The member at `key` of the object at `object`, appended holding `null`
    /// when the key is new. A key already present keeps its position.
    ///
    /// # Panics
    /// When `object` does not hold an object.
    pub fn member_slot(&mut self, object: PendingId, key: impl Into<Cow<'a, str>>) -> PendingId {
        let key = key.into();
        assert!(self.is_object(object), "not a pending object");
        if let Some(member) = self.member(object, &key) {
            return member;
        }
        let member = self.add(key, Pending::Value(NewValue::Null));
        self.link(object, member.0);
        member
    }

    /// A new element holding `null` at the end of the array at `array`.
    ///
    /// # Panics
    /// When `array` does not hold an array.
    pub fn push(&mut self, array: PendingId) -> PendingId {
        let element = self.root();
        self.append(array, element);
        element
    }

    /// Appends the root slot `root` to the array at `array`, for a value
    /// written before the caller knew whether to keep it. `root` must not
    /// contain `array`; that would make a cycle, which is not detected.
    ///
    /// # Panics
    /// When `array` does not hold an array, or `root` is not a root slot.
    pub fn append(&mut self, array: PendingId, root: PendingId) {
        assert!(self.is_array(array), "not a pending array");
        assert!(
            self.slot_mut(root).next == DETACHED && root != array,
            "not a root slot"
        );
        self.link(array, root.0);
    }

    /// Moves the members of the object at `from` to the object at `into`. A
    /// key `into` already holds stays where it is, and `merge` combines the
    /// two: it is called with the existing member and the incoming one, and
    /// the incoming one is dropped afterwards. `from` is left empty.
    ///
    /// # Panics
    /// When either slot does not hold an object.
    pub fn merge_members(
        &mut self,
        into: PendingId,
        from: PendingId,
        mut merge: impl FnMut(&mut Self, PendingId, PendingId),
    ) {
        assert!(self.is_object(into), "not a pending object");
        let mut next = self.first(from);
        self.set_object(from);
        while let Some(incoming) = next {
            next = self.next(incoming);
            match self.member(into, self.key(incoming)) {
                Some(existing) => merge(self, existing, incoming),
                None => self.link(into, incoming.0),
            }
        }
    }

    /// Writes the subtree at `id` into an arena of its own, leaving `null` in
    /// its place.
    pub fn take(&mut self, id: PendingId) -> Value {
        let mut builder = ValueBuilder::new();
        builder.set_pending(self, id);
        builder.seal()
    }

    /// Writes the tree under `root` into one arena. Non-finite floats become
    /// `null`, as in the constructors taking plain Rust data.
    pub fn seal(mut self, root: PendingId) -> Value {
        self.take(root)
    }
}

impl std::fmt::Debug for PendingTree<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingTree")
            .field("slots", &self.len)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopened_containers_keep_member_order_and_overwrites_keep_position() {
        let mut tree = PendingTree::new();
        let root = tree.object();
        let a = tree.member_slot(root, "a");
        tree.set_object(a);
        let x = tree.member_slot(a, "x");
        tree.set(x, 1i64);
        let list = tree.member_slot(root, "list");
        tree.set_array(list);
        for i in 0..3i64 {
            let element = tree.push(list);
            tree.set(element, i);
        }
        // Back into `a` after `list` was written, as a second fragment would.
        let y = tree.member_slot(a, "y");
        tree.set(y, "two");
        assert_eq!(tree.member_slot(a, "x"), x);
        tree.set(x, 3i64);
        let gone = tree.member_slot(root, "gone");
        tree.set_object(gone);
        let inner = tree.member_slot(gone, "inner");
        tree.set(inner, true);
        tree.set(gone, ());
        assert!(tree.is_null(gone));
        assert_eq!(
            tree.seal(root).to_string(),
            r#"{"a":{"x":3,"y":"two"},"list":[0,1,2],"gone":null}"#
        );
    }

    #[test]
    fn roots_are_appended_once_and_discarded_roots_are_never_written() {
        let mut tree = PendingTree::new();
        let list = tree.array();
        for i in 0..4i64 {
            let candidate = tree.root();
            tree.set_object(candidate);
            let member = tree.member_slot(candidate, "i");
            tree.set(member, i);
            if i % 2 == 0 {
                tree.append(list, candidate);
            }
        }
        let kept = tree.first(list).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tree.append(list, kept);
        }));
        assert!(result.is_err());
        assert_eq!(tree.seal(list).to_string(), r#"[{"i":0},{"i":2}]"#);
    }

    #[test]
    fn slots_are_located_across_segment_boundaries() {
        assert_eq!(locate(0), (0, 0));
        assert_eq!(locate(31), (0, 31));
        assert_eq!(locate(32), (1, 0));
        assert_eq!(locate(95), (1, 63));
        assert_eq!(locate(96), (2, 0));
        let last = (FIRST_SEGMENT * ((1 << SEGMENTS) - 1) - 1) as u32;
        assert_eq!(
            locate(last),
            (SEGMENTS - 1, (FIRST_SEGMENT << (SEGMENTS - 1)) - 1)
        );
        assert!(last < DETACHED);

        let mut tree = PendingTree::new();
        let list = tree.array();
        for i in 0..1000i64 {
            let element = tree.push(list);
            tree.set(element, i);
        }
        let expected = format!(
            "[{}]",
            (0..1000)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(tree.seal(list).to_string(), expected);
    }

    #[test]
    fn merge_members_moves_new_keys_and_merges_present_ones() {
        let mut tree = PendingTree::new();
        let into = tree.object();
        let a = tree.member_slot(into, "a");
        tree.set(a, 1i64);
        let from = tree.object();
        let b = tree.member_slot(from, "b");
        tree.set(b, 2i64);
        let a2 = tree.member_slot(from, "a");
        tree.set(a2, 10i64);
        let mut merged = Vec::new();
        tree.merge_members(into, from, |tree, existing, incoming| {
            merged.push((tree.key(existing).to_owned(), tree.key(incoming).to_owned()));
            tree.set(existing, "merged");
        });
        assert_eq!(merged, [("a".to_owned(), "a".to_owned())]);
        assert!(tree.first(from).is_none());
        assert_eq!(tree.seal(into).to_string(), r#"{"a":"merged","b":2}"#);
    }

    #[test]
    fn adopted_handles_and_take_leave_null_behind() {
        let shared = Value::parse(br#"{"k":[1,{"z":null}]}"#.to_vec()).unwrap();
        let mut tree = PendingTree::new();
        let root = tree.array();
        let first = tree.push(root);
        tree.set(first, shared.clone());
        let second = tree.push(root);
        tree.set_object(second);
        let n = tree.member_slot(second, "n");
        tree.set(n, f64::NAN);
        assert_eq!(tree.take(second).to_string(), r#"{"n":null}"#);
        assert!(tree.is_null(second));
        assert_eq!(
            tree.seal(root).to_string(),
            r#"[{"k":[1,{"z":null}]},null]"#
        );
    }
}
