//! Arena-backed JSON document model with structural sharing and zero-copy
//! pass-through serialization.
//!
//! A parsed [`Value`] keeps its input bytes and all nodes in one arena
//! behind a single atomic refcount. Leaf scalars are spans into the input —
//! numbers parse and strings unescape lazily on access, and serializing an
//! untouched value emits the original bytes verbatim. Subtrees are
//! themselves [`Value`] handles, shared across documents by reference; a
//! [`Value`] keeps its arena alive and is `Send + Sync + 'static`. Mutation goes
//! through [`ValueBuilder`]: in place for nodes the builder owns, copy-on-
//! write path copying for nodes shared with other handles.
//!
//! Parsing, serialization, conversion, and drop are all iterative with
//! explicit stacks, and parsing enforces configurable depth and arena-size
//! limits ([`ParseOptions`]).
//!
//! Typed access goes through serde in both directions: [`from_value`]
//! deserializes any `T: serde::Deserialize` straight off the parsed nodes,
//! borrowing escape-free strings zero-copy; [`from_slice`] and [`from_str`]
//! deserialize in a single streaming pass without building a document; and
//! [`to_value`] builds a value from any `T: serde::Serialize`. Fields typed
//! [`Value`] cross serde without a rebuild — captured on deserialize,
//! adopted on serialize — and reserialize byte-identically.
//!
//! # Example
//! ```
//! use apollo_json::{ValueBuilder, Value};
//!
//! let a = Value::parse(br#"{"user":{"id":1,"name":"ada"}}"#.to_vec())?;
//! let b = Value::parse(br#"{"labels":["x","y"]}"#.to_vec())?;
//!
//! // Compose a response that references subtrees of both documents.
//! let mut builder = ValueBuilder::new();
//! builder.set("user", a.get("user").unwrap())?;
//! builder.set("labels", b.get("labels").unwrap())?;
//! let composed = builder.seal();
//! drop((a, b)); // the composition keeps both arenas alive
//!
//! // Edit through cursors, then seal back into an immutable document.
//! let mut builder = composed.edit();
//! builder.get_mut("user")?.set("id", 2)?;
//! assert_eq!(
//!     builder.seal().to_vec(),
//!     br#"{"user":{"id":2,"name":"ada"},"labels":["x","y"]}"#
//! );
//! # Ok::<(), apollo_json::JsonError>(())
//! ```
//!
//! Typed access is plain serde, and a field typed [`Value`] captures its
//! subtree by reference — raw literals survive untouched:
//! ```
//! use apollo_json::Value;
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct Event {
//!     id: u64,
//!     payload: Value,
//! }
//!
//! let event: Event = apollo_json::from_str(r#"{"id":7,"payload":{"price":1.50}}"#)?;
//! assert_eq!(event.id, 7);
//! assert_eq!(event.payload.to_vec(), br#"{"price":1.50}"#);
//! # Ok::<(), apollo_json::JsonError>(())
//! ```
//!
//! Sharing pins whole arenas: retaining one handle keeps everything parsed
//! alongside it resident. The retention boundary is
//! [`Value::into_self_contained`] — free for values that already own
//! everything they reference — which anything storing values beyond the
//! request lifecycle (caches, subscriptions) must call or require.
//!
//! # Limitations
//!
//! A [`Value`] is a reference — an arena plus a node index — rather than a
//! self-contained tree. Anything that requires a value to exist apart from
//! its arena runs into one of the points below.
//!
//! ## Capture cannot cross serde's buffering — and panics
//!
//! Serde's data model carries data (strings, numbers, maps), never
//! references, so a [`Value`] field crosses serde through a side channel
//! that only this crate's deserializers answer. Two situations make that
//! side channel unreachable, and both panic:
//!
//! - **A foreign deserializer** (`serde_json`, a web framework's `.json()`,
//!   `serde_yaml`, ...) has no arena to hand over. No input makes such a
//!   call succeed.
//! - **Serde's internal buffering** — `#[serde(flatten)]`, untagged enums,
//!   and internally tagged enums — copies the input into serde's own owned
//!   tree and replays it; by replay time the arena is gone, even under this
//!   crate's own deserializer. Adjacently tagged enums buffer only when the
//!   content member precedes the tag in the input, which makes them fail by
//!   JSON key order — treat them as unsupported.
//!
//! The panic is deliberate: the compiled code can never work, and a
//! deserialization error would be consumed as control flow (fallback arms,
//! cache misses), hiding the defect. Do not catch it — restructure per
//! below.
//!
//! To parse an envelope whose *shape* needs those serde features — a tagged
//! protocol message carrying a JSON payload — parse the whole body with
//! this crate and read the envelope by key instead. A parsed document is
//! randomly addressable, so the tag's position never matters (the property
//! serde buys with buffering), and the payload subtree is captured from the
//! same arena by reference:
//! ```
//! use apollo_json::Value;
//!
//! let bytes = br#"{"payload":{"n":1.50},"type":"next"}"#.to_vec();
//! let message = Value::parse(bytes)?;
//! assert_eq!(message.get("type").unwrap().as_string().as_deref(), Some("next"));
//! let payload = message.get("payload").unwrap(); // shares the arena — no copy
//! assert_eq!(payload.to_vec(), br#"{"n":1.50}"#);
//! # Ok::<(), apollo_json::JsonError>(())
//! ```
//!
//! Serialization is unaffected: serde serializes by streaming, never by
//! buffering, so a [`Value`] field serializes through any serializer — as an
//! arena reference under this crate's serializer, structurally under a
//! foreign one (where number spelling may normalize, e.g. `1.50` to `1.5`).
//!
//! ## Retention pins the whole arena
//!
//! A handle keeps its entire source arena resident, however small the
//! subtree it names — the same failure mode as slicing a large buffer with
//! `bytes::Bytes`. Request-scoped values are fine; anything stored past the
//! request (caches, subscriptions, deduplication state) must sever the pin
//! with [`Value::into_self_contained`] or [`Value::compact`], which
//! deep-copy into a minimal arena.
//!
//! ## Reading a document under construction requires [`BuilderRef`]
//!
//! A container that grows inside a [`ValueBuilder`] lives in an overlay
//! owned by the builder until [`ValueBuilder::seal`] packs it. A sealed
//! reader ([`ValueRef`]) cannot see overlays, so reads during assembly go
//! through [`BuilderRef`] (from [`ValueBuilder::value`] or
//! [`ValueMut::value`]), which resolves them.
//!
//! ## Serialized output can exceed in-memory size
//!
//! Documents are DAGs: adopting a subtree stores a reference, and
//! serialization expands every reference. Size accounting for limits or
//! cost analysis must measure serialized expansion, not arena bytes.

mod arena;
mod builder;
mod construct;
mod convert;
mod cursor;
mod de;
mod detach;
mod document;
mod error;
mod handoff;
mod lex;
mod macros;
mod node;
mod options;
mod parse;
mod peek;
mod ser;
mod serialize;
mod simd;
mod slab;
mod stream;
mod text;
mod utf8;

pub use builder::{NewValue, PathSegment, ValueBuilder};
pub use cursor::ValueMut;
pub use de::{from_slice, from_slice_with_buffers, from_str, from_value};
pub use document::{JsonKind, Value, ValueRef};
pub use error::JsonError;
pub use options::ParseOptions;
pub use parse::ParseBuffers;
pub use peek::BuilderRef;
pub use ser::to_value;
pub use stream::Chunks;
