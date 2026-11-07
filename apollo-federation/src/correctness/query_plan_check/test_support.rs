//! What the checker's unit tests share: a buffer of what a plan has fetched, written as an
//! operation, and a subgraph whose operations can descend through an interface without end.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::Selection;

use crate::schema::ValidFederationSchema;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;

/// The root selections of an operation against `schema`, which is how a test writes what a plan
/// has fetched: the checker reads that rooted at the query type.
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

/// The selections under the first field named `key`, at any depth.
pub(super) fn under(selections: &[Selection], key: &str) -> Vec<Selection> {
    for selection in selections {
        let nested = match selection {
            Selection::Field(field) if field.response_name() == key => {
                return field.selection_set.selections.clone();
            }
            Selection::Field(field) => &field.selection_set.selections,
            Selection::InlineFragment(fragment) => &fragment.selection_set.selections,
            Selection::FragmentSpread(_) => continue,
        };
        let found = under(nested, key);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
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
