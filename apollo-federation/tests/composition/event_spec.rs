use apollo_federation::subgraph::typestate::Subgraph;

use super::compose;

#[test]
fn composes_event_subscribe_through_join_directive() {
    let events = Subgraph::parse(
        "events",
        "http://events",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.10")
          @link(url: "https://specs.apollo.dev/event/v0.1", import: ["@subscribe"])

        type Query { ready: Boolean }
        type Subscription {
          productUpdated: Product
            @subscribe(source: "product-updates", destinations: ["products"])
        }
        type Product { id: ID! }
        "#,
    )
    .expect("event subgraph parses");

    let supergraph = compose(vec![events]).expect("event subgraph composes");
    let schema = supergraph.schema().schema().to_string();
    assert!(schema.contains("https://specs.apollo.dev/event/v0.1"));
    assert!(schema.contains(
        r#"@join__directive(name: "link", graphs: [EVENTS], args: {url: "https://specs.apollo.dev/event/v0.1", import: ["@subscribe"]})"#
    ));
    assert!(schema.contains("name: \"subscribe\""));
    assert!(schema.contains("source: \"product-updates\""));
    assert!(schema.contains("destinations: [\"products\"]"));
}
