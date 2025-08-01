use apollo_federation::error::CompositionError;
use apollo_federation::supergraph::Supergraph;

use crate::composition::{ServiceDefinition, compose_as_fed2_subgraphs};

fn error_messages<S>(result: &Result<Supergraph<S>, Vec<CompositionError>>) -> Vec<String> {
    match result {
        Ok(_) => panic!("Expected an error, but got a successful composition"),
        Err(err) => err.iter().map(|e| e.to_string()).collect(),
    }
}

#[test]
fn external_type_mismatch() {
    let subgraph_a = ServiceDefinition {
        name: "SubgraphA",
        type_defs: r#"
            type Query {
                t: T
            }

            type T @key(fields: "id") {
                id: ID!
                f: String @external
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "SubgraphB",
        type_defs: r#"
            type T @key(fields: "id") {
                id: ID!
                f: Int @shareable
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let messages = error_messages(&result);
    
    // Verify that composition fails due to type mismatch between @external field and actual field
    // Should produce EXTERNAL_TYPE_MISMATCH error
    assert!(messages.iter().any(|msg| 
        msg.contains("EXTERNAL_TYPE_MISMATCH") ||
        msg.contains("type") && msg.contains("incompatible") ||
        msg.contains("String") && msg.contains("Int")
    ), "Expected EXTERNAL_TYPE_MISMATCH error, got: {:?}", messages);
}

#[test]
fn external_argument_missing() {
    let subgraph_a = ServiceDefinition {
        name: "SubgraphA",
        type_defs: r#"
            type Query {
                t: T
            }

            type T @key(fields: "id") {
                id: ID!
                f: String @external
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "SubgraphB",
        type_defs: r#"
            type T @key(fields: "id") {
                id: ID!
                f(x: Int!): String @shareable
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let messages = error_messages(&result);
    
    // Verify that composition fails due to missing arguments in @external field declaration
    // Should produce EXTERNAL_ARGUMENT_MISSING error
    assert!(messages.iter().any(|msg| 
        msg.contains("EXTERNAL_ARGUMENT_MISSING") ||
        msg.contains("argument") && msg.contains("missing") ||
        msg.contains("external") && msg.contains("argument")
    ), "Expected EXTERNAL_ARGUMENT_MISSING error, got: {:?}", messages);
}

#[test]
fn external_argument_type_mismatch() {
    let subgraph_a = ServiceDefinition {
        name: "SubgraphA",
        type_defs: r#"
            type Query {
                t: T
            }

            type T @key(fields: "id") {
                id: ID!
                f(x: String!): String @external
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "SubgraphB",
        type_defs: r#"
            type T @key(fields: "id") {
                id: ID!
                f(x: Int!): String @shareable
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let messages = error_messages(&result);
    
    // Verify that composition fails due to argument type mismatch in @external field
    // Should produce EXTERNAL_ARGUMENT_TYPE_MISMATCH error
    assert!(messages.iter().any(|msg| 
        msg.contains("EXTERNAL_ARGUMENT_TYPE_MISMATCH") ||
        msg.contains("argument") && msg.contains("type") && msg.contains("mismatch") ||
        msg.contains("String") && msg.contains("Int")
    ), "Expected EXTERNAL_ARGUMENT_TYPE_MISMATCH error, got: {:?}", messages);
}

#[test]
fn external_on_type_success() {
    let subgraph_a = ServiceDefinition {
        name: "SubgraphA",
        type_defs: r#"
            type Query {
                products: [Product!]
            }

            type Product @key(fields: "sku") {
                sku: String!
                name: String! @external
                description: String! @requires(fields: "name")
            }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "SubgraphB",
        type_defs: r#"
            type Product @key(fields: "sku") {
                sku: String!
                name: String! @shareable
                price: Float!
            }
        "#,
    };

    // This should succeed - valid @external usage with @requires
    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert!(result.is_ok(), "Expected successful composition, got error: {:?}", 
        result.err().map(|errors| errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()));
    
    // Optionally verify the composed schema
    if let Ok(supergraph) = result {
        let api_schema = supergraph.to_api_schema(Default::default()).unwrap();
        let schema_sdl = api_schema.schema().to_string();
        
        // Verify that the API schema contains the expected types and fields
        assert!(schema_sdl.contains("type Product"));
        assert!(schema_sdl.contains("sku: String!"));
        assert!(schema_sdl.contains("name: String!"));
        assert!(schema_sdl.contains("description: String!"));
        assert!(schema_sdl.contains("price: Float!"));
        
        // Verify that federation directives are removed from API schema
        assert!(!schema_sdl.contains("@external"));
        assert!(!schema_sdl.contains("@requires"));
        assert!(!schema_sdl.contains("@key"));
        assert!(!schema_sdl.contains("@shareable"));
    }
}