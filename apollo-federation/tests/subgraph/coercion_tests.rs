use apollo_federation::subgraph::test_utils::build_and_validate;

#[test]
fn coerces_directive_argument_values() {
    // Test that directive argument values are coerced correctly.
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

        type Query {
            test: T!
        }

        type T @key(fields: ["id"]) {
            id: ID!
            x: Int!
        }
    "#;

    let _subgraph = build_and_validate(schema);
    // Success: schema validated after coercion
}

#[test]
fn coerces_field_argument_default_values() {
    // Test that field argument default values are coerced correctly.
    // The field argument expects String! but the default is a list ["id"]
    // which should be coerced to "id".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

        type Query {
            test: T!
        }

        type T @key(fields: "id") {
            id: ID!
            name(arg: String! = ["id"]): String!
            x: Int!
        }
    "#;

    let _subgraph = build_and_validate(schema);
    // Success: schema validated after coercion
}

#[test]
fn coerces_input_field_default_values() {
    // Test that input object field default values are coerced correctly.
    // - `name` has an enum-like default value `Anonymous` which should be coerced for custom scalars
    // - `age` expects Int but the default is a list [18] which should be coerced
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

        type Query {
            test(input: UserInput): String
        }

        input UserInput {
            name: String = Anonymous
            age: Int = [18]
        }
    "#;
    let _subgraph = build_and_validate(schema);
    // Success: schema validated after coercion
}
