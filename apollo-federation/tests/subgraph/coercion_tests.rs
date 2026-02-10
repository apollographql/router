use apollo_federation::subgraph::test_utils::build_and_validate;

#[test]
fn coerces_directive_argument_values() {
    // Test that directive argument values are coerced correctly.
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

        type Query {
            test: T!
        }

        type T @key(fields: "id") {
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

#[test]
fn coerces_enum_value_to_non_null_string_on_custom_directive() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @myDirective(arg: String!) on FIELD_DEFINITION

        type Query {
            test: T!
        }

        interface T {
            id: ID! @myDirective(arg: MyEnum)
            x: Int!
        }
    "#;

    let _subgraph = build_and_validate(schema);
    // Success: schema validated after coercion
}

#[test]
fn coerces_enum_literal_to_string_on_union_directive() {
    // Test that enum literal values are coerced to strings for union type directives.
    // The directive expects String! but receives an enum literal Searchable
    // which should be coerced to "Searchable".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @metadata(tag: String!) on UNION

        type Query {
            search: SearchResult
        }

        type Book {
            title: String!
        }

        type Author {
            name: String!
        }

        union SearchResult @metadata(tag: Searchable) = Book | Author
    "#;

    let _subgraph = build_and_validate(schema);
    // Success: schema validated after coercion
}

#[test]
fn coerces_enum_literal_to_string_on_scalar_directive() {
    // Test that enum literal values are coerced to strings for scalar type directives.
    // The directive expects String! but receives an enum literal ISO8601
    // which should be coerced to "ISO8601".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @format(type: String!) on SCALAR

        type Query {
            data: JSON
        }

        scalar JSON @format(type: ISO8601)
    "#;

    let _subgraph = build_and_validate(schema);
    // Success: schema validated after coercion
}

#[test]
fn coerces_enum_literal_to_string_on_enum_type_directive() {
    // Test that enum literal values are coerced to strings for enum type directives.
    // The directive expects String! but receives an enum literal StatusType
    // which should be coerced to "StatusType".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @metadata(category: String!) on ENUM

        type Query {
            status: Status
        }

        enum Status @metadata(category: StatusType) {
            ACTIVE
            INACTIVE
        }
    "#;

    let _subgraph = build_and_validate(schema);
    // Success: schema validated after coercion
}

#[test]
fn coerces_enum_literal_to_string_on_enum_value_directive() {
    // Test that enum literal values are coerced to strings for enum value directives.
    // The directive expects String! but receives an enum literal Important
    // which should be coerced to "Important".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @alias(name: String!) on ENUM_VALUE

        type Query {
            priority: Priority
        }

        enum Priority {
            HIGH @alias(name: Important)
            MEDIUM
            LOW
        }
    "#;

    let _subgraph = build_and_validate(schema);
    // Success: schema validated after coercion
}
