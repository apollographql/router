//! Layer 1: federated subgraph SDL generator.
//!
//! Phase A scope: this module is a stub. Phase C will fill in the
//! constructive generator described in the plan: produce a canonical type
//! universe with apollo-smith, then project entities (with @key) and shared
//! value types (with @shareable) across N subgraphs.
//!
//! For now we expose a hand-rolled fixture builder so the diff harness can
//! be exercised end-to-end against a known-good two-subgraph composition.

/// A named subgraph SDL pair, suitable as input to [`crate::compose`].
///
/// SDL is emitted *without* a `@link` to the federation spec — the compose
/// layer calls `Subgraph::into_fed2_test_subgraph(true)` which injects the
/// standard fed2 link with all directive imports. Pre-injecting it would
/// cause `InvalidLinkDirectiveUsage: duplicate @link inclusion`.
#[derive(Debug, Clone)]
pub struct SubgraphSdl {
    pub name: String,
    pub sdl: String,
}

impl SubgraphSdl {
    pub fn new(name: impl Into<String>, body: impl AsRef<str>) -> Self {
        Self {
            name: name.into(),
            sdl: body.as_ref().to_string(),
        }
    }
}

/// A trivial two-subgraph fixture used for the Phase-A smoke test.
/// Demonstrates an entity (`User`) shared across subgraphs via `@key`.
pub fn smoke_test_fixture() -> Vec<SubgraphSdl> {
    let users = SubgraphSdl::new(
        "users",
        r#"
            type Query {
              me: User
            }

            type User @key(fields: "id") {
              id: ID!
              name: String!
            }
        "#,
    );

    let reviews = SubgraphSdl::new(
        "reviews",
        r#"
            type Query {
              latestReview: Review
            }

            type Review @key(fields: "id") {
              id: ID!
              body: String!
              author: User!
            }

            type User @key(fields: "id") {
              id: ID!
              reviews: [Review!]!
            }
        "#,
    );

    vec![users, reviews]
}
