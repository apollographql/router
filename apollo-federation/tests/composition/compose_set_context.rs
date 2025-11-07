use test_log::test;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;

#[test]
fn vanilla_setcontext_success_case() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let supergraph = result.expect("Expected composition to succeed");
    assert!(
        !supergraph.schema().schema().types.is_empty(),
        "Supergraph should contain types"
    );
}

#[test]
fn using_a_list_as_input_to_fromcontext() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: [String]!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: [String] @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let supergraph = result.expect("Expected composition to succeed");
    assert!(
        !supergraph.schema().schema().types.is_empty(),
        "Supergraph should contain types"
    );
}

#[test]
fn invalid_context_name_shouldnt_throw() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context name \"\" is invalid. It should have only alphanumeric characters.",
        "Expected error message about invalid context name, but got: {}",
        error_message
    );
}

#[test]
fn forbid_default_values_on_contextual_arguments() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(
            a: String = "default" @fromContext(field: "$context { prop }")
          ): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] @fromContext arguments may not have a default value: \"U.field(a:)\".",
        "Expected error message about forbidden default values on contextual arguments, but got: {}",
        error_message
    );
}

#[test]
fn forbid_contextual_arguments_on_interfaces() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        interface I @key(fields: "id") {
          id: ID!
          field: Int!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U implements I @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] @fromContext argument cannot be used on a field implementing an interface field \"I.field\".",
        "Expected error message about forbidden contextual arguments on interfaces, but got: {}",
        error_message
    );
}

#[test]
fn contextual_argument_on_directive_definition_argument() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        directive @foo(
          a: String @fromContext(field: "$context { prop }")
        ) on FIELD_DEFINITION

        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field: Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] @fromContext argument cannot be used on a directive definition argument \"@foo(a:)\".",
        "Expected error message about forbidden contextual arguments on directive definition arguments, but got: {}",
        error_message
    );
}

#[test]
fn contextual_argument_is_present_in_multiple_subgraphs_default_value() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
            @shareable
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String! = "default"): Int! @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn contextual_argument_is_present_in_multiple_subgraphs_nullable() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
            @shareable
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String): Int! @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn contextual_argument_is_present_in_multiple_subgraphs_not_nullable_no_default() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
            @shareable
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String!): Int! @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "Argument \"U.field(a:)\" is contextual in at least one subgraph but in \"U.field(a:)\" it does not have @fromContext, is not nullable and has no default value.",
        "Expected error message about incompatible non-nullable contextual arguments, but got: {}",
        error_message
    );
}

#[test]
fn contextual_argument_is_present_in_multiple_subgraphs_success_case() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
            @shareable
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
          field: Int! @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn context_selection_references_interface_object() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        directive @foo on FIELD
        type Query {
          t: T!
        }

        type T @interfaceObject @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid: One of the types in the selection is an interfaceObject: \"T\".",
        "Expected error message about invalid context selection referencing interfaceObject, but got: {}",
        error_message
    );
}

#[test]
fn context_selection_contains_query_directive() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        directive @foo on FIELD
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop @foo }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid: directives are not allowed in the selection",
        "Expected error message about invalid context selection containing directives, but got: {}",
        error_message
    );
}

#[test]
fn context_name_is_invalid() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "_context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$_context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 2, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message, "[Subgraph1] Context name \"_context\" may not contain an underscore.",
        "Expected error message about invalid context name with underscore, but got: {}",
        error_message
    );
}

#[test]
fn context_selection_contains_an_alias() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { foo: prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid: aliases are not allowed in the selection",
        "Expected error message about invalid context selection containing aliases, but got: {}",
        error_message
    );
}

#[test]
// Since it's possible that we have to call into the same subgraph with multiple fetch groups where we would have previously used only one,
// we need to verify that there is a resolvable key on the object that uses a context.
fn at_least_one_key_on_object_with_context_must_be_resolvable() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id", resolvable: false) {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Object \"U\" has no resolvable key but has a field with a contextual argument.",
        "Expected error message about object with no resolvable key having contextual arguments, but got: {}",
        error_message
    );
}

