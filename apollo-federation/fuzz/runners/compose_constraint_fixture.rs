//! Composes the supergraph for the constraint-deduplication test.
//!
//!     cargo run --release --example compose_constraint_fixture \
//!         > ../src/correctness/query_compare/testdata/constraint_state.graphql
//!
//! `P.f` is resolvable only in subgraph A and `Q.f` only in B, and both return `R`. The two child
//! tasks therefore agree on the immediate region `{R}` while disagreeing on which subgraphs remain
//! possible. `R.g` returns an interface whose implementations differ per subgraph -- `X` in A, `Y`
//! in B -- so that disagreement changes what the *next* level admits.

use apollo_federation::subgraph::Subgraph;
use apollo_federation::Supergraph;

const A: &str = r#"
extend schema @link(url: "https://specs.apollo.dev/federation/v2.5", import: ["@key", "@shareable"])

type Query {
  items: [I!]! @shareable
}

interface I {
  id: ID!
  f: R!
}

type P implements I @key(fields: "id") {
  id: ID!
  f: R!
}

type R @key(fields: "id") {
  id: ID!
  g: G! @shareable
}

interface G {
  id: ID!
}

type X implements G @key(fields: "id") {
  id: ID!
  x: Int!
}
"#;

const B: &str = r#"
extend schema @link(url: "https://specs.apollo.dev/federation/v2.5", import: ["@key", "@shareable"])

type Query {
  items: [I!]! @shareable
}

interface I {
  id: ID!
  f: R!
}

type Q implements I @key(fields: "id") {
  id: ID!
  f: R!
}

type R @key(fields: "id", resolvable: false) {
  id: ID!
  g: G! @shareable
}

interface G {
  id: ID!
}

type Y implements G @key(fields: "id") {
  id: ID!
  y: Int!
}
"#;

fn main() {
    let a = Subgraph::parse_and_expand("a", "http://a", A).expect("subgraph a");
    let b = Subgraph::parse_and_expand("b", "http://b", B).expect("subgraph b");
    let supergraph = Supergraph::compose(vec![&a, &b]).expect("compose");
    println!("{}", supergraph.schema.schema());
}
