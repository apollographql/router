//! The arena backing a document: the input bytes, the node store, slab
//! storage for container children / object entries / owned text, and the
//! foreign-reference table.
//!
//! "Arena" means the lifetime model (one region per document, freed
//! wholesale), not a bump-pointer allocator: storage is plain `Vec`-backed
//! chunks addressed by `u32` indices, which keeps it safe Rust, byte-cap
//! accountable, and resettable for recycling.
//!
//! All storage grows by fixed-size chunks (linear growth — no geometric
//! doubling and no worst-case pre-allocation). The whole arena carries a
//! single atomic refcount via `Arc<Arena>`; sharing a subtree bumps that one
//! counter, and because nodes and slabs are plain data, dropping the last
//! reference frees whole chunks with no per-node destructor traversal.

use std::borrow::Cow;
use std::mem;
use std::sync::Arc;

use bytes::Bytes;

use crate::node::{Child, Entry, KeyRef, Node, NodeId, Span};
use crate::slab::{SlabRef, Slabs};
use crate::utf8::{Utf8Bytes, Utf8Slabs, ValidatedUtf8};

/// Index into an arena's foreign-reference table.
pub(crate) type ForeignId = u32;

/// Nodes per full chunk: 4096 × 12-byte nodes = 48 KiB.
const CHUNK_NODES: usize = 4096;

/// Node estimate for arenas built value-by-value with no size signal —
/// builders, serializer sinks, deep copies. Also the smallest first chunk an
/// estimate can request, so tiny documents get a tiny chunk.
pub(crate) const DEFAULT_NODE_ESTIMATE: usize = 8;

/// A reference to a node owned by another arena. Holding the `Arc` keeps the
/// foreign arena (and everything it pins) alive.
pub(crate) struct ForeignRef {
    pub(crate) arena: Arc<Arena>,
    pub(crate) node: NodeId,
}

pub(crate) struct Arena {
    /// The document bytes, validated as UTF-8 before attachment. Held as
    /// `Bytes` (inside [`Utf8Bytes`]) so streaming serialization can yield
    /// untouched spans as zero-copy slices sharing this buffer.
    input: Utf8Bytes,
    chunks: Vec<Vec<Node>>,
    /// Total nodes across chunks, kept so pushes do not recompute it.
    node_count: u32,
    children: Slabs<Child>,
    entries: Slabs<Entry>,
    text: Utf8Slabs,
    foreign: Vec<ForeignRef>,
    /// Bytes retained outside the slab stores (input, node chunks, foreign
    /// table), maintained incrementally for the size cap.
    base_bytes: usize,
    /// The node this arena was built to represent, recorded when the owning
    /// document is finalized. A document rooted anywhere else is a subtree
    /// view that retains more than it can reach (see
    /// `Value::is_self_contained`).
    root: NodeId,
}

impl Arena {
    /// Creates an arena with no input attached yet. `estimated_nodes` sizes
    /// the first node chunk (clamped to one full chunk) so small documents
    /// do not pay for a full chunk.
    pub(crate) fn new(estimated_nodes: usize) -> Self {
        let first = estimated_nodes.clamp(DEFAULT_NODE_ESTIMATE, CHUNK_NODES);
        let chunk: Vec<Node> = Vec::with_capacity(first);
        let base_bytes = chunk.capacity() * mem::size_of::<Node>();
        Arena {
            input: Utf8Bytes::new(),
            chunks: vec![chunk],
            node_count: 0,
            children: Slabs::new(),
            entries: Slabs::new(),
            text: Utf8Slabs::new(),
            foreign: Vec::new(),
            base_bytes,
            root: 0,
        }
    }

    /// Records the node the arena represents; set once when the owning
    /// document is finalized.
    pub(crate) fn set_root(&mut self, root: NodeId) {
        self.root = root;
    }

    /// The node the arena was built to represent.
    pub(crate) fn root(&self) -> NodeId {
        self.root
    }

    pub(crate) fn input(&self) -> &[u8] {
        &self.input
    }

