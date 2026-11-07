//! What the checker's unit tests share: selections written as an operation, and a subgraph whose
//! operations can descend through an interface without end.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::Selection;

use crate::schema::ValidFederationSchema;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;

/// The root selections of an operation against `schema`, which is how a test writes what a fetch
/// before the one under test selected at the query root.
pub(super) fn selections(schema: &ValidFederationSchema, operation: &str) -> Vec<Selection> {
    ExecutableDocument::parse_and_validate(schema.schema(), operation, "op.graphql")
        .unwrap()
        .operations
        .get(None)
        .unwrap()
        .selection_set
        .selections
        .clone()
}

/// A subgraph whose `root` is a `Recursive`, an interface with four implementations each returning
/// another `Recursive`.
pub(super) fn recursive_subgraph() -> Subgraph<Validated> {
    Subgraph::parse(
        "recursive",
        "http://recursive",
        r#"
            extend schema @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@key"])

            type Query { root: Recursive }
            interface Recursive { next: Recursive, leaf: String }
            type First implements Recursive { next: Recursive, leaf: String }
            type Second implements Recursive { next: Recursive, leaf: String }
            type Third implements Recursive { next: Recursive, leaf: String }
            type Fourth implements Recursive { next: Recursive, leaf: String }
        "#,
    )
    .expect("valid subgraph")
    .expand_links()
    .expect("links expand")
    .validate()
    .expect("valid subgraph schema")
}

/// `{ root { next { … { leaf } } } }`, `depth` levels of `next` through the interface.
pub(super) fn recursive_chain(schema: &ValidFederationSchema, depth: usize) -> ExecutableDocument {
    let operation = format!(
        "{{ root {}{{ leaf }}{} }}",
        "{ next ".repeat(depth),
        " }".repeat(depth)
    );
    ExecutableDocument::parse_and_validate(schema.schema(), operation, "op.graphql")
        .expect("valid operation")
        .into_inner()
}
