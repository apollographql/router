// PORT_NOTE: This file ports `gateway-js/src/core/__tests__/core.test.ts`.
use apollo_federation::Supergraph;

mod core_v0_1 {
    use super::*;

    #[test]
    fn throws_no_errors_when_using_a_valid_core_v0_1_document() {
        let sdl = r#"
            schema
                @core(feature: "https://specs.apollo.dev/core/v0.1")
                @core(feature: "https://specs.apollo.dev/join/v0.1") {
                query: Query
            }

            directive @core(feature: String!) repeatable on SCHEMA

            directive @join__field(
                graph: join__Graph
                requires: join__FieldSet
                provides: join__FieldSet
            ) on FIELD_DEFINITION

            directive @join__type(
                graph: join__Graph!
                key: join__FieldSet
            ) repeatable on OBJECT | INTERFACE

            directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @tag(
                name: String!
            ) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            enum CacheControlScope {
                PRIVATE
                PUBLIC
            }

            scalar join__FieldSet

            enum join__Graph {
                WORLD @join__graph(name: "world", url: "https://world.api.com.invalid")
            }

            type Query {
                hello: String! @join__field(graph: WORLD)
            }
        "#;
        Supergraph::new(sdl).expect("parse and validate");
    }

    #[test]
    fn throws_error_when_for_argument_is_used_in_core_v0_1_document() {
        let sdl = r#"
            schema
                @core(feature: "https://specs.apollo.dev/core/v0.1")
                @core(feature: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
                @core(
                    feature: "https://specs.apollo.dev/something-unsupported/v0.1"
                    for: SECURITY
                ) {
                query: Query
            }

            directive @core(feature: String!) repeatable on SCHEMA

            directive @join__field(
                graph: join__Graph
                requires: join__FieldSet
                provides: join__FieldSet
            ) on FIELD_DEFINITION

            directive @join__type(
                graph: join__Graph!
                key: join__FieldSet
            ) repeatable on OBJECT | INTERFACE

            directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @tag(
                name: String!
            ) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            enum CacheControlScope {
                PRIVATE
                PUBLIC
            }

            scalar join__FieldSet

            enum join__Graph {
                WORLD @join__graph(name: "world", url: "https://world.api.com.invalid")
            }

            type Query {
                hello: String! @join__field(graph: WORLD)
            }
        "#;

        let err = Supergraph::new(sdl).expect_err("parsing schema with unsupported feature");
        // PORT_NOTE: The JS version delays schema validation so `checkFeatureSupport` function can
        //            specialize the error for core v0.1. However, in Rust version, the schema
        //            validation happens eagerly and generates the error below without
        //            specialization.
        insta::assert_snapshot!(
            err.to_string(),
            @r###"
        The following errors occurred:
          - Error: the argument `for` is not supported by `@core`
               ╭─[ schema.graphql:4:70 ]
               │
             4 │                 @core(feature: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
               │                 ──────────────────────────────────┬─────────────────────────┬───────  
               │                                                   ╰─────────────────────────────────── @core defined here
               │                                                                             │         
               │                                                                             ╰───────── argument by this name not found
            ───╯
            
          - Error: the argument `for` is not supported by `@core`
               ╭─[ schema.graphql:7:21 ]
               │
             5 │ ╭─▶                 @core(
               ┆ ┆   
             7 │ │                       for: SECURITY
               │ │                       ──────┬──────  
               │ │                             ╰──────── argument by this name not found
             8 │ ├─▶                 ) {
               │ │                         
               │ ╰───────────────────────── @core defined here
            ───╯
        "###,
        );
    }
}

mod core_v0_2 {
    use super::*;

    #[test]
    fn does_not_throw_errors_when_using_supported_features() {
        let sdl = r#"
            schema
                @core(feature: "https://specs.apollo.dev/core/v0.2")
                @core(feature: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
                @core(feature: "https://specs.apollo.dev/tag/v0.2") {
                query: Query
            }

            directive @core(
                feature: String!
                as: String
                for: core__Purpose
            ) repeatable on SCHEMA

            directive @join__field(
                graph: join__Graph
                requires: join__FieldSet
                provides: join__FieldSet
            ) on FIELD_DEFINITION

            directive @join__type(
                graph: join__Graph!
                key: join__FieldSet
            ) repeatable on OBJECT | INTERFACE

            directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @tag(
                name: String!
            ) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            enum CacheControlScope {
                PRIVATE
                PUBLIC
            }

            enum core__Purpose {
                """
                `EXECUTION` features provide metadata necessary to for operation execution.
                """
                EXECUTION

                """
                `SECURITY` features provide metadata necessary to securely resolve fields.
                """
                SECURITY
            }

            scalar join__FieldSet

            enum join__Graph {
                WORLD @join__graph(name: "world", url: "https://world.api.com.invalid")
            }

            type Query {
                hello: String! @join__field(graph: WORLD)
            }
        "#;

        Supergraph::new(sdl).expect("parse and validate");
    }

    #[test]
    fn does_not_throw_errors_when_using_unsupported_features_with_no_for_argument() {
        let sdl = r#"
            schema
                @core(feature: "https://specs.apollo.dev/core/v0.2")
                @core(feature: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
                @core(feature: "https://specs.apollo.dev/tag/v0.2")
                @core(feature: "https://specs.apollo.dev/unsupported-feature/v0.1") {
                query: Query
            }

            directive @core(
                feature: String!
                as: String
                for: core__Purpose
            ) repeatable on SCHEMA

            directive @join__field(
                graph: join__Graph
                requires: join__FieldSet
                provides: join__FieldSet
            ) on FIELD_DEFINITION

            directive @join__type(
                graph: join__Graph!
                key: join__FieldSet
            ) repeatable on OBJECT | INTERFACE

            directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @tag(
                name: String!
            ) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            enum CacheControlScope {
                PRIVATE
                PUBLIC
            }

            enum core__Purpose {
                """
                `EXECUTION` features provide metadata necessary to for operation execution.
                """
                EXECUTION

                """
                `SECURITY` features provide metadata necessary to securely resolve fields.
                """
                SECURITY
            }

            scalar join__FieldSet

            enum join__Graph {
                WORLD @join__graph(name: "world", url: "https://world.api.com.invalid")
            }

            type Query {
                hello: String! @join__field(graph: WORLD)
            }
        "#;

        Supergraph::new(sdl).expect("parse and validate");
    }

    #[test]
    fn throws_errors_when_using_unsupported_features_for_execution() {
        let sdl = r#"
            schema
                @core(feature: "https://specs.apollo.dev/core/v0.2")
                @core(feature: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
                @core(
                    feature: "https://specs.apollo.dev/unsupported-feature/v0.1"
                    for: EXECUTION
                ) {
                query: Query
            }

            directive @core(
                feature: String!
                as: String
                for: core__Purpose
            ) repeatable on SCHEMA

            directive @join__field(
                graph: join__Graph
                requires: join__FieldSet
                provides: join__FieldSet
            ) on FIELD_DEFINITION

            directive @join__type(
                graph: join__Graph!
                key: join__FieldSet
            ) repeatable on OBJECT | INTERFACE

            directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @tag(
                name: String!
            ) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            enum CacheControlScope {
                PRIVATE
                PUBLIC
            }

            enum core__Purpose {
                """
                `EXECUTION` features provide metadata necessary to for operation execution.
                """
                EXECUTION

                """
                `SECURITY` features provide metadata necessary to securely resolve fields.
                """
                SECURITY
            }

            scalar join__FieldSet

            enum join__Graph {
                WORLD @join__graph(name: "world", url: "https://world.api.com.invalid")
            }

            type Query {
                hello: String! @join__field(graph: WORLD)
            }
        "#;

        let err = Supergraph::new(sdl).expect_err("should error on validation");
        assert_eq!(
            err.to_string(),
            "feature https://specs.apollo.dev/unsupported-feature/v0.1 is for: EXECUTION but is unsupported",
        );
    }

    #[test]
    fn throws_errors_when_using_unsupported_features_for_security() {
        let sdl = r#"
            schema
                @core(feature: "https://specs.apollo.dev/core/v0.2")
                @core(feature: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
                @core(
                    feature: "https://specs.apollo.dev/unsupported-feature/v0.1"
                    for: SECURITY
                ) {
                query: Query
            }

            directive @core(
                feature: String!
                as: String
                for: core__Purpose
            ) repeatable on SCHEMA

            directive @join__field(
                graph: join__Graph
                requires: join__FieldSet
                provides: join__FieldSet
            ) on FIELD_DEFINITION

            directive @join__type(
                graph: join__Graph!
                key: join__FieldSet
            ) repeatable on OBJECT | INTERFACE

            directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @tag(
                name: String!
            ) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            enum CacheControlScope {
                PRIVATE
                PUBLIC
            }

            enum core__Purpose {
                """
                `EXECUTION` features provide metadata necessary to for operation execution.
                """
                EXECUTION

                """
                `SECURITY` features provide metadata necessary to securely resolve fields.
                """
                SECURITY
            }

            scalar join__FieldSet

            enum join__Graph {
                WORLD @join__graph(name: "world", url: "https://world.api.com.invalid")
            }

            type Query {
                hello: String! @join__field(graph: WORLD)
            }
        "#;

        let err = Supergraph::new(sdl).expect_err("should error on validation");
        assert_eq!(
            err.to_string(),
            "feature https://specs.apollo.dev/unsupported-feature/v0.1 is for: SECURITY but is unsupported",
        );
    }
}
