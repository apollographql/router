//! In-arena node representation.
//!
//! Nodes are plain `Copy` data: byte spans into the arena-owned input,
//! `(chunk, start, len)` references into the arena's slab storage (container
//! children, object entries, owned text), and indices into the foreign
//! reference table. Nothing in a node owns heap memory, so dropping an arena
//! frees whole chunks with no per-node destructor traversal.

use crate::arena::ForeignId;
use crate::slab::SlabRef;

/// Index of a node within its arena.
pub(crate) type NodeId = u32;

/// Byte range into the arena-owned input.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Span {
    pub(crate) start: u32,
    pub(crate) len: u32,
}

impl Span {
    pub(crate) fn range(&self) -> std::ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }

    pub(crate) fn slice<'a>(&self, input: &'a [u8]) -> &'a [u8] {
        &input[self.range()]
    }
}

/// A container child slot: either a node in the same arena or an entry in the
/// arena's foreign-reference table (cross-document sharing).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Child(u32);

const FOREIGN_BIT: u32 = 1 << 31;

/// Exclusive upper bound on node ids and foreign-table indices: a `Child`
/// packs the index and the local/foreign discriminant into one `u32`, so an
/// index reaching the discriminant bit would alias the other index space.
/// The arena enforces this bound when it hands out indices.
pub(crate) const CHILD_INDEX_LIMIT: u32 = FOREIGN_BIT;

impl Child {
    pub(crate) fn local(id: NodeId) -> Self {
        debug_assert!(id & FOREIGN_BIT == 0, "node index overflow");
        Child(id)
    }

    pub(crate) fn foreign(id: ForeignId) -> Self {
        debug_assert!(id & FOREIGN_BIT == 0, "foreign index overflow");
        Child(id | FOREIGN_BIT)
    }

    pub(crate) fn as_local(self) -> Option<NodeId> {
        (self.0 & FOREIGN_BIT == 0).then_some(self.0)
    }

    pub(crate) fn as_foreign(self) -> Option<ForeignId> {
        (self.0 & FOREIGN_BIT != 0).then_some(self.0 & !FOREIGN_BIT)
    }
}

/// An object member key. Parsed keys keep their original (possibly escaped)
/// span; keys introduced by mutation or conversion live unescaped in the
/// arena's owned-text buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) enum KeyRef {
    Span { span: Span, escaped: bool },
    Owned(SlabRef),
}

/// One object member.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub(crate) key: KeyRef,
    pub(crate) child: Child,
}

/// A JSON value in the arena.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Node {
    Null,
    Bool(bool),
    /// Number literal span into the input; parsed lazily on access.
    Number(Span),
    /// Number literal in the owned-text buffer (mutation/conversion).
    OwnedNumber(SlabRef),
    /// String content span into the input (escapes intact); unescaped lazily
    /// on access.
    String {
        span: Span,
        escaped: bool,
    },
    /// Unescaped string in the owned-text buffer (mutation/conversion);
    /// re-escaped on serialize.
    OwnedString(SlabRef),
    /// Children in the arena's child slab.
    Array(SlabRef),
    /// Members in the arena's entry slab.
    Object(SlabRef),
    /// Builder-only: children live in the builder's mutable overlay until
    /// `seal()` flattens them into a slab. Never present in a sealed
    /// document.
    MutArray(u32),
    /// Builder-only counterpart of [`Node::MutArray`] for objects.
    MutObject(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The top bit separates the two index spaces: an index just below the
    /// limit must round-trip as local and as foreign without aliasing.
    #[test]
    fn boundary_index_keeps_local_and_foreign_apart() {
        let id = CHILD_INDEX_LIMIT - 1;
        assert_eq!(Child::local(id).as_local(), Some(id));
        assert_eq!(Child::local(id).as_foreign(), None);
        assert_eq!(Child::foreign(id).as_foreign(), Some(id));
        assert_eq!(Child::foreign(id).as_local(), None);
    }

    /// Nodes and entries are compact plain data; growing them is a memory
    /// regression the fixture matrix would surface only indirectly.
    #[test]
    fn node_and_entry_stay_compact() {
        assert_eq!(size_of::<Node>(), 12);
        assert_eq!(size_of::<Entry>(), 16);
        assert_eq!(size_of::<Child>(), 4);
    }
}
