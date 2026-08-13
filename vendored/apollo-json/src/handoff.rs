//! In-band marker hand-off for moving subtrees across serde's data model.
//!
//! Serde's data model cannot carry an `Arc` in band, so both serde
//! directions use the same side channel (the `serde_json` `RawValue`
//! technique): a newtype-struct request or value tagged with a private
//! marker name, plus a thread-local slot holding the `(Arc<Arena>, NodeId)`
//! pair. Deserialization captures shared subtrees into `Value` fields;
//! serialization adopts them by reference into the document being built.

use std::cell::Cell;
use std::sync::Arc;

use crate::arena::Arena;
use crate::node::NodeId;

/// Marker name signalling a hand-off through a newtype struct.
pub(crate) const TOKEN: &str = "$apollo_json::private::Capture";

thread_local! {
    /// Hand-off slot between the side that owns the subtree and the side
    /// that recognizes the marker.
    static SLOT: Cell<Option<(Arc<Arena>, NodeId)>> = const { Cell::new(None) };
}

/// Places a subtree in the hand-off slot for the other side to take.
pub(crate) fn stash(arena: Arc<Arena>, node: NodeId) {
    SLOT.with(|slot| slot.set(Some((arena, node))));
}

/// Takes the stashed subtree, leaving the slot empty.
pub(crate) fn take() -> Option<(Arc<Arena>, NodeId)> {
    SLOT.with(Cell::take)
}

/// Empties the slot so a marker exchange that never took its stash cannot
/// leak it into a later one.
pub(crate) fn clear() {
    SLOT.with(Cell::take);
}