    /// The `span` of the input as validated text. Spans are recorded at
    /// token boundaries (quotes and digits are ASCII), so they cannot split
    /// a multi-byte character.
    pub(crate) fn input_utf8(&self, span: Span) -> ValidatedUtf8<'_> {
        self.input.utf8().slice(span.range())
    }

    /// The input as a shareable buffer, for zero-copy chunk slices.
    pub(crate) fn input_bytes(&self) -> &Bytes {
        self.input.as_shared()
    }

    /// Attaches the input bytes after parsing (the parser reads the buffer
    /// while the arena is being built).
    pub(crate) fn set_input(&mut self, input: Utf8Bytes) {
        debug_assert!(self.input.is_empty(), "input attached twice");
        self.base_bytes += input.len();
        self.input = input;
    }

    /// Approximate bytes retained by this arena.
    pub(crate) fn bytes(&self) -> usize {
        self.base_bytes + self.children.bytes() + self.entries.bytes() + self.text.bytes()
    }

    /// Clears the arena for reuse: releases the input and every foreign
    /// reference (and whatever they pinned), empties nodes and slabs, and
    /// keeps the chunk storage capacity for the next parse.
    pub(crate) fn reset(&mut self) {
        self.input = Utf8Bytes::new();
        for chunk in &mut self.chunks {
            chunk.clear();
        }
        self.node_count = 0;
        self.children.reset();
        self.entries.reset();
        self.text.reset();
        self.foreign.clear();
        self.base_bytes =
            self.chunks.iter().map(Vec::capacity).sum::<usize>() * mem::size_of::<Node>();
        self.root = 0;
    }

    pub(crate) fn node(&self, id: NodeId) -> Node {
        let id = id as usize;
        self.chunks[id / CHUNK_NODES][id % CHUNK_NODES]
    }

    pub(crate) fn set_node(&mut self, id: NodeId, node: Node) {
        let id = id as usize;
        self.chunks[id / CHUNK_NODES][id % CHUNK_NODES] = node;
    }

    /// Appends a node, spilling into the next fixed-size chunk when the
    /// current one is full. Chunks are addressed by node index, so cleared
    /// chunks retained by a recycled arena refill in order.
    pub(crate) fn push_node(&mut self, node: Node) -> NodeId {
        let count = self.node_count;
        // Node ids become `Child` payloads, whose top bit marks foreign
        // references: an id reaching that bit would silently alias a
        // foreign-table entry, so node capacity stops one bit short of
        // `u32`. At 12 bytes per node the documented byte cap trips long
        // before this does; the assert is the hard backstop.
        assert!(
            count < crate::node::CHILD_INDEX_LIMIT,
            "arena node count exceeds the 2^31 local id space"
        );
        let index = count as usize / CHUNK_NODES;
        if index == self.chunks.len() {
            let fresh: Vec<Node> = Vec::with_capacity(CHUNK_NODES);
            self.base_bytes += fresh.capacity() * mem::size_of::<Node>();
            self.chunks.push(fresh);
        }
        let chunk = &mut self.chunks[index];
        if chunk.len() == chunk.capacity() && chunk.capacity() < CHUNK_NODES {
            // Ramp the first (estimate-sized) chunk up to the fixed chunk
            // size; the transient waste is bounded by one chunk.
            let grow = chunk.capacity().min(CHUNK_NODES - chunk.capacity());
            chunk.reserve_exact(grow);
            self.base_bytes += grow * mem::size_of::<Node>();
        }
        chunk.push(node);
        self.node_count = count + 1;
        count
    }

    pub(crate) fn alloc_children(&mut self, items: &[Child]) -> SlabRef {
        self.children.alloc(items)
    }

    pub(crate) fn children(&self, slab: SlabRef) -> &[Child] {
        self.children.get(slab)
    }

    pub(crate) fn children_mut(&mut self, slab: SlabRef) -> &mut [Child] {
        self.children.get_mut(slab)
    }

    pub(crate) fn alloc_entries(&mut self, items: &[Entry]) -> SlabRef {
        self.entries.alloc(items)
    }

    pub(crate) fn entries(&self, slab: SlabRef) -> &[Entry] {
        self.entries.get(slab)
    }

    pub(crate) fn entries_mut(&mut self, slab: SlabRef) -> &mut [Entry] {
        self.entries.get_mut(slab)
    }

    /// Copies `text` into the owned-text buffer.
    pub(crate) fn alloc_text(&mut self, text: &str) -> SlabRef {
        self.text.alloc(text)
    }

    pub(crate) fn text(&self, slab: SlabRef) -> &[u8] {
        self.text.get(slab).as_bytes()
    }

    /// Owned text as `&str`; always valid UTF-8 because it is written from
    /// `&str` values.
    pub(crate) fn text_str(&self, slab: SlabRef) -> &str {
        self.text.get(slab).as_str()
    }

    /// The key's logical (unescaped) text.
    pub(crate) fn key_unescaped(&self, key: KeyRef) -> Cow<'_, str> {
        match key {
            KeyRef::Span {
                span,
                escaped: false,
            } => Cow::Borrowed(self.input_utf8(span).as_str()),
            KeyRef::Span {
                span,
                escaped: true,
            } => Cow::Owned(crate::text::unescape(self.input_utf8(span))),
            KeyRef::Owned(text) => Cow::Borrowed(self.text_str(text)),
        }
    }

    /// Compares the key's logical bytes to `other`, skipping the byte fetch
    /// when lengths differ (lengths live in the key reference itself).
    pub(crate) fn key_matches_bytes(&self, key: KeyRef, other: &[u8]) -> bool {
        match key {
            KeyRef::Span {
                span,
                escaped: false,
            } => span.len as usize == other.len() && span.slice(&self.input) == other,
            KeyRef::Span {
                span,
                escaped: true,
            } => crate::text::unescape(self.input_utf8(span)).as_bytes() == other,
            KeyRef::Owned(text) => {
                text.len() == other.len() && self.text.get(text).as_bytes() == other
            }
        }
    }

    /// Compares the key's logical text to `other`. Lengths live in the key
    /// reference itself, so mismatched lengths skip the byte fetch entirely
    /// (this runs in every linear member scan).
    pub(crate) fn key_matches_str(&self, key: KeyRef, other: &str) -> bool {
        match key {
            KeyRef::Span {
                span,
                escaped: false,
            } => span.len as usize == other.len() && span.slice(&self.input) == other.as_bytes(),
            KeyRef::Span {
                span,
                escaped: true,
            } => crate::text::unescape(self.input_utf8(span)) == other,
            KeyRef::Owned(text) => {
                text.len() == other.len() && self.text.get(text).as_bytes() == other.as_bytes()
            }
        }
    }

    /// Records a reference to a node owned by `arena`, keeping that arena
    /// alive for as long as this one exists.
    pub(crate) fn push_foreign(&mut self, arena: Arc<Arena>, node: NodeId) -> Child {
        // Foreign ids share the `Child` payload with local node ids (top bit
        // set); the same one-bit-short capacity bound applies.
        let id = ForeignId::try_from(self.foreign.len())
            .ok()
            .filter(|&id| id < crate::node::CHILD_INDEX_LIMIT)
            .expect("foreign reference count exceeds the 2^31 foreign id space");
        self.base_bytes += mem::size_of::<ForeignRef>();
        self.foreign.push(ForeignRef { arena, node });
        Child::foreign(id)
    }

    /// Whether this arena references nodes owned by other arenas (and
    /// therefore pins them).
    pub(crate) fn has_foreign(&self) -> bool {
        !self.foreign.is_empty()
    }

    pub(crate) fn foreign_ref(&self, id: ForeignId) -> &ForeignRef {
        &self.foreign[id as usize]
    }

    /// Resolves a child slot to the arena that owns the node, cloning the
    /// owner's `Arc` for foreign children.
    pub(crate) fn resolve_owner(self: &Arc<Self>, child: Child) -> (Arc<Arena>, NodeId) {
        match child.as_local() {
            Some(id) => (Arc::clone(self), id),
            None => {
                let fref = self.foreign_ref(child.as_foreign().expect("child is foreign"));
                (Arc::clone(&fref.arena), fref.node)
            }
        }
    }
}

