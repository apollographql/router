//! Capture of subtrees into [`Value`] fields.
//!
//! The `Deserialize` impls below request `deserialize_newtype_struct` with
//! the private marker name from [`crate::handoff`]; the crate's
//! deserializers recognize it and stash an `(Arc<Arena>, NodeId)` pair in
//! the hand-off slot, and the visitor takes it back out. The document path
//! hands over a share of the source arena (one refcount bump); the
//! streaming path hands over a dedicated arena holding just the subtree's
//! bytes. Either way the subtree is never rebuilt through serde's data
//! model, so reserialization stays byte-identical.
//!
//! On any other `Deserializer` the marker goes unrecognized, the slot stays
//! empty, and deserialization fails — including serde's internal buffering,
//! which replays values through a generic content deserializer (the same
//! limitation `RawValue` has). `flatten`, untagged enums, and internally
//! tagged enums always buffer; adjacently tagged enums buffer only when the
//! content member precedes the tag in the input, so a capture inside one
//! succeeds or fails with the input's key order.

use std::sync::Arc;

use serde::de::{self, Visitor};

use crate::arena::Arena;
use crate::document::Value;
use crate::error::JsonError;
use crate::handoff;
use crate::node::NodeId;

/// Error text for a capture request served by anything other than this
/// crate's deserializers.
const CANNOT_CAPTURE: &str = "apollo_json::Value can only be \
     deserialized from apollo-json deserializers, and not through serde's internal \
     buffering (flatten, or tagged and untagged enums). The compiled code can never \
     succeed here: parse with apollo_json::from_slice/from_str/from_value, or keep \
     arena values out of buffered containers";

struct CaptureVisitor;

impl<'de> Visitor<'de> for CaptureVisitor {
    type Value = (Arc<Arena>, NodeId);

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a subtree captured from an apollo-json document")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(handoff::take().unwrap_or_else(|| panic!("{CANNOT_CAPTURE}")))
    }

    fn visit_newtype_struct<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        // A deserializer that does not recognize the marker falls through to
        // here (serde_json, serde's flatten/untagged content buffering, ...).
        // That is a defect in the calling code, not a runtime condition: no
        // input makes it succeed. An error would be swallowed -- callers use
        // deserialization errors as control flow, and a cache treats a failed
        // read as a miss, degrading silently on every hit.
        panic!("{CANNOT_CAPTURE}")
    }
}

impl<'de> de::Deserialize<'de> for Value {
    /// Captures the value's subtree when driven by this crate's
    /// deserializers: [`from_value`](crate::from_value) shares the source
    /// arena — one refcount bump, no copy — while
    /// [`from_slice`](crate::from_slice) and [`from_str`](crate::from_str)
    /// give the handle its own arena holding just the subtree's bytes. Both
    /// reserialize byte-identically. Panics on any other deserializer,
    /// including serde's internal buffering: `flatten`, untagged and
    /// internally tagged enums, and adjacently tagged enums whenever the
    /// content member precedes the tag in the input (so those captures
    /// depend on JSON key order — treat them as unsupported). The panic is
    /// deliberate: no input makes such a call succeed, and an error would
    /// disappear into fallbacks and cache-miss handling.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let (arena, node) =
            deserializer.deserialize_newtype_struct(handoff::TOKEN, CaptureVisitor)?;
        Ok(Value { arena, node })
    }
}

/// Fulfils a capture request: stashes the deserializer's subtree and signals
/// the visitor to take it.
pub(super) fn capture<'de, V>(
    deserializer: &super::ValueDeserializer<'de>,
    visitor: V,
) -> Result<V::Value, JsonError>
where
    V: Visitor<'de>,
{
    handoff::stash(Arc::clone(deserializer.arena()), deserializer.node_id());
    let result = visitor.visit_unit();
    // The visitor takes the stash; if a foreign impl requested our marker
    // name but never collected, drop the capture rather than leak it into a
    // later deserialization.
    handoff::clear();
    result
}
