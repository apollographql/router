use apollo_federation::composition::compose;
use apollo_federation::error::CompositionError;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::supergraph::Satisfiable;
use apollo_federation::supergraph::Supergraph;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::extract_subgraphs_from_supergraph_result;

fn compose_fed1_subgraphs(
    service_list: &[ServiceDefinition<'_>],
) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
    let mut subgraphs = Vec::new();
    let mut errors = Vec::new();
    for service in service_list {
        let result = Subgraph::parse(
            service.name,
            &format!("http://{}", service.name),
            service.type_defs,
        );
        match result {
            Ok(subgraph) => {
                subgraphs.push(subgraph);
            }
            Err(err) => {
                errors.extend(err.to_composition_errors());
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    compose(subgraphs)
}

mod basic_type_extensions {
    use super::*;
    use insta::assert_snapshot;

    #[test]
    fn works_when_extension_subgraph_is_second() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    products: [Product!]
                }

                type Product @key(fields: "sku") {
                    sku: String!
                    name: String!
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                extend type Product @key(fields: "sku") {
                    sku: String! @external
                    price: Int!
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = result.expect("Expected composition to succeed");

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        assert_snapshot!(api_schema.schema().to_string(), @r#"
        type Product {
          sku: String!
          name: String!
          price: Int!
        }

        type Query {
          products: [Product!]
        }
        "#);

        let subgraphs = extract_subgraphs_from_supergraph_result(&supergraph)
            .expect("Expected subgraph extraction to succeed");

        let subgraph_a_extracted = subgraphs
            .get("subgraphA")
            .expect("Expected subgraphA to be present");
        assert_snapshot!(subgraph_a_extracted.schema.schema().to_string(), @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject", "@authenticated", "@requiresScopes", "@policy", "@sourceAPI", "@sourceType", "@sourceField", "@context", "@fromContext", "@cost", "@listSize"]) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @extends on OBJECT | INTERFACE

        directive @shareable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!, label: String) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @sourceAPI repeatable on SCHEMA

        directive @sourceType repeatable on OBJECT | INTERFACE

        directive @sourceField repeatable on FIELD_DEFINITION

        directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        type Product @key(fields: "sku") {
          sku: String! @shareable
          name: String!
        }

        type Query {
          products: [Product!]
        }
        "#);

        let subgraph_b_extracted = subgraphs
            .get("subgraphB")
            .expect("Expected subgraphB to be present");
        assert_snapshot!(subgraph_b_extracted.schema.schema().to_string(), @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject", "@authenticated", "@requiresScopes", "@policy", "@sourceAPI", "@sourceType", "@sourceField", "@context", "@fromContext", "@cost", "@listSize"]) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @extends on OBJECT | INTERFACE

        directive @shareable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!, label: String) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @sourceAPI repeatable on SCHEMA

        directive @sourceType repeatable on OBJECT | INTERFACE

        directive @sourceField repeatable on FIELD_DEFINITION

        directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        type Product @key(fields: "sku") {
          sku: String! @shareable
          price: Int!
        }
        "#);
    }

    #[test]
    fn works_when_extension_subgraph_is_first() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                extend type Product @key(fields: "sku") {
                    sku: String! @external
                    price: Int!
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type Query {
                    products: [Product!]
                }

                type Product @key(fields: "sku") {
                    sku: String!
                    name: String!
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = result.expect("Expected composition to succeed");

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        assert_snapshot!(api_schema.schema().to_string(), @r#"
        type Product {
          sku: String!
          price: Int!
          name: String!
        }

        type Query {
          products: [Product!]
        }
        "#);

        let subgraphs = extract_subgraphs_from_supergraph_result(&supergraph)
            .expect("Expected subgraph extraction to succeed");

        let subgraph_a_extracted = subgraphs
            .get("subgraphA")
            .expect("Expected subgraphA to be present");
        assert_snapshot!(subgraph_a_extracted.schema.schema().to_string(), @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject", "@authenticated", "@requiresScopes", "@policy", "@sourceAPI", "@sourceType", "@sourceField", "@context", "@fromContext", "@cost", "@listSize"]) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @extends on OBJECT | INTERFACE

        directive @shareable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!, label: String) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @sourceAPI repeatable on SCHEMA

        directive @sourceType repeatable on OBJECT | INTERFACE

        directive @sourceField repeatable on FIELD_DEFINITION

        directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        type Product @key(fields: "sku") {
          sku: String! @shareable
          price: Int!
        }
        "#);

        let subgraph_b_extracted = subgraphs
            .get("subgraphB")
            .expect("Expected subgraphB to be present");
        assert_snapshot!(subgraph_b_extracted.schema.schema().to_string(), @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject", "@authenticated", "@requiresScopes", "@policy", "@sourceAPI", "@sourceType", "@sourceField", "@context", "@fromContext", "@cost", "@listSize"]) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @extends on OBJECT | INTERFACE

        directive @shareable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!, label: String) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @sourceAPI repeatable on SCHEMA

        directive @sourceType repeatable on OBJECT | INTERFACE

        directive @sourceField repeatable on FIELD_DEFINITION

        directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        type Product @key(fields: "sku") {
          sku: String! @shareable
          name: String!
        }

        type Query {
          products: [Product!]
        }
        "#);
    }

    #[test]
    fn works_with_multiple_extensions_on_the_same_type() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                extend type Product @key(fields: "sku") {
                    sku: String!
                    price: Int!
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type Query {
                    products: [Product!]
                }

                type Product {
                    sku: String!
                    name: String!
                }
            "#,
        };

        let subgraph_c = ServiceDefinition {
            name: "subgraphC",
            type_defs: r#"
                extend type Product @key(fields: "sku") {
                    sku: String!
                    color: String!
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
        let supergraph = result.expect("Expected composition to succeed");

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        assert_snapshot!(api_schema.schema().to_string(), @r#"
        type Product {
          sku: String!
          price: Int!
          name: String!
          color: String!
        }

        type Query {
          products: [Product!]
        }
        "#);

        let subgraphs = extract_subgraphs_from_supergraph_result(&supergraph)
            .expect("Expected subgraph extraction to succeed");

        let subgraph_a_extracted = subgraphs
            .get("subgraphA")
            .expect("Expected subgraphA to be present");
        assert_snapshot!(subgraph_a_extracted.schema.schema().to_string(), @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject", "@authenticated", "@requiresScopes", "@policy", "@sourceAPI", "@sourceType", "@sourceField", "@context", "@fromContext", "@cost", "@listSize"]) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @extends on OBJECT | INTERFACE

        directive @shareable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!, label: String) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @sourceAPI repeatable on SCHEMA

        directive @sourceType repeatable on OBJECT | INTERFACE

        directive @sourceField repeatable on FIELD_DEFINITION

        directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        type Product @key(fields: "sku") {
          sku: String! @shareable
          price: Int!
        }
        "#);

        let subgraph_b_extracted = subgraphs
            .get("subgraphB")
            .expect("Expected subgraphB to be present");
        assert_snapshot!(subgraph_b_extracted.schema.schema().to_string(), @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject", "@authenticated", "@requiresScopes", "@policy", "@sourceAPI", "@sourceType", "@sourceField", "@context", "@fromContext", "@cost", "@listSize"]) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @extends on OBJECT | INTERFACE

        directive @shareable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!, label: String) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @sourceAPI repeatable on SCHEMA

        directive @sourceType repeatable on OBJECT | INTERFACE

        directive @sourceField repeatable on FIELD_DEFINITION

        directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        type Product {
          sku: String! @shareable
          name: String!
        }

        type Query {
          products: [Product!]
        }
        "#);

        let subgraph_c_extracted = subgraphs
            .get("subgraphC")
            .expect("Expected subgraphC to be present");
        assert_snapshot!(subgraph_c_extracted.schema.schema().to_string(), @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject", "@authenticated", "@requiresScopes", "@policy", "@sourceAPI", "@sourceType", "@sourceField", "@context", "@fromContext", "@cost", "@listSize"]) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @extends on OBJECT | INTERFACE

        directive @shareable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!, label: String) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @sourceAPI repeatable on SCHEMA

        directive @sourceType repeatable on OBJECT | INTERFACE

        directive @sourceField repeatable on FIELD_DEFINITION

        directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        type Product @key(fields: "sku") {
          sku: String! @shareable
          color: String!
        }
        "#);
    }
}

mod validations {
    use super::*;

    #[test]
    fn errors_if_a_type_extension_has_no_definition_counterpart() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    q: String
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                extend type A @key(fields: "k") {
                    k: ID!
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        assert_composition_errors(
            &result,
            &[(
                "EXTENSION_WITH_NO_BASE",
                r#"[subgraphB] Type "A" is an extension type, but there is no type definition for "A" in any subgraph."#,
            )],
        );
    }

    #[test]
    fn include_pointers_to_fed1_schema_in_errors() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    a: A
                }

                scalar A
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @key(fields: "f") {
                    f: String
                }
            "#,
        };

        let subgraph_c = ServiceDefinition {
            name: "subgraphC",
            type_defs: r#"
                extend type A @key(fields: "f") {
                    f: String
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
        assert_composition_errors(
            &result,
            &[(
                "TYPE_KIND_MISMATCH",
                r#"Type "A" has mismatched kind: it is defined as Scalar Type in subgraph "subgraphA" but Object Type in subgraphs "subgraphB" and "subgraphC""#,
            )],
        );
    }
}

mod shareable {
    use super::*;
    use insta::assert_snapshot;

    #[test]
    fn handles_provides() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    a1: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type Query {
                    a2: A @provides(fields: "x")
                }

                extend type A @key(fields: "id") {
                    id: ID! @external
                    x: Int @external
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = result.expect("Expected composition to succeed");

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        assert_snapshot!(api_schema.schema().to_string(), @r#"
        type A {
          id: ID!
          x: Int
        }

        type Query {
          a1: A
          a2: A
        }
        "#);
    }

    #[test]
    fn handles_provides_with_mixed_fed1_fed2_schema_when_the_provides_is_in_the_fed2_schema() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    a1: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@provides", "@external"])

                type Query {
                    a2: A @provides(fields: "x")
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int @external
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = result.expect("Expected composition to succeed");

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        assert_snapshot!(api_schema.schema().to_string(), @r#"
        type A {
          id: ID!
          x: Int
        }

        type Query {
          a1: A
          a2: A
        }
        "#);
    }

    #[test]
    fn handles_provides_with_mixed_fed1_fed2_schema_when_the_provides_is_in_the_fed1_schema() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@shareable"])

                type Query {
                    a1: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type Query {
                    a2: A @provides(fields: "x")
                }

                extend type A @key(fields: "id") {
                    id: ID! @external
                    x: Int @external
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = result.expect("Expected composition to succeed");

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        assert_snapshot!(api_schema.schema().to_string(), @r#"
        type A {
          id: ID!
          x: Int
        }

        type Query {
          a1: A
          a2: A
        }
        "#);
    }

    #[test]
    fn errors_on_provides_with_non_shared_field_with_mixed_fed1_fed2_schema() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

                type Query {
                    a1: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type Query {
                    a2: A @provides(fields: "x")
                }

                extend type A @key(fields: "id") {
                    id: ID! @external
                    x: Int @external
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        assert_composition_errors(
            &result,
            &[(
                "INVALID_FIELD_SHARING",
                r#"Non-shareable field "A.x" is resolved from multiple subgraphs: it is resolved from subgraphs "subgraphA" and "subgraphB" and defined as non-shareable in subgraph "subgraphA""#,
            )],
        );
    }

    #[test]
    fn makes_value_types_shareable() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    a1: A
                }

                type A {
                    x: Int
                    y: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type Query {
                    a2: A
                }

                type A {
                    x: Int
                    y: Int
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = result.expect("Expected composition to succeed");

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        assert_snapshot!(api_schema.schema().to_string(), @r#"
        type A {
          x: Int
          y: Int
        }

        type Query {
          a1: A
          a2: A
        }
        "#);
    }

    #[test]
    fn supports_fed1_subgraphs_that_define_shareable() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Queryf {
                    friendlyFruit: Fruit!
                }

                directive @shareable on OBJECT | FIELD_DEFINITION

                type Fruit @shareable {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type Query {
                    forbiddenFruit: Fruit!
                }

                directive @shareable on OBJECT | FIELD_DEFINITION

                type Fruit @shareable {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        result.expect("Expected composition to succeed");
    }
}

mod override_tests {
    use super::*;
    use insta::assert_snapshot;

    #[test]
    fn accepts_override_if_the_definition_is_manually_provided() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    a: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @key(fields: "id") {
                    id: ID!
                    x: Int @override(from: "subgraphA")
                }

                directive @override(from: String!) on FIELD_DEFINITION
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        let supergraph = result.expect("Expected composition to succeed");

        let type_a = supergraph
            .schema()
            .schema()
            .types
            .get("A")
            .expect("A exists in the schema");
        assert_snapshot!(type_a.to_string(), @r#"type A @join__type(graph: SUBGRAPHA, key: "id") @join__type(graph: SUBGRAPHB, key: "id") {
          id: ID!
          x: Int @join__field(graph: SUBGRAPHB, override: "subgraphA")
        }"#);
    }

    #[test]
    fn errors_if_override_is_used_but_not_defined() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    a: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @key(fields: "id") {
                    id: ID!
                    x: Int @override(from: "subgraphA")
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        assert_composition_errors(
            &result,
            &[(
                "INVALID_GRAPHQL",
                r#"If you meant the "@override" federation 2 directive, note that this schema is a federation 1 schema. To be a federation 2 schema, it needs to @link to the federation specification v2."#,
            )],
        );
    }

    #[test]
    fn errors_if_override_is_defined_but_is_incompatible() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                type Query {
                    a: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "subgraphB",
            type_defs: r#"
                type A @key(fields: "id") {
                    id: ID!
                    x: Int @override
                }

                directive @override on FIELD_DEFINITION
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a, subgraph_b]);
        assert_composition_errors(
            &result,
            &[(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[subgraphB] Invalid definition for directive "@override": missing required argument "from""#,
            )],
        );
    }

    #[test]
    fn repro_redefined_built_in_scalar_breaks_key_directive() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                scalar Boolean
                type Query {
                    q: String
                }
                type A @key(fields: "k") {
                    k: ID!
                }
            "#,
        };

        let result = compose_fed1_subgraphs(&[subgraph_a]);
        result.expect("Expected composition to succeed");
    }
}
