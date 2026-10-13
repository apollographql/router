//! Streaming serialization: the document as a sequence of [`Bytes`] chunks.
//!
//! Chunks come from a small accumulation buffer that flushes near the target
//! size, except that large untouched input spans are yielded as zero-copy
//! slices sharing the arena's input buffer — the streaming form of the
//! verbatim-span pass-through. Holding a yielded chunk can therefore keep a
//! source arena's input buffer alive. The walk is iterative and the output
//! is byte-identical to [`Value::to_vec`](crate::Value::to_vec).

use std::sync::Arc;

use bytes::Bytes;

use crate::arena::Arena;
use crate::node::{KeyRef, Node, NodeId, Span};
use crate::text::escape_into;

/// Owned chunk iterator over a serialized document or subtree.
///
/// The iterator owns references to every arena it walks, so it is
/// `Send + Sync + 'static` and suitable as the source of an HTTP response
/// body.
pub struct Chunks {
    stack: Vec<Frame>,
    pending: Option<(Arc<Arena>, NodeId)>,
    accum: Vec<u8>,
    /// A zero-copy slice waiting to be yielded after the flush that
    /// preceded it.
    queued: Option<Bytes>,
    target: usize,
    done: bool,
}

struct Frame {
    arena: Arc<Arena>,
    node: NodeId,
    next: usize,
}

impl Chunks {
    pub(crate) fn new(arena: Arc<Arena>, root: NodeId, target_chunk_size: usize) -> Self {
        Chunks {
            stack: Vec::new(),
            pending: Some((arena, root)),
            accum: Vec::new(),
            queued: None,
            target: target_chunk_size.max(16),
            done: false,
        }
    }

    /// Spans at least this long are yielded as zero-copy slices; shorter
    /// ones are cheaper to copy than to refcount and fragment.
    fn zero_copy_min(&self) -> usize {
        (self.target / 2).max(64)
    }

    fn flush(&mut self) -> Option<Bytes> {
        if self.accum.is_empty() {
            None
        } else {
            Some(Bytes::from(std::mem::take(&mut self.accum)))
        }
    }

    fn chunk_if_full(&mut self) -> Option<Bytes> {
        if self.accum.len() >= self.target {
            self.flush()
        } else {
            None
        }
    }

    /// Emits an input span: zero-copy for large spans (flushing what was
    /// accumulated first, to preserve output order), copied otherwise.
    fn emit_span(&mut self, arena: &Arena, span: Span) -> Option<Bytes> {
        if span.len as usize >= self.zero_copy_min() {
            let start = span.start as usize;
            let slice = arena.input_bytes().slice(start..start + span.len as usize);
            match self.flush() {
                Some(chunk) => {
                    self.queued = Some(slice);
                    Some(chunk)
                }
                None => Some(slice),
            }
        } else {
            self.accum.extend_from_slice(span.slice(arena.input()));
            None
        }
    }

    /// Advances the walk one step, returning a chunk if one became ready.
    fn step(&mut self) -> Option<Bytes> {
        if let Some((arena, node)) = self.pending.take() {
            match arena.node(node) {
                Node::Null => self.accum.extend_from_slice(b"null"),
                Node::Bool(true) => self.accum.extend_from_slice(b"true"),
                Node::Bool(false) => self.accum.extend_from_slice(b"false"),
                Node::Number(span) => {
                    let ready = self.emit_span(&arena, span);
                    if ready.is_some() {
                        return ready;
                    }
                }
                Node::OwnedNumber(text) => self.accum.extend_from_slice(arena.text(text)),
                Node::String { span, .. } => {
                    self.accum.push(b'"');
                    let ready = self.emit_span(&arena, span);
                    // The closing quote lands in the (possibly fresh)
                    // accumulator, which is emitted after any queued slice —
                    // output order is preserved.
                    self.accum.push(b'"');
                    if ready.is_some() {
                        return ready;
                    }
                }
                Node::OwnedString(text) => {
                    self.accum.push(b'"');
                    escape_into(arena.text(text), &mut self.accum);
                    self.accum.push(b'"');
                }
                Node::Array(slab) => {
                    self.accum.push(b'[');
                    if slab.len() == 0 {
                        self.accum.push(b']');
                    } else {
                        self.stack.push(Frame {
                            arena,
                            node,
                            next: 0,
                        });
                    }
                }
                Node::Object(slab) => {
                    self.accum.push(b'{');
                    if slab.len() == 0 {
                        self.accum.push(b'}');
                    } else {
                        self.stack.push(Frame {
                            arena,
                            node,
                            next: 0,
                        });
                    }
                }
                Node::MutArray(_) | Node::MutObject(_) => {
                    unreachable!("builder overlays are flattened at seal")
                }
            }
            return self.chunk_if_full();
        }

        let Some(frame) = self.stack.last() else {
            self.done = true;
            return None;
        };
        let (arena, node, next) = (Arc::clone(&frame.arena), frame.node, frame.next);
        match arena.node(node) {
            Node::Array(slab) => {
                let items = arena.children(slab);
                if next == items.len() {
                    self.accum.push(b']');
                    self.stack.pop();
                    return self.chunk_if_full();
                }
                let child = items[next];
                if next > 0 {
                    self.accum.push(b',');
                }
                self.stack.last_mut().expect("frame checked above").next += 1;
                self.pending = Some(arena.resolve_owner(child));
            }
            Node::Object(slab) => {
                let entries = arena.entries(slab);
                if next == entries.len() {
                    self.accum.push(b'}');
                    self.stack.pop();
                    return self.chunk_if_full();
                }
                let entry = entries[next];
                if next > 0 {
                    self.accum.push(b',');
                }
                self.stack.last_mut().expect("frame checked above").next += 1;
                self.accum.push(b'"');
                match entry.key {
                    KeyRef::Span { span, .. } => {
                        self.accum.extend_from_slice(span.slice(arena.input()));
                    }
                    KeyRef::Owned(text) => escape_into(arena.text(text), &mut self.accum),
                }
                self.accum.extend_from_slice(b"\":");
                self.pending = Some(arena.resolve_owner(entry.child));
            }
            _ => unreachable!("only containers are stacked"),
        }
        self.chunk_if_full()
    }
}

impl std::fmt::Debug for Chunks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chunks").finish_non_exhaustive()
    }
}

impl Iterator for Chunks {
    type Item = Bytes;

    fn next(&mut self) -> Option<Bytes> {
        if let Some(queued) = self.queued.take() {
            return Some(queued);
        }
        loop {
            if let Some(chunk) = self.step() {
                return Some(chunk);
            }
            if self.done {
                return self.flush();
            }
        }
    }
}
