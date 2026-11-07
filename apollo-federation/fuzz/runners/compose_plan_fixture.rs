//! Composes the plan lane's fixture, `fixtures/supergraph.graphql`, from its subgraphs, so the
//! fixture has a visible source. Not part of any lane; run by hand when the fixture changes, and
//! mirror the change in `lean/QueryPlanCheckerOracle.lean`.
//!
//!     cargo run --release --example compose_plan_fixture > fixtures/supergraph.graphql
//!
//! It extends the subgraphs of `it_handles_multiple_requires_within_the_same_entity_fetch`:
//!
//! - `cs: [C!]!` beside `f` in the `@requires` field set, so an entry can nest and a rewrite path
//!   can run through `@`;
//! - `u: U`, where Subgraph1's `U` has `T1 | T2` and Subgraph2 adds `T3`, so a fetch can return
//!   fewer types than the supergraph admits there;
//! - `T2.peer: T3 @provides(fields: "f")` in Subgraph2, so a fetch selects an `@external` field
//!   inside a `@provides` window;
//! - Subgraph3 declaring `I` an interface object with a field `h`, which makes `I` and its
//!   implementations entities in Subgraph1.

use apollo_federation::composition::compose;
use apollo_federation::composition::CompositionOptions;
use apollo_federation::subgraph::typestate::Subgraph;

const SUBGRAPH1: &str = r#"
extend schema @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", "@external", "@shareable"])

type Query {
  is: [I!]!
}

interface I @key(fields: "id") {
  id: ID!
  f: Int
  cs: [C!]!
  g: Int
  u: U
}

type T1 implements I @key(fields: "id") {
  id: ID!
  f: Int
  cs: [C!]!
  g: Int
  u: U
}

type T2 implements I @key(fields: "id") {
  id: ID!
  f: Int!
  cs: [C!]!
  g: Int @external
  u: U
}

type T3 implements I @key(fields: "id") {
  id: ID!
  f: Int @shareable
  cs: [C!]!
  g: Int @external
  u: U
}

type C {
  a: Int
}

union U = T1 | T2
"#;

const SUBGRAPH2: &str = r#"
extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", "@external", "@requires", "@provides"])

type T2 @key(fields: "id") {
  id: ID!
  f: Int! @external
  cs: [C!]! @external
  g: Int @requires(fields: "f cs { ... on C { a } }")
  peer: T3 @provides(fields: "f")
}

type T3 @key(fields: "id") {
  id: ID!
  f: Int @external
  cs: [C!]! @external
  g: Int @requires(fields: "f cs { ... on C { a } }")
}

type C {
  a: Int @external
}

union U = T2 | T3
"#;

const SUBGRAPH3: &str = r#"
extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", "@interfaceObject"])

type I @key(fields: "id") @interfaceObject {
  id: ID!
  h: Int
}
"#;

fn main() {
    let subgraphs = [
        ("Subgraph1", SUBGRAPH1),
        ("Subgraph2", SUBGRAPH2),
        ("Subgraph3", SUBGRAPH3),
    ]
    .map(|(name, sdl)| {
        Subgraph::parse(name, &format!("http://{name}"), sdl)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    });
    let supergraph = compose(subgraphs.into(), CompositionOptions::default())
        .unwrap_or_else(|failure| panic!("compose: {failure:?}"));
    println!("{}", supergraph.schema().schema());
}
