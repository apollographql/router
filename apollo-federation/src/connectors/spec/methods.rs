//! The `methods:` argument, shared by `@source` and `@connect` (connect/v0.5+).
//!
//! # Colocation, not scoping
//!
//! Both spellings feed **one** subgraph-wide registry of custom `->` methods.
//! Declaring a method on a `@connect` is purely *colocation*: it lets an author
//! keep a mapping next to the connector that motivated it. It does **not**
//! scope the method to that connector.
//!
//! Concretely, a method declared on any one `@connect` is:
//! - callable from every selection in the subgraph, including other
//!   connectors' and `@source`-level selections, and
//! - subject to the single global name namespace, so it collides with a
//!   same-named method declared on any other `@connect`, on a `@source`, or
//!   derived from a `@method`.
//!
//! Keeping a method near its only caller is therefore a convention for human
//! readers, not an invariant the composer enforces. If per-connector *private*
//! methods are ever wanted, that is a separate feature with its own name
//! resolution rules — not something `@connect(methods:)` already provides.

use apollo_compiler::Name;
use apollo_compiler::ast::Value;
use apollo_compiler::name;

use crate::connectors::CompiledMethod;
use crate::connectors::ConnectSpec;
use crate::error::FederationError;

/// The `methods` argument on `@source` and `@connect`.
pub(crate) const METHODS_ARGUMENT_NAME: Name = name!("methods");

/// Compile a `methods:` object literal into [`CompiledMethod`]s.
///
/// This is the strict, extraction-time path: anything malformed becomes an
/// internal error, because validation has already reported it to the user in
/// friendlier terms. [`crate::connectors::validation::methods`] holds the lenient
/// counterpart that produces user-facing messages.
pub(crate) fn compile_methods_argument(
    value: &Value,
    directive_name: &Name,
    spec: ConnectSpec,
) -> Result<Vec<CompiledMethod>, FederationError> {
    let methods_object = value.as_object().ok_or_else(|| {
        FederationError::internal(format!(
            "`methods` field in `@{directive_name}` directive is not an object"
        ))
    })?;

    methods_object
        .iter()
        .map(|(method_name, method_value)| {
            // v1: string-only leaves; object leaves are reserved for future
            // long-form config.
            let body = method_value.as_str().ok_or_else(|| {
                FederationError::internal(format!(
                    "`methods.{method_name}` in `@{directive_name}` directive must be a string"
                ))
            })?;
            CompiledMethod::parse(method_name.as_str(), body, spec).map_err(|e| {
                FederationError::internal(format!(
                    "`methods.{method_name}` in `@{directive_name}` directive failed to parse: {}",
                    e.message
                ))
            })
        })
        .collect()
}
