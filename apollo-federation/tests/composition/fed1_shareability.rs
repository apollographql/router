use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;

#[test]
fn test_fed1_fields_are_implicitly_shareable() {
    let fed1_subgraph = r#"
        type Query {
            sharedField: String
            fed1OnlyField: Int
        }
    "#;
    let fed2_subgraph = r#"
        schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@shareable"]) {
            query: Query
        }

        type Query {
            sharedField: String @shareable
            fed2OnlyField: Boolean
        }
    "#;

    let subgraph1 =
        Subgraph::parse("fed1", "http://fed1", fed1_subgraph).expect("Fed 1 subgraph should parse");
    let subgraph2 =
        Subgraph::parse("fed2", "http://fed2", fed2_subgraph).expect("Fed 2 subgraph should parse");

    // This should succeed because Fed 1 fields are implicitly shareable
    let result = compose(vec![subgraph1, subgraph2]);
    assert!(
        result.is_ok(),
        "Composition should succeed when Fed 1 and Fed 2 subgraphs share fields. \
         Fed 1 fields are implicitly shareable. Error: {:?}",
        result.err()
    );
}

#[test]
fn test_fed1_with_custom_root_type_names() {
    let fed1_subgraph = r#"
        type Query {
            sharedField: String
        }
    "#;
    let fed2_subgraph = r#"
        schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@shareable"]) {
            query: RootQueryType
        }

        type RootQueryType {
            sharedField: String @shareable
        }
    "#;

    let subgraph1 =
        Subgraph::parse("fed1", "http://fed1", fed1_subgraph).expect("Fed 1 subgraph should parse");
    let subgraph2 =
        Subgraph::parse("fed2", "http://fed2", fed2_subgraph).expect("Fed 2 subgraph should parse");

    // This should succeed even though root types have different names
    // The upgrader should recognize that Query and RootQueryType are both query root types
    let result = compose(vec![subgraph1, subgraph2]);
    assert!(
        result.is_ok(),
        "Composition should succeed when Fed 1 uses 'Query' and Fed 2 uses 'RootQueryType'. \
         The upgrader should handle root type name differences. Error: {:?}",
        result.err()
    );
}

#[test]
fn test_fed1_non_root_types_are_shareable() {
    let fed1_subgraph = r#"
        type Query {
            product(id: ID!): Product
        }

        type Product {
            id: ID!
            name: String
            price: Float
        }
    "#;
    let fed2_subgraph = r#"
        schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@shareable"]) {
            query: Query
        }

        type Query {
            products: [Product]
        }

        type Product @key(fields: "id") @shareable {
            id: ID!
            name: String
            price: Float
        }
    "#;

    let subgraph1 =
        Subgraph::parse("fed1", "http://fed1", fed1_subgraph).expect("Fed 1 subgraph should parse");
    let subgraph2 =
        Subgraph::parse("fed2", "http://fed2", fed2_subgraph).expect("Fed 2 subgraph should parse");

    // This should succeed - Fed 1 types are implicitly shareable
    let result = compose(vec![subgraph1, subgraph2]);
    assert!(
        result.is_ok(),
        "Composition should succeed when Fed 1 and Fed 2 share non-root types. \
         Fed 1 types are implicitly shareable. Error: {:?}",
        result.err()
    );
}

#[test]
fn test_multiple_fed1_subgraphs_sharing_fields() {
    let fed1_subgraph_a = r#"
        type Query {
            sharedField: String
            fieldA: Int
        }
    "#;
    let fed1_subgraph_b = r#"
        type Query {
            sharedField: String
            fieldB: Boolean
        }
    "#;

    let subgraph1 = Subgraph::parse("fed1a", "http://fed1a", fed1_subgraph_a)
        .expect("Fed 1 subgraph A should parse");
    let subgraph2 = Subgraph::parse("fed1b", "http://fed1b", fed1_subgraph_b)
        .expect("Fed 1 subgraph B should parse");

    // This should succeed - all Fed 1 fields are implicitly shareable
    let result = compose(vec![subgraph1, subgraph2]);
    assert!(
        result.is_ok(),
        "Composition should succeed when multiple Fed 1 subgraphs share fields. \
         All Fed 1 fields are implicitly shareable. Error: {:?}",
        result.err()
    );
}
