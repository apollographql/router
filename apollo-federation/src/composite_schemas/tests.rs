//! The `graphql_federation` gate. The crate's tests build with the feature on, so these tests
//! close the gate with [`super::GRAPHQL_FEDERATION_OVERRIDE`]; the rest of the GraphQL Federation
//! composition tests live in `tests/composition/composite_schemas.rs`.

use crate::composition::CompositionOptions;
use crate::composition::compose;
use crate::subgraph::typestate::Subgraph;

/// Closes the gate on this thread until dropped.
struct GateClosed;

impl GateClosed {
    fn new() -> Self {
        super::GRAPHQL_FEDERATION_OVERRIDE.set(Some(false));
        Self
    }
}

impl Drop for GateClosed {
    fn drop(&mut self) {
        super::GRAPHQL_FEDERATION_OVERRIDE.set(None);
    }
}

/// The error codes of composing `sources` with the gate closed, or `None` if it composes.
fn codes_with_gate_closed(sources: &[(&str, &str)]) -> Option<Vec<String>> {
    let _gate = GateClosed::new();
    let subgraphs = sources
        .iter()
        .map(|(name, sdl)| Subgraph::parse(name, &format!("http://{name}"), sdl).expect("parses"))
        .collect();
    compose(subgraphs, CompositionOptions::default())
        .err()
        .map(|failure| {
            failure
                .errors
                .iter()
                .map(|e| e.code().definition().code().to_string())
                .collect()
        })
}

#[test]
fn source_schema_is_rejected_when_the_gate_is_closed() {
    let codes = codes_with_gate_closed(&[(
        "products",
        r#"
        type Query { productById(id: ID!): Product @lookup }
        type Product @key(fields: "id") { id: ID! name: String! }
        "#,
    )]);
    assert_eq!(
        codes.as_deref(),
        Some(&["GRAPHQL_FEDERATION_NOT_ENABLED".to_string()][..])
    );
}

#[test]
fn linked_subgraph_applying_require_is_rejected_when_the_gate_is_closed() {
    let codes = codes_with_gate_closed(&[
        (
            "a",
            r#"
            extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key", "@require"])
            type Query { t: T }
            type T @key(fields: "id") {
              id: ID!
              f(x: Int @require(field: "x")): Int
            }
            "#,
        ),
        (
            "b",
            r#"
            extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key"])
            type Query { u: T }
            type T @key(fields: "id") { id: ID! x: Int }
            "#,
        ),
    ]);
    assert_eq!(
        codes.as_deref(),
        Some(&["GRAPHQL_FEDERATION_NOT_ENABLED".to_string()][..])
    );
}

#[test]
fn federation_subgraphs_compose_when_the_gate_is_closed() {
    let codes = codes_with_gate_closed(&[(
        "a",
        r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key"])
        type Query { t: T }
        type T @key(fields: "id") { id: ID! }
        "#,
    )]);
    assert_eq!(codes, None);
}
