//! Composes the checker's test supergraph from subgraph SDL, so the fixture checked in beside the
//! tests has a visible source. Not part of any lane; run by hand when the fixture changes.
//!
//!     cargo run --release --example compose_fixture > ../src/correctness/query_plan_check/testdata/entity_requires.graphql

use apollo_federation::subgraph::Subgraph;
use apollo_federation::Supergraph;

const LOCATIONS: &str = r#"
type Query {
  locations: [Location!]!
  location(id: ID!): Location
  feed: [Item!]!
}

type Location @key(fields: "id") {
  id: ID!
  name: String!
  reviewsForLocation: [Review]! @external
  "A @requires whose field set wraps its body in a type condition that is already the field's own type."
  reviews: [Review!]! @requires(fields: "reviewsForLocation { ... on Review { id rating } }")
}

type Review @key(fields: "id", resolvable: false) {
  id: ID!
  rating: Int @external
}

"""
Two entity types under one interface whose `@key` field sets name different fields. An entity
fetch over both gets one `requires` entry per case, and the checker tries every entry against
every case -- so it must decide pairs whose `@key` does not typecheck at the entry's type.
"""
interface Item {
  id: ID!
}

type Book implements Item @key(fields: "id") {
  id: ID!
  isbn: String!
}

type Film implements Item @key(fields: "code") {
  id: ID!
  code: String!
  minutes: Int!
}
"#;

const REVIEWS: &str = r#"
type Query {
  latestReviews: [Review!]!
}

type Location @key(fields: "id") {
  id: ID!
  overallRating: Float
  reviewsForLocation: [Review]!
}

type Review @key(fields: "id") {
  id: ID!
  comment: String
  rating: Int
  location: Location
}

interface Item {
  id: ID!
}

type Book implements Item @key(fields: "id") {
  id: ID!
  isbn: String! @external
  "A `@requires` naming a field the other subgraph owns, on an entity keyed by `id`."
  blurb: String! @requires(fields: "isbn")
}

type Film implements Item @key(fields: "code") {
  id: ID!
  code: String!
  minutes: Int! @external
  "The same, on an entity keyed by a different field -- `code`, which `Book` does not declare."
  summary: String! @requires(fields: "minutes")
}
"#;

fn main() {
    let locations = Subgraph::parse_and_expand("locations", "http://locations", LOCATIONS)
        .expect("locations subgraph");
    let reviews =
        Subgraph::parse_and_expand("reviews", "http://reviews", REVIEWS).expect("reviews subgraph");
    let supergraph = Supergraph::compose(vec![&locations, &reviews]).expect("compose");
    println!("{}", supergraph.schema.schema());
}
