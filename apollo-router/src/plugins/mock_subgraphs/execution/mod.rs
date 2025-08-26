//! GraphQL execution engine initially copied from
//! <https://github.com/apollographql/apollo-rs/tree/apollo-compiler%401.27.0/crates/apollo-compiler/src/execution>
//!
//! It exists inside apollo-compiler to support introspection but is not exposed in its public API.
//! This may change if we figure out a good public API for execution and resolvers,
//! at which point this duplicated code could be removed.
//!
//! Changes to merge back upstream in that case:
//!
//! * `ResolvedValue::List` contains an iterator of results,
//!   in case an error happens during iteration.
//! * `Resolver::type_name` can return a `&str` not necessarily `&'static str`.
//! * Added the `response_extensions` parameter.

#[macro_use]
pub(crate) mod resolver;
pub(crate) mod engine;
pub(crate) mod input_coercion;
pub(crate) mod result_coercion;
pub(crate) mod validation;