#[test]
fn fields_marked_external_because_of_context_not_flagged_as_unused() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String! @external
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type T @key(fields: "id") {
          id: ID!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn selection_contains_more_than_one_value() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { id prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid: multiple selections are made",
        "Expected error message about invalid context selection with multiple fields, but got: {}",
        error_message
    );
}

#[test]
fn nullability_mismatch_is_ok_if_contextual_value_non_nullable() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };
    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn context_fails_on_union_when_type_is_missing_prop() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        union T @context(name: "context") = T1 | T2

        type T1 @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
          a: String!
        }

        type T2 @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          b: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid for type \"T2\".",
        "Expected error message about invalid context selection on union type, but got: {}",
        error_message
    );
}

#[test]
fn setcontext_on_interface_with_type_condition_success() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          i: I!
        }

        interface I @context(name: "context") {
          prop: String!
        }

        type T implements I @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(
            a: String @fromContext(field: "$context ... on T { prop }")
          ): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn setcontext_on_interface_success() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          i: I!
        }

        interface I @context(name: "context") {
          prop: String!
        }

        type T implements I @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn type_matches_no_type_conditions() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          bar: Bar!
        }

        type Foo @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type Bar @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop2: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(
            a: String @fromContext(field: "$context ... on Foo { prop }")
          ): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid: no type condition matches the location \"Bar\"",
        "Expected error message about type condition not matching context type, but got: {}",
        error_message
    );
}

#[test]
fn context_variable_does_not_appear_in_selection() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "{ prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(
        errors.len(),
        1,
        "Expected exactly 1 error but got {:?} error",
        errors
    );

    let error_message = errors[0].to_string();

    assert_eq!(
        error_message,
        "[Subgraph1] @fromContext argument does not reference a context \"{ prop }\".",
        "Expected error message about missing context variable in selection, but got: {}",
        error_message
    );
}

#[test]
fn resolved_field_is_not_available_in_context() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(
            a: String @fromContext(field: "$context { invalidprop }")
          ): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    println!("error_message: {}", error_message);
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid for type \"T\".",
        "Expected error message about field not available in context, but got: {}",
        error_message
    );
}

#[test]
fn context_is_never_set() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$unknown { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"unknown\" is used at location \"U.field(a:)\" but is never set.",
        "Expected error message about context never being set, but got: {}",
        error_message
    );
}

#[test]
fn setcontext_with_multiple_contexts_type_conditions_success() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          foo: Foo!
          bar: Bar!
        }

        type Foo @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type Bar @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop2: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(
            a: String
              @fromContext(
                field: "$context ... on Foo { prop } ... on Bar { prop2 }"
              )
          ): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn setcontext_with_multiple_contexts_duck_typing_type_mismatch() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          foo: Foo!
          bar: Bar!
        }

        type Foo @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type Bar @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: Int!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid: the type of the selection \"Int\" does not match the expected type \"String\"",
        "Expected error message about type mismatch in context selection, but got: {}",
        error_message
    );
}

#[test]
fn setcontext_with_multiple_contexts_duck_typing_success() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          foo: Foo!
          bar: Bar!
        }

        type Foo @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type Bar @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String!
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    assert!(result.is_ok(), "Expected composition to succeed");
}

#[test]
fn nullability_mismatch_is_not_ok_if_argument_is_non_nullable() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          t: T!
        }

        type T @key(fields: "id") @context(name: "context") {
          id: ID!
          u: U!
          prop: String
        }

        type U @key(fields: "id") {
          id: ID!
          field(a: String! @fromContext(field: "$context { prop }")): Int!
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        type Query {
          a: Int!
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);

    let errors = result.expect_err("Expected composition to fail");
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");

    let error_message = errors[0].to_string();
    assert_eq!(
        error_message,
        "[Subgraph1] Context \"context\" is used in \"U.field(a:)\" but the selection is invalid: the type of the selection \"String\" does not match the expected type \"String!\"",
        "Expected error message about type mismatch, but got: {}",
        error_message
    );
}
