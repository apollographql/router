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
///
/// `arena` is `None` once the arena the reference pointed into has been
/// absorbed into this one ([`Arena::absorb`]): the entry then names a local
/// node, so children that already carry its foreign id resolve here without
/// being rewritten.
pub(crate) struct ForeignRef {
    pub(crate) arena: Option<Arc<Arena>>,
    pub(crate) node: NodeId,
}

pub(crate) struct Arena {
    /// The document bytes, validated as UTF-8 before attachment. Held as
    /// `Bytes` (inside [`Utf8Bytes`]) so streaming serialization can yield
    /// untouched spans as zero-copy slices sharing this buffer.
    input: Utf8Bytes,
    /// Inputs of arenas absorbed into this one, each with the offset its
    /// spans were moved to. Spans address the inputs as one sequence: this
    /// arena's own input first, then these in order.
    absorbed_inputs: Vec<(u32, Utf8Bytes)>,
    /// Where the next absorbed input starts in that sequence.
    input_end: u32,
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
    /// Whether the nodes are exactly as the parser wrote them. The parser
    /// writes a container after its members, so every subtree is the
    /// contiguous id range ending at its root, which is what lets
    /// [`Arena::subtrees_disjoint`] check references for overlap cheaply.
    pristine: bool,
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
            absorbed_inputs: Vec::new(),
            input_end: 0,
            chunks: vec![chunk],
            node_count: 0,
            children: Slabs::new(),
            entries: Slabs::new(),
            text: Utf8Slabs::new(),
            foreign: Vec::new(),
            base_bytes,
            root: 0,
            pristine: false,
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

    /// This arena's own input, without any absorbed one.
    #[cfg(test)]
    pub(crate) fn input(&self) -> &[u8] {
        &self.input
    }

    /// Total length of the inputs spans address, absorbed ones included.
    pub(crate) fn input_len(&self) -> usize {
        self.input_end as usize
    }

    /// The input holding `span`, and the offset its spans start at.
    fn segment(&self, span: Span) -> (&Utf8Bytes, usize) {
        match self.absorbed_inputs.first() {
            Some(&(first, _)) if span.start >= first => {
                let at = self
                    .absorbed_inputs
                    .partition_point(|&(start, _)| start <= span.start)
                    - 1;
                let (start, ref input) = self.absorbed_inputs[at];
                (input, start as usize)
            }
            _ => (&self.input, 0),
        }
    }