impl Drop for Arena {
    /// Releases foreign references iteratively so chains of cross-document
    /// references cannot recurse through nested arena drops. Node chunks and
    /// slabs are plain data and free without per-element work.
    fn drop(&mut self) {
        let mut pending = mem::take(&mut self.foreign);
        while let Some(ForeignRef { arena, .. }) = pending.pop() {
            if let Ok(mut inner) = Arc::try_unwrap(arena) {
                // Strip the doomed arena's own references before it drops so
                // its `Drop` sees an empty table and does not recurse.
                pending.append(&mut inner.foreign);
            }
        }
    }
}

/// Resolves a child slot to the owning arena's shared handle and node,
/// borrowing through the foreign table — no refcount traffic. The borrowed
/// `Arc` can be cloned when an owned handle to the subtree is needed.
pub(crate) fn resolve_shared(arena: &Arc<Arena>, child: Child) -> (&Arc<Arena>, NodeId) {
    match child.as_local() {
        Some(id) => (arena, id),
        None => {
            let fref = arena.foreign_ref(child.as_foreign().expect("child is foreign"));
            (&fref.arena, fref.node)
        }
    }
}

/// Resolves a child slot to a borrowed view of the owning arena and node.
/// Foreign children borrow through the arena's reference table, so the result
/// lives as long as the starting arena borrow.
pub(crate) fn resolve(arena: &Arena, child: Child) -> (&Arena, NodeId) {
    match child.as_local() {
        Some(id) => (arena, id),
        None => {
            let fref = arena.foreign_ref(child.as_foreign().expect("child is foreign"));
            (&fref.arena, fref.node)
        }
    }
}

/// Estimates the node count of a document from its input length, used to size
/// an arena's first chunk.
pub(crate) fn estimate_nodes(input_len: usize) -> usize {
    input_len / 16
}
