//! Serde serialization of value handles.

mod build;

use std::sync::Arc;

use serde::ser::{Error as _, Serialize, SerializeMap, SerializeSeq, Serializer};

use crate::document::{Value, ValueRef};
use crate::error::JsonError;
use crate::handoff;
use crate::node::Node;
use crate::options::DEFAULT_MAX_DEPTH;

pub use build::to_value;

#[cfg(test)]
mod tests;

impl Serialize for Value {
    /// Serializes the value through serde's data model: strings and keys
    /// in their unescaped form, key order preserved, shared subtrees expanded
    /// at every reference.
    ///
    /// Serde's data model has no raw-literal representation, so numbers
    /// normalize to `i64`/`u64`/`f64` at `serde_json`'s reading of the
    /// literal — `1.50e2` serializes as `150.0`, integer literals beyond 64
    /// bits as `f64` — and the original spelling cannot survive (serializing
    /// into `serde_json` re-formats every number). For byte-identical output
    /// use [`Value::to_vec`] and friends instead.
    ///
    /// Under [`to_value`] the value is instead adopted by reference —
    /// shared, not copied — preserving raw literals; see [`to_value`].
    ///
    /// # Errors
    /// Number literals whose value overflows `f64`, and compositions nested
    /// deeper than 128 levels, error through the target serializer.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        adopt_or_walk(serializer, &self.arena, self.node, self.value())
    }
}

/// Offers the subtree for adoption through the marker hand-off, walking it
/// structurally on any serializer that does not take it.
fn adopt_or_walk<S>(
    serializer: S,
    arena: &Arc<crate::arena::Arena>,
    node: crate::node::NodeId,
    value: ValueRef<'_>,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    handoff::stash(Arc::clone(arena), node);
    let result = serializer.serialize_newtype_struct(handoff::TOKEN, &Structural::root(value));
    // Serializers other than to_value never take the stash; drop it
    // rather than leak it into a later serialization.
    handoff::clear();
    result
}

impl Serialize for ValueRef<'_> {
    /// Serializes the subtree; see [`Value`]'s `Serialize` impl for the
    /// data-model semantics. A borrowed view carries no owning handle, so
    /// [`to_value`] copies it structurally — share a
    /// [`Value`] to adopt the subtree by reference instead.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Structural::root(*self).serialize(serializer)
    }
}

/// One node in the serialization walk, carrying the remaining nesting
/// budget: parsing already caps document depth, but compositions can stack
/// arbitrarily many arenas.
struct Structural<'a> {
    value: ValueRef<'a>,
    depth: usize,
}

impl<'a> Structural<'a> {
    fn root(value: ValueRef<'a>) -> Self {
        Structural {
            value,
            depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// The nesting budget for children, or the depth error when this
    /// container would exceed it.
    fn descend<E>(&self) -> Result<usize, E>
    where
        E: serde::ser::Error,
    {
        self.depth.checked_sub(1).ok_or_else(|| {
            E::custom(JsonError::DepthLimitExceeded {
                limit: DEFAULT_MAX_DEPTH,
            })
        })
    }
}

impl Serialize for Structural<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.value.node() {
            Node::Null => serializer.serialize_unit(),
            Node::Bool(b) => serializer.serialize_bool(b),
            Node::Number(_) | Node::OwnedNumber(_) => serialize_number(
                self.value.raw_number().expect("node is a number"),
                serializer,
            ),
            Node::String { .. } | Node::OwnedString(_) => {
                serializer.serialize_str(&self.value.as_str().expect("node is a string"))
            }
            Node::Array(slab) => {
                let depth = self.descend::<S::Error>()?;
                let mut seq = serializer.serialize_seq(Some(slab.len()))?;
                for element in self.value.array_iter() {
                    seq.serialize_element(&Structural {
                        value: element,
                        depth,
                    })?;
                }
                seq.end()
            }
            Node::Object(slab) => {
                let depth = self.descend::<S::Error>()?;
                let mut map = serializer.serialize_map(Some(slab.len()))?;
                for (key, member) in self.value.object_iter() {
                    map.serialize_entry(
                        &*key,
                        &Structural {
                            value: member,
                            depth,
                        },
                    )?;
                }
                map.end()
            }
            Node::MutArray(_) | Node::MutObject(_) => {
                unreachable!("builder-only nodes never appear in sealed documents")
            }
        }
    }
}

/// Serializes a number literal at `serde_json`'s reading of it, per
/// [`classify_number`](crate::de::classify_number).
fn serialize_number<S>(raw: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use crate::de::NumberClass;
    match crate::de::classify_number(raw) {
        NumberClass::Unsigned(n) => serializer.serialize_u64(n),
        NumberClass::Signed(n) => serializer.serialize_i64(n),
        NumberClass::Float => {
            let value: f64 = raw.parse().expect("number literals parse as floats");
            if value.is_finite() {
                serializer.serialize_f64(value)
            } else {
                Err(S::Error::custom(format_args!("number out of range: {raw}")))
            }
        }
    }
}