    /// The `span` of the input as validated text. Spans are recorded at
    /// token boundaries (quotes and digits are ASCII), so they cannot split
    /// a multi-byte character.
    pub(crate) fn input_utf8(&self, span: Span) -> ValidatedUtf8<'_> {
        let (input, offset) = self.segment(span);
        let range = span.range();
        input.utf8().slice(range.start - offset..range.end - offset)
    }

    /// The bytes of `span`.
    pub(crate) fn span_bytes(&self, span: Span) -> &[u8] {
        let (input, offset) = self.segment(span);
        let range = span.range();
        &input[range.start - offset..range.end - offset]
    }

    /// The bytes of `span` as a zero-copy slice of the shared input buffer.
    pub(crate) fn span_shared(&self, span: Span) -> Bytes {
        let (input, offset) = self.segment(span);
        let range = span.range();
        input
            .as_shared()
            .slice(range.start - offset..range.end - offset)
    }

    /// Attaches the input bytes after parsing (the parser reads the buffer
    /// while the arena is being built).
    pub(crate) fn set_input(&mut self, input: Utf8Bytes) {
        debug_assert!(self.input.is_empty(), "input attached twice");
        self.base_bytes += input.len();
        self.input_end = u32::try_from(input.len()).expect("input length within the arena cap");
        self.input = input;
    }

    /// Records that the nodes are exactly as the parser wrote them; see
    /// [`Arena::subtrees_disjoint`].
    pub(crate) fn mark_pristine(&mut self) {
        self.pristine = true;
    }

    /// Records that the nodes may no longer be in parser order, because a
    /// builder has taken the arena over.
    pub(crate) fn mark_edited(&mut self) {
        self.pristine = false;
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
        self.absorbed_inputs.clear();
        self.input_end = 0;
        self.pristine = false;
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

    /// Makes room in a container's child run for `extra` more elements past
    /// its first `used`; see [`Slabs::grow`].
    pub(crate) fn grow_children(
        &mut self,
        run: SlabRef,
        used: usize,
        extra: usize,
        exact: bool,
    ) -> SlabRef {
        self.children.grow(run, used, extra, exact, Child::local(0))
    }

    /// Makes room in an object's entry run for `extra` more members past its
    /// first `used`; see [`Slabs::grow`].
    pub(crate) fn grow_entries(
        &mut self,
        run: SlabRef,
        used: usize,
        extra: usize,
        exact: bool,
    ) -> SlabRef {
        let filler = Entry {
            key: KeyRef::Owned(SlabRef::EMPTY),
            child: Child::local(0),
        };
        self.entries.grow(run, used, extra, exact, filler)
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
            } => span.len as usize == other.len() && self.span_bytes(span) == other,
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
            } => span.len as usize == other.len() && self.span_bytes(span) == other.as_bytes(),
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
        self.foreign.push(ForeignRef {
            arena: Some(arena),
            node,
        });
        Child::foreign(id)
    }

    /// Whether this arena retains more than its own document: it references
    /// nodes owned by other arenas (and therefore pins them), or it has
    /// absorbed another arena, whose unreferenced nodes it keeps.
    pub(crate) fn has_foreign(&self) -> bool {
        !self.foreign.is_empty() || !self.absorbed_inputs.is_empty()
    }

    /// The node a foreign child names: in another arena, or (for an entry
    /// whose arena was absorbed) in this one, when the arena is `None`.
    pub(crate) fn foreign_target(&self, id: ForeignId) -> (Option<&Arc<Arena>>, NodeId) {
        let fref = &self.foreign[id as usize];
        (fref.arena.as_ref(), fref.node)
    }

    /// The local node a child names, if it lives in this arena: a local
    /// child, or a foreign one whose arena has been absorbed.
    pub(crate) fn local_node(&self, child: Child) -> Option<NodeId> {
        match child.as_local() {
            Some(id) => Some(id),
            None => match self.foreign_target(child.as_foreign().expect("child is foreign")) {
                (None, node) => Some(node),
                (Some(_), _) => None,
            },
        }
    }

    /// Resolves a child slot to the arena that owns the node, cloning the
    /// owner's `Arc` for foreign children.
    pub(crate) fn resolve_owner(self: &Arc<Self>, child: Child) -> (Arc<Arena>, NodeId) {
        match child.as_local() {
            Some(id) => (Arc::clone(self), id),
            None => match self.foreign_target(child.as_foreign().expect("child is foreign")) {
                (Some(arena), node) => (Arc::clone(arena), node),
                (None, node) => (Arc::clone(self), node),
            },
        }
    }

    /// Whether the subtrees rooted at `nodes` share no node, judged from the
    /// parser's layout: a parsed container follows its members, so a subtree
    /// is the id range from its leftmost descendant up to its root. Arenas a
    /// builder has edited lose that layout, so for them only a single
    /// reference counts as disjoint. Sorts `nodes`.
    pub(crate) fn subtrees_disjoint(&self, nodes: &mut [NodeId]) -> bool {
        if nodes.len() <= 1 {
            return true;
        }
        if !self.pristine {
            return false;
        }
        nodes.sort_unstable();
        nodes
            .windows(2)
            .all(|pair| pair[0] < self.leftmost_descendant(pair[1]))
    }

    /// The smallest id in the subtree rooted at `node`, in parser layout.
    fn leftmost_descendant(&self, mut node: NodeId) -> NodeId {
        loop {
            let first = match self.node(node) {
                Node::Array(slab) => self.children(slab).first().copied(),
                Node::Object(slab) => self.entries(slab).first().map(|entry| entry.child),
                _ => None,
            };
            match first.and_then(Child::as_local) {
                Some(child) => node = child,
                None => return node,
            }
        }
    }

    /// Whether [`Arena::absorb`] can take `other`: the combined arena has
    /// to fit the id, input and chunk index ranges.
    pub(crate) fn fits_absorb(&self, other: &Arena) -> bool {
        let node_offset = self.chunks.len() * CHUNK_NODES;
        let limit = crate::node::CHILD_INDEX_LIMIT as usize;
        node_offset + (other.node_count as usize) < limit
            && self.foreign.len() + other.foreign.len() < limit
            && self.input_end as usize + other.input_end as usize <= u32::MAX as usize
            && self.children.can_absorb(&other.children)
            && self.entries.can_absorb(&other.entries)
            && self.text.can_absorb(&other.text)
    }

    /// Moves `other`'s storage into this arena, so its nodes become local
    /// ones. Node chunks, slab chunks and the input buffer move rather than
    /// being copied; every id, slab reference and span in them is shifted by
    /// where it landed. Returns the shifts to apply to ids that pointed into
    /// `other`. The caller checks [`Arena::fits_absorb`] first.
    pub(crate) fn absorb(&mut self, mut other: Arena) -> Absorbed {
        debug_assert!(self.fits_absorb(&other));
        let node_offset = self.chunks.len() * CHUNK_NODES;
        let foreign_offset = self.foreign.len();
        let shift = Shift {
            node: node_offset as u32,
            foreign: foreign_offset as u32,
            input: self.input_end,
            children: self.children.chunk_count(),
            entries: self.entries.chunk_count(),
            text: self.text.chunk_count(),
        };

        for chunk in &mut other.chunks {
            for node in chunk.iter_mut() {
                *node = shift.node_value(*node);
            }
        }
        other
            .children
            .for_each_mut(|child| *child = shift.child(*child));
        other
            .entries
            .for_each_mut(|entry| *entry = shift.entry(*entry));
        for fref in &mut other.foreign {
            if fref.arena.is_none() {
                fref.node += shift.node;
            }
        }

        self.node_count = shift.node + other.node_count;
        self.chunks.append(&mut other.chunks);
        self.children.absorb(&mut other.children);
        self.entries.absorb(&mut other.entries);
        self.text.absorb(&mut other.text);
        self.foreign.append(&mut other.foreign);
        let input = std::mem::replace(&mut other.input, Utf8Bytes::new());
        if !input.is_empty() {
            self.absorbed_inputs.push((self.input_end, input));
        }
        for (start, input) in other.absorbed_inputs.drain(..) {
            self.absorbed_inputs.push((self.input_end + start, input));
        }
        self.input_end += other.input_end;
        self.base_bytes += other.base_bytes;
        self.pristine = false;
        Absorbed {
            node_offset: shift.node,
        }
    }

    /// Takes the arena handle out of foreign entry `id`, leaving the entry
    /// to be re-pointed with [`Arena::alias_foreign`] or restored with
    /// [`Arena::restore_foreign`].
    pub(crate) fn take_foreign(&mut self, id: ForeignId) -> Option<Arc<Arena>> {
        self.foreign[id as usize].arena.take()
    }

    /// Puts an arena handle taken with [`Arena::take_foreign`] back.
    pub(crate) fn restore_foreign(&mut self, id: ForeignId, arena: Arc<Arena>) {
        self.foreign[id as usize].arena = Some(arena);
    }

    /// Re-points foreign entry `id`, whose arena handle was taken, at the
    /// local node its node became when that arena was absorbed `offset` ids
    /// along.
    pub(crate) fn alias_foreign(&mut self, id: ForeignId, offset: NodeId) {
        let fref = &mut self.foreign[id as usize];
        debug_assert!(fref.arena.is_none(), "the arena handle was taken");
        fref.node += offset;
    }

    /// Number of foreign entries that still point into another arena.
    #[cfg(test)]
    pub(crate) fn live_foreign(&self) -> usize {
        self.foreign
            .iter()
            .filter(|fref| fref.arena.is_some())
            .count()
    }

    /// Number of foreign entries, for walking those an absorption appended.
    pub(crate) fn foreign_len(&self) -> usize {
        self.foreign.len()
    }
}

