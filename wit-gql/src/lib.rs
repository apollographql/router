//! Decode a WebAssembly component's WIT interface and map its exported functions to a GraphQL
//! surface.
//!
//! This crate is the single source of truth shared by:
//! - the `wit-to-gql` CLI, which renders a GraphQL SDL from a component ([`schema::generate`]); and
//! - the Apollo Router's wasm data-source integration, which needs the inverse mapping at query
//!   time — GraphQL field → WIT export ([`mapping::operation_map`]).
//!
//! Because both consumers use the same traversal and [`naming`] rules, the generated schema and the
//! runtime dispatch mapping cannot drift.

pub mod decode;
pub mod mapping;
pub mod naming;
pub mod schema;

pub use decode::decode_component;
pub use mapping::{ExportLocation, FieldMapping, OpKind, OperationMap, ParamMapping, operation_map};
pub use schema::generate;