/// Where an absorbed arena's ids landed; see [`Arena::absorb`].
#[derive(Clone, Copy)]
pub(crate) struct Absorbed {
    pub(crate) node_offset: NodeId,
}

/// How each kind of index moves when one arena is absorbed into another.
#[derive(Clone, Copy)]
struct Shift {
    node: u32,
    foreign: u32,
    input: u32,
    children: u16,
    entries: u16,
    text: u16,
}

impl Shift {
    fn span(self, span: Span) -> Span {
        Span {
            start: span.start + self.input,
            len: span.len,
        }
    }

    fn child(self, child: Child) -> Child {
        match child.as_local() {
            Some(id) => Child::local(id + self.node),
            None => Child::foreign(child.as_foreign().expect("child is foreign") + self.foreign),
        }
    }

    fn key(self, key: KeyRef) -> KeyRef {
        match key {
            KeyRef::Span { span, escaped } => KeyRef::Span {
                span: self.span(span),
                escaped,
            },
            KeyRef::Owned(text) => KeyRef::Owned(text.shift_chunk(self.text)),
        }
    }

    fn entry(self, entry: Entry) -> Entry {
        Entry {
            key: self.key(entry.key),
            child: self.child(entry.child),
        }
    }

    fn node_value(self, node: Node) -> Node {
        match node {
            Node::Null | Node::Bool(_) => node,
            Node::Number(span) => Node::Number(self.span(span)),
            Node::String { span, escaped } => Node::String {
                span: self.span(span),
                escaped,
            },
            Node::OwnedNumber(text) => Node::OwnedNumber(text.shift_chunk(self.text)),
            Node::OwnedString(text) => Node::OwnedString(text.shift_chunk(self.text)),
            Node::Array(slab) => Node::Array(slab.shift_chunk(self.children)),
            Node::Object(slab) => Node::Object(slab.shift_chunk(self.entries)),
            Node::MutArray(_) | Node::MutObject(_) => {
                unreachable!("only sealed arenas are absorbed")
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
            let Some(arena) = arena else {
                continue;
            };
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
        None => match arena.foreign_target(child.as_foreign().expect("child is foreign")) {
            (Some(owner), node) => (owner, node),
            (None, node) => (arena, node),
        },
    }
}

/// Resolves a child slot to a borrowed view of the owning arena and node.
/// Foreign children borrow through the arena's reference table, so the result
/// lives as long as the starting arena borrow.
pub(crate) fn resolve(arena: &Arena, child: Child) -> (&Arena, NodeId) {
    match child.as_local() {
        Some(id) => (arena, id),
        None => match arena.foreign_target(child.as_foreign().expect("child is foreign")) {
            (Some(owner), node) => (owner, node),
            (None, node) => (arena, node),
        },
    }
}

/// Estimates the node count of a document from its input length, used to size
/// an arena's first chunk.
pub(crate) fn estimate_nodes(input_len: usize) -> usize {
    input_len / 16
}
