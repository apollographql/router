use apollo_federation::assert_errors;
use apollo_federation::subgraph::test_utils::BuildOption;
use apollo_federation::subgraph::test_utils::build_and_validate;
use apollo_federation::subgraph::test_utils::build_for_errors;
use apollo_federation::subgraph::test_utils::build_for_errors_with_option;

mod fieldset_based_directives {
    use super::*;

    #[test]
    fn rejects_field_defined_with_arguments_in_key() {
        let schema_str = r#"
            type Query {		
                t: T		
            }				  		
            type T @key(fields: "f") {		
                f(x: Int): Int		
            }	
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "KEY_FIELDS_HAS_ARGS",
                r#"[S] On type "T", for @key(fields: "f"): field T.f cannot be included because it has arguments (fields with argument are not allowed in @key)"#,
            )]
        );
    }

    #[test]
    fn rejects_field_defined_with_arguments_in_provides() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "f")
            }

            type T {
                f(x: Int): Int @external
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "PROVIDES_FIELDS_HAS_ARGS",
                r#"[S] On field "Query.t", for @provides(fields: "f"): field T.f cannot be included because it has arguments (fields with argument are not allowed in @provides)"#,
            )]
        );
    }

    #[test]
    fn rejects_provides_on_non_external_fields() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "f")
            }

            type T {
                f: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "PROVIDES_FIELDS_MISSING_EXTERNAL",
                r#"[S] On field "Query.t", for @provides(fields: "f"): field "T.f" should not be part of a @provides since it is already provided by this subgraph (it is not marked @external)"#,
            )]
        );
    }

    #[test]
    fn rejects_requires_on_non_external_fields() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T {
                f: Int
                g: Int @requires(fields: "f")
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "REQUIRES_FIELDS_MISSING_EXTERNAL",
                r#"[S] On field "T.g", for @requires(fields: "f"): field "T.f" should not be part of a @requires since it is already provided by this subgraph (it is not marked @external)"#,
            )]
        );
    }

    #[test]
    fn rejects_key_on_interfaces_in_all_specs() {
        for version in ["2.0", "2.1", "2.2"] {
            let schema_str = format!(
                r#"
                extend schema
                @link(url: "https://specs.apollo.dev/federation/v{version}", import: ["@key"])

                type Query {{
                t: T
                }}

                interface T @key(fields: "f") {{
                f: Int
                }}
            "#
            );
            let err = build_for_errors_with_option(&schema_str, BuildOption::AsIs);

            assert_errors!(
                err,
                [(
                    "KEY_UNSUPPORTED_ON_INTERFACE",
                    r#"[S] Cannot use @key on interface "T": @key is not yet supported on interfaces"#,
                )]
            );
        }
    }

    #[test]
    fn rejects_provides_on_interfaces() {
        let schema_str = r#"
            type Query {
                t: T
            }

            interface T {
                f: U @provides(fields: "g")
            }

            type U {
                g: Int @external
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "PROVIDES_UNSUPPORTED_ON_INTERFACE",
                r#"[S] Cannot use @provides on field "T.f" of parent type "T": @provides is not yet supported within interfaces"#,
            )]
        );
    }

    #[test]
    fn rejects_requires_on_interfaces() {
        let schema_str = r#"
            type Query {
                t: T
            }

            interface T {
                f: Int @external
                g: Int @requires(fields: "f")
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [
                (
                    "REQUIRES_UNSUPPORTED_ON_INTERFACE",
                    r#"[S] Cannot use @requires on field "T.g" of parent type "T": @requires is not yet supported within interfaces"#,
                ),
                (
                    "EXTERNAL_ON_INTERFACE",
                    r#"[S] Interface type field "T.f" is marked @external but @external is not allowed on interface fields."#,
                ),
            ]
        );
    }

    #[test]
    fn rejects_unused_external() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T {
                f: Int @external
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "EXTERNAL_UNUSED",
                r#"[S] Field "T.f" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#,
            )]
        );
    }

    #[test]
    fn rejects_provides_on_non_object_fields() {
        let schema_str = r#"
            type Query {
                t: Int @provides(fields: "f")
            }

            type T {
                f: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "PROVIDES_ON_NON_OBJECT_FIELD",
                r#"[S] Invalid @provides directive on field "Query.t": field has type "Int" which is not a Composite Type"#,
            )]
        );
    }

    #[test]
    fn rejects_non_string_argument_to_key() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T @key(fields: ["f"]) {
                f: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "KEY_INVALID_FIELDS_TYPE",
                r#"[S] On type "T", for @key(fields: ["f"]): Invalid value for argument "fields": must be a string."#,
            )]
        );
    }

    #[test]
    fn rejects_non_string_argument_to_provides() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: ["f"])
            }

            type T {
                f: Int @external
            }
        "#;
        let err = build_for_errors(schema_str);

        // Note: since the error here is that we cannot parse the key `fields`, this also means that @external on
        // `f` will appear unused and we get an error for it. It's kind of hard to avoid cleanly and hopefully
        // not a big deal (having errors dependencies is not exactly unheard of).
        assert_errors!(
            err,
            [
                (
                    "PROVIDES_INVALID_FIELDS_TYPE",
                    r#"[S] On field "Query.t", for @provides(fields: ["f"]): Invalid value for argument "fields": must be a string."#,
                ),
                (
                    "EXTERNAL_UNUSED",
                    r#"[S] Field "T.f" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#,
                ),
            ]
        );
    }

    #[test]
    fn rejects_non_string_argument_to_requires() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T {
                f: Int @external
                g: Int @requires(fields: ["f"])
            }
        "#;
        let err = build_for_errors(schema_str);

        // Note: since the error here is that we cannot parse the key `fields`, this also means that @external on
        // `f` will appear unused and we get an error for it. It's kind of hard to avoid cleanly and hopefully
        // not a big deal (having errors dependencies is not exactly unheard of).
        assert_errors!(
            err,
            [
                (
                    "REQUIRES_INVALID_FIELDS_TYPE",
                    r#"[S] On field "T.g", for @requires(fields: ["f"]): Invalid value for argument "fields": must be a string."#,
                ),
                (
                    "EXTERNAL_UNUSED",
                    r#"[S] Field "T.f" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#,
                ),
            ]
        );
    }

    #[test]
    // Special case of non-string argument, specialized because it hits a different
    // code-path due to enum values being parsed as string and requiring special care.
    fn rejects_enum_like_argument_to_key() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T @key(fields: f) {
                f: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "KEY_INVALID_FIELDS_TYPE",
                r#"[S] On type "T", for @key(fields: f): Invalid value for argument "fields": must be a string."#,
            )]
        );
    }

    #[test]
    // Special case of non-string argument, specialized because it hits a different
    // code-path due to enum values being parsed as string and requiring special care.
    fn rejects_enum_like_argument_to_provides() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: f)
            }

            type T {
                f: Int @external
            }
        "#;
        let err = build_for_errors(schema_str);

        // Note: since the error here is that we cannot parse the key `fields`, this also mean that @external on
        // `f` will appear unused and we get an error for it. It's kind of hard to avoid cleanly and hopefully
        // not a big deal (having errors dependencies is not exactly unheard of).
        assert_errors!(
            err,
            [
                (
                    "PROVIDES_INVALID_FIELDS_TYPE",
                    r#"[S] On field "Query.t", for @provides(fields: f): Invalid value for argument "fields": must be a string."#,
                ),
                (
                    "EXTERNAL_UNUSED",
                    r#"[S] Field "T.f" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#,
                ),
            ]
        );
    }

    #[test]
    // Special case of non-string argument, specialized because it hits a different
    // code-path due to enum values being parsed as string and requiring special care.
    fn rejects_enum_like_argument_to_requires() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T {
                f: Int @external
                g: Int @requires(fields: f)
            }
        "#;
        let err = build_for_errors(schema_str);

        // Note: since the error here is that we cannot parse the key `fields`, this also mean that @external on
        // `f` will appear unused and we get an error for it. It's kind of hard to avoid cleanly and hopefully
        // not a big deal (having errors dependencies is not exactly unheard of).
        assert_errors!(
            err,
            [
                (
                    "REQUIRES_INVALID_FIELDS_TYPE",
                    r#"[S] On field "T.g", for @requires(fields: f): Invalid value for argument "fields": must be a string."#,
                ),
                (
                    "EXTERNAL_UNUSED",
                    r#"[S] Field "T.f" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#,
                ),
            ]
        );
    }

    #[test]
    fn rejects_invalid_fields_argument_to_key() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T @key(fields: ":f") {
                f: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "KEY_INVALID_FIELDS",
                r#"[S] On type "T", for @key(fields: ":f"): Syntax error: expected at least one Selection in Selection Set"#,
            )]
        );
    }

    #[test]
    fn rejects_invalid_fields_argument_to_provides() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "{{f}}")
            }

            type T {
                f: Int @external
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [
                (
                    "PROVIDES_INVALID_FIELDS",
                    r#"[S] On field "Query.t", for @provides(fields: "{{f}}"): Syntax error: expected at least one Selection in Selection Set"#,
                ),
                (
                    "PROVIDES_INVALID_FIELDS",
                    r#"[S] On field "Query.t", for @provides(fields: "{{f}}"): Syntax error: expected R_CURLY, got {"#
                ),
                (
                    "EXTERNAL_UNUSED",
                    r#"[S] Field "T.f" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#,
                ),
            ]
        );
    }

    #[test]
    fn rejects_invalid_fields_argument_to_requires() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T {
                f: Int @external
                g: Int @requires(fields: "f b")
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "REQUIRES_INVALID_FIELDS",
                r#"[S] On field "T.g", for @requires(fields: "f b"): Cannot query field "b" on type "T". If the field is defined in another subgraph, you need to add it to this subgraph with @external."#,
            )]
        );
    }

    #[test]
    fn rejects_key_on_interface_field() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T @key(fields: "f") {
                f: I
            }

            interface I {
                i: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "KEY_FIELDS_SELECT_INVALID_TYPE",
                r#"[S] On type "T", for @key(fields: "f"): field "T.f" is an Interface type which is not allowed in @key"#,
            )]
        );
    }

    #[test]
    fn rejects_key_on_union_field() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T @key(fields: "f") {
                f: U
            }

            union U = Query | T
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "KEY_FIELDS_SELECT_INVALID_TYPE",
                r#"[S] On type "T", for @key(fields: "f"): field "T.f" is a Union type which is not allowed in @key"#,
            )]
        );
    }

    #[test]
    fn rejects_directive_applications_in_key() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T @key(fields: "v { x ... @include(if: false) { y }}") {
                v: V
            }

            type V {
                x: Int
                y: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "KEY_DIRECTIVE_IN_FIELDS_ARG",
                r#"[S] On type "T", for @key(fields: "v { x ... @include(if: false) { y }}"): cannot have directive applications in the @key(fields:) argument but found @include(if: false)."#,
            )]
        );
    }

    #[test]
    fn rejects_directive_applications_in_provides() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "v { ... on V @skip(if: true) { x y } }")
            }

            type T @key(fields: "id") {
                id: ID
                v: V @external
            }

            type V {
                x: Int
                y: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "PROVIDES_DIRECTIVE_IN_FIELDS_ARG",
                r#"[S] On field "Query.t", for @provides(fields: "v { ... on V @skip(if: true) { x y } }"): cannot have directive applications in the @provides(fields:) argument but found @skip(if: true)."#,
            )]
        );
    }

    #[test]
    fn rejects_directive_applications_in_requires() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T @key(fields: "id") {
                id: ID
                a: Int @requires(fields: "... @skip(if: false) { b }")
                b: Int @external
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "REQUIRES_DIRECTIVE_IN_FIELDS_ARG",
                r#"[S] On field "T.a", for @requires(fields: "... @skip(if: false) { b }"): cannot have directive applications in the @requires(fields:) argument but found @skip(if: false)."#,
            )]
        );
    }

    #[test]
    fn can_collect_multiple_errors_in_a_single_fields_argument() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "f(x: 3)")
            }

            type T @key(fields: "id") {
                id: ID
                f(x: Int): Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [
                (
                    "PROVIDES_FIELDS_HAS_ARGS",
                    r#"[S] On field "Query.t", for @provides(fields: "f(x: 3)"): field T.f cannot be included because it has arguments (fields with argument are not allowed in @provides)"#,
                ),
                (
                    "PROVIDES_FIELDS_MISSING_EXTERNAL",
                    r#"[S] On field "Query.t", for @provides(fields: "f(x: 3)"): field "T.f" should not be part of a @provides since it is already provided by this subgraph (it is not marked @external)"#,
                ),
            ]
        );
    }

    #[test]
    fn rejects_aliases_in_key() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T @key(fields: "foo: id") {
                id: ID!
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "KEY_INVALID_FIELDS",
                r#"[S] On type "T", for @key(fields: "foo: id"): Cannot use alias "foo" in "foo: id": aliases are not currently supported in @key"#,
            )]
        );
    }

    #[test]
    fn rejects_aliases_in_provides() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "bar: x")
            }

            type T @key(fields: "id") {
                id: ID!
                x: Int @external
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "PROVIDES_INVALID_FIELDS",
                r#"[S] On field "Query.t", for @provides(fields: "bar: x"): Cannot use alias "bar" in "bar: x": aliases are not currently supported in @provides"#,
            )]
        );
    }

    #[test]
    fn rejects_aliases_in_requires() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T {
                x: X @external
                y: Int @external
                g: Int @requires(fields: "foo: y")
                h: Int @requires(fields: "x { m: a n: b }")
            }

            type X {
                a: Int
                b: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [
                (
                    "REQUIRES_INVALID_FIELDS",
                    r#"[S] On field "T.g", for @requires(fields: "foo: y"): Cannot use alias "foo" in "foo: y": aliases are not currently supported in @requires"#,
                ),
                (
                    "REQUIRES_INVALID_FIELDS",
                    r#"[S] On field "T.h", for @requires(fields: "x { m: a n: b }"): Cannot use alias "m" in "m: a": aliases are not currently supported in @requires"#,
                ),
                // PORT NOTE: JS didn't include this last message, but we should report the other alias if we're making the effort to collect all the errors
                (
                    "REQUIRES_INVALID_FIELDS",
                    r#"[S] On field "T.h", for @requires(fields: "x { m: a n: b }"): Cannot use alias "n" in "n: b": aliases are not currently supported in @requires"#,
                ),
            ]
        );
    }

    #[test]
    fn handles_requires_with_sub_selection() {
        let schema_str = r#"
            extend schema @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@external", "@key", "@requires"])

            type Query {
                t: T
            }

            type T @key(fields: "id") {
                id: ID!
                u: U
                required: Int @requires(fields: "u { inner }")
            }

            type U @key(fields: "id") {
              id: ID!
              inner: String @external
            }
        "#;
        build_and_validate(schema_str);
    }
}

mod root_types {
    use super::*;

    #[test]
    fn rejects_using_query_as_type_name_if_not_the_query_root() {
        let schema_str = r#"
            schema {
                query: MyQuery
            }

            type MyQuery {
                f: Int
            }

            type Query {
                g: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "ROOT_QUERY_USED",
                r#"[S] The schema has a type named "Query" but it is not set as the query root type ("MyQuery" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#,
            )]
        );
    }

    #[test]
    fn rejects_using_mutation_as_type_name_if_not_the_mutation_root() {
        let schema_str = r#"
            schema {
                mutation: MyMutation
            }

            type MyMutation {
                f: Int
            }

            type Mutation {
                g: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "ROOT_MUTATION_USED",
                r#"[S] The schema has a type named "Mutation" but it is not set as the mutation root type ("MyMutation" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#,
            )]
        );
    }

    #[test]
    fn rejects_using_subscription_as_type_name_if_not_the_subscription_root() {
        let schema_str = r#"
            schema {
                subscription: MySubscription
            }

            type MySubscription {
                f: Int
            }

            type Subscription {
                g: Int
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "ROOT_SUBSCRIPTION_USED",
                r#"[S] The schema has a type named "Subscription" but it is not set as the subscription root type ("MySubscription" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#,
            )]
        );
    }
}

mod custom_error_message_for_misnamed_directives {
    use super::*;

    struct FedVersionSchemaParams {
        build_option: BuildOption,
        extra_msg: &'static str,
    }

    #[test]
    fn has_suggestions_if_a_federation_directive_is_misspelled_in_all_schema_versions() {
        let schema_versions = [
            FedVersionSchemaParams {
                // fed1
                build_option: BuildOption::AsIs,
                extra_msg: " If so, note that it is a federation 2 directive but this schema is a federation 1 one. To be a federation 2 schema, it needs to @link to the federation specification v2.",
            },
            FedVersionSchemaParams {
                // fed2
                build_option: BuildOption::AsFed2,
                extra_msg: "",
            },
        ];
        for fed_ver in schema_versions {
            let schema_str = r#"
                    type T @keys(fields: "id") {
                        id: Int @foo
                        foo: String @sharable
                    }
                "#;
            let err = build_for_errors_with_option(schema_str, fed_ver.build_option);

            assert_errors!(
                err,
                [
                    (
                        "INVALID_GRAPHQL",
                        r#"[S] Error: cannot find directive `@keys` in this document
   ╭─[ S:2:28 ]
   │
 2 │                     type T @keys(fields: "id") {
   │                            ─────────┬─────────
   │                                     ╰─────────── directive not defined
───╯
Did you mean "@key"?"#,
                    ),
                    (
                        "INVALID_GRAPHQL",
                        r#"[S] Error: cannot find directive `@foo` in this document
   ╭─[ S:3:33 ]
   │
 3 │                         id: Int @foo
   │                                 ──┬─
   │                                   ╰─── directive not defined
───╯"#,
                    ),
                    (
                        "INVALID_GRAPHQL",
                        &format!(
                            r#"[S] Error: cannot find directive `@sharable` in this document
   ╭─[ S:4:37 ]
   │
 4 │                         foo: String @sharable
   │                                     ────┬────
   │                                         ╰────── directive not defined
───╯
Did you mean "@shareable"?{}"#,
                            fed_ver.extra_msg
                        ),
                    ),
                ]
            );
        }
    }

    #[test]
    fn has_suggestions_if_a_fed2_directive_is_used_in_fed1() {
        let schema_str = r#"
            type T @key(fields: "id") {
                id: Int
                foo: String @shareable
            }
        "#;
        let err = build_for_errors_with_option(schema_str, BuildOption::AsIs);

        assert_errors!(
            err,
            [(
                "INVALID_GRAPHQL",
                r#"[S] Error: cannot find directive `@shareable` in this document
   ╭─[ S:4:29 ]
   │
 4 │                 foo: String @shareable
   │                             ─────┬────
   │                                  ╰────── directive not defined
───╯
 If you meant the "@shareable" federation 2 directive, note that this schema is a federation 1 schema. To be a federation 2 schema, it needs to @link to the federation specification v2."#,
            )]
        );
    }

    #[test]
    fn has_suggestions_if_a_fed2_directive_is_used_under_wrong_name_for_the_schema() {
        let schema_str = r#"
            extend schema
                @link(
                url: "https://specs.apollo.dev/federation/v2.0"
                import: [{ name: "@key", as: "@myKey" }]
                )

            type T @key(fields: "id") {
                id: Int
                foo: String @shareable
            }
        "#;
        let err = build_for_errors_with_option(schema_str, BuildOption::AsIs);

        assert_errors!(
            err,
            [
                (
                    "INVALID_GRAPHQL",
                    r#"[S] Error: cannot find directive `@key` in this document
   ╭─[ S:8:20 ]
   │
 8 │             type T @key(fields: "id") {
   │                    ─────────┬────────
   │                             ╰────────── directive not defined
───╯
 If you meant the "@key" federation directive, you should use "@myKey" as it is imported under that name in the @link to the federation specification of this schema."#,
                ),
                (
                    "INVALID_GRAPHQL",
                    r#"[S] Error: cannot find directive `@shareable` in this document
    ╭─[ S:10:29 ]
    │
 10 │                 foo: String @shareable
    │                             ─────┬────
    │                                  ╰────── directive not defined
────╯
 If you meant the "@shareable" federation directive, you should use fully-qualified name "@federation__shareable" or add "@shareable" to the \`import\` argument of the @link to the federation specification."#,
                ),
            ]
        );
    }
}

// PORT_NOTE: Corresponds to '@core/@link handling' tests in JS
#[cfg(test)]
mod link_handling_tests {
    use similar::TextDiff;

    use super::*;

    // There are a few whitespace differences between this and the JS version, but the more important difference is that
    // the links are added as a new extension instead of being attached to the top-level schema definition. We may need
    // to revisit that later if we're doing strict comparisons of SDLs between versions.
    const EXPECTED_FULL_SCHEMA: &str = r#"schema {
  query: Query
}

extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION

directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION

directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION

directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

directive @federation__extends on OBJECT | INTERFACE

directive @federation__shareable on OBJECT | FIELD_DEFINITION

directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

directive @federation__override(from: String!) on FIELD_DEFINITION

type T @key(fields: "k") {
  k: ID!
}

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

scalar _Any

type _Service {
  sdl: String
}

union _Entity = T

type Query {
  _entities(representations: [_Any!]!): [_Entity]!
  _service: _Service!
}
"#;

    #[test]
    fn expands_everything_if_only_the_federation_spec_is_linked() {
        let subgraph = build_and_validate(
            r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type T @key(fields: "k") {
                k: ID!
            }
            "#,
        );

        assert_eq!(
            subgraph.schema_string(),
            EXPECTED_FULL_SCHEMA,
            "{}",
            TextDiff::from_lines(EXPECTED_FULL_SCHEMA, subgraph.schema_string().as_str())
                .unified_diff()
        );
    }

    #[test]
    fn expands_definitions_if_both_the_federation_spec_and_link_spec_are_linked() {
        let subgraph = build_and_validate(
            r#"
            extend schema
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type T @key(fields: "k") {
                k: ID!
            }
            "#,
        );

        assert_eq!(
            subgraph.schema_string(),
            EXPECTED_FULL_SCHEMA,
            "{}",
            TextDiff::from_lines(EXPECTED_FULL_SCHEMA, subgraph.schema_string().as_str())
                .unified_diff()
        );
    }

    #[test]
    fn is_valid_if_a_schema_is_complete_from_the_get_go() {
        let subgraph = build_and_validate(EXPECTED_FULL_SCHEMA);
        assert_eq!(
            subgraph.schema_string(),
            EXPECTED_FULL_SCHEMA,
            "{}",
            TextDiff::from_lines(EXPECTED_FULL_SCHEMA, subgraph.schema_string().as_str())
                .unified_diff()
        );
    }

    #[test]
    fn expands_missing_definitions_when_some_are_partially_provided() {
        let docs = [
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/link/v1.0")
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@key"]
                  )

                type T @key(fields: "k") {
                  k: ID!
                }

                directive @key(
                  fields: federation__FieldSet!
                  resolvable: Boolean = true
                ) repeatable on OBJECT | INTERFACE

                scalar federation__FieldSet

                scalar link__Import
            "#,
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/link/v1.0")
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@key"]
                  )

                type T @key(fields: "k") {
                  k: ID!
                }

                scalar link__Import
            "#,
            r#"
                extend schema
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@key"]
                  )

                type T @key(fields: "k") {
                  k: ID!
                }

                scalar link__Import
            "#,
            r#"
                extend schema
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@key"]
                  )

                type T @key(fields: "k") {
                  k: ID!
                }

                directive @federation__external(
                  reason: String
                ) on OBJECT | FIELD_DEFINITION
            "#,
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type T {
                  k: ID!
                }

                enum link__Purpose {
                  EXECUTION
                  SECURITY
                }
            "#,
        ];

        // Note that we cannot use `validateFullSchema` as-is for those examples because the order
        // or directive is going to be different. But that's ok, we mostly care that the validation
        // doesn't fail, so we can be somewhat sure that if something necessary wasn't expanded
        // properly, we would have an issue. The main reason we did validate the full schema in
        // prior tests is so we had at least one full example of a subgraph expansion in the tests.
        docs.iter().for_each(|doc| {
            _ = build_and_validate(doc);
        });
    }

    #[test]
    fn allows_directive_redefinition_without_optional_argument() {
        // @key has a `resolvable` argument in its full definition, but it is optional.
        let _ = build_and_validate(
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/link/v1.0")
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@key"]
                  )

                type T @key(fields: "k") {
                  k: ID!
                }

                directive @key(
                  fields: federation__FieldSet!
                ) repeatable on OBJECT | INTERFACE

                scalar federation__FieldSet
            "#,
        );
    }

    #[test]
    fn allows_directive_redefinition_with_subset_of_locations() {
        // @inaccessible can be put in a bunch of locations, but you're welcome to restrict
        // yourself to just fields.
        let _ = build_and_validate(
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/link/v1.0")
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@inaccessible"]
                  )

                type T {
                  k: ID! @inaccessible
                }

                directive @inaccessible on FIELD_DEFINITION
            "#,
        );
    }

    #[test]
    fn allows_directive_redefinition_without_repeatable() {
        // @key is repeatable, but you're welcome to restrict yourself to never repeating it.
        let _ = build_and_validate(
            r#"
                extend schema
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@key"]
                  )

                type T @key(fields: "k") {
                  k: ID!
                }

                directive @key(
                  fields: federation__FieldSet!
                  resolvable: Boolean = true
                ) on OBJECT | INTERFACE

                scalar federation__FieldSet
            "#,
        );
    }

    #[test]
    fn allows_directive_redefinition_changing_optional_argument_to_required() {
        let docs = [
            // @key `resolvable` argument is optional, but you're welcome to force users to always
            // provide it.
            r#"
                extend schema
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@key"]
                  )

                type T @key(fields: "k", resolvable: true) {
                  k: ID!
                }

                directive @key(
                  fields: federation__FieldSet!
                  resolvable: Boolean!
                ) repeatable on OBJECT | INTERFACE

                scalar federation__FieldSet
            "#,
            // @link `url` argument is allowed to be `null` now, but it used not too, so making
            // sure we still accept definition where it's mandatory.
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/link/v1.0")
                  @link(
                    url: "https://specs.apollo.dev/federation/v2.0"
                    import: ["@key"]
                  )

                type T @key(fields: "k") {
                  k: ID!
                }

                directive @link(
                  url: String!
                  as: String
                  for: link__Purpose
                  import: [link__Import]
                ) repeatable on SCHEMA

                scalar link__Import
                scalar link__Purpose
            "#,
        ];

        // Like above, we really only care that the examples validate.
        docs.iter().for_each(|doc| {
            _ = build_and_validate(doc);
        });
    }

    #[test]
    fn errors_on_invalid_known_directive_location() {
        // @external is not allowed on 'schema' and likely never will.
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type T @key(fields: "k") {
                k: ID!
            }

            directive @federation__external(
                reason: String
            ) on OBJECT | FIELD_DEFINITION | SCHEMA
            "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@federation__external": "@federation__external" should have locations OBJECT, FIELD_DEFINITION, but found (non-subset) OBJECT, FIELD_DEFINITION, SCHEMA"#,
            )]
        );
    }

    #[test]
    fn errors_on_invalid_non_repeatable_directive_marked_repeatable() {
        let doc = r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/federation/v2.0" import: ["@key"])

                type T @key(fields: "k") {
                  k: ID!
                }

                directive @federation__external repeatable on OBJECT | FIELD_DEFINITION
            "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@federation__external": "@federation__external" should not be repeatable"#,
            )]
        );
    }

    #[test]
    fn errors_on_unknown_argument_of_known_directive() {
        let doc = r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type T @key(fields: "k") {
              k: ID!
            }

            directive @federation__external(foo: Int) on OBJECT | FIELD_DEFINITION
        "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@federation__external": unknown/unsupported argument "foo""#,
            )]
        );
    }

    #[test]
    fn errors_on_invalid_type_for_a_known_argument() {
        let doc = r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

              type T @key(fields: "k") {
                k: ID!
              }

              directive @key(
                fields: String!
                resolvable: String
              ) repeatable on OBJECT | INTERFACE
        "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@key": argument "resolvable" should have type "Boolean" but found type "String""#,
            )]
        );
    }

    #[test]
    fn errors_on_a_required_argument_defined_as_optional() {
        let doc = r#"
                extend schema
                    @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

                type T @key(fields: "k") {
                    k: ID!
                }

                directive @key(
                    fields: federation__FieldSet
                    resolvable: Boolean = true
                ) repeatable on OBJECT | INTERFACE

                scalar federation__FieldSet
            "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@key": argument "fields" should have type "federation__FieldSet!" but found type "federation__FieldSet""#,
            )]
        );
    }

    #[test]
    fn errors_on_invalid_definition_for_link_purpose() {
        let doc = r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type T {
                    k: ID!
                }

                enum link__Purpose {
                    EXECUTION
                    RANDOM
                }
            "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "TYPE_DEFINITION_INVALID",
                r#"[S] Invalid definition for type "Purpose": expected values [EXECUTION, SECURITY] but found [EXECUTION, RANDOM]."#,
            )]
        );
    }

    #[test]
    fn allows_any_non_scalar_type_in_redefinition_when_expected_type_is_a_scalar() {
        // Just making sure this doesn't error out.
        build_and_validate(
            r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

              type T @key(fields: "k") {
                k: ID!
              }

              # 'fields' should be of type 'federation_FieldSet!', but ensure we allow 'String!' alternatively.
              directive @key(
                fields: String!
                resolvable: Boolean = true
              ) repeatable on OBJECT | INTERFACE
            "#,
        );
    }

    #[test]
    fn allows_defining_a_repeatable_directive_as_non_repeatable_but_validates_usages() {
        let doc = r#"
            type T @key(fields: "k1") @key(fields: "k2") {
                k1: ID!
                k2: ID!
            }

            directive @key(fields: String!) on OBJECT
        "#;

        // Test for fed2 (with @key being @link-ed)
        assert_errors!(
            build_for_errors(doc),
            [(
                "INVALID_GRAPHQL",
                r###"
                [S] Error: non-repeatable directive key can only be used once per location
                   ╭─[ S:2:39 ]
                   │
                 2 │             type T @key(fields: "k1") @key(fields: "k2") {
                   │                    ──┬─               ─────────┬────────  
                   │                      ╰──────────────────────────────────── directive `@key` first called here
                   │                                                │          
                   │                                                ╰────────── directive `@key` called again here
                ───╯
                "###
            )]
        );

        // Test for fed1
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "INVALID_GRAPHQL",
                r###"
                [S] Error: non-repeatable directive key can only be used once per location
                   ╭─[ S:2:39 ]
                   │
                 2 │             type T @key(fields: "k1") @key(fields: "k2") {
                   │                    ──┬─               ─────────┬────────  
                   │                      ╰──────────────────────────────────── directive `@key` first called here
                   │                                                │          
                   │                                                ╰────────── directive `@key` called again here
                ───╯
                "###
            )]
        );
    }
}

mod federation_1_schema_tests {
    use super::*;

    #[test]
    fn accepts_federation_directive_definitions_without_arguments() {
        let doc = r#"
            type Query {
                a: Int
            }

            directive @key on OBJECT | INTERFACE
            directive @requires on FIELD_DEFINITION
        "#;
        build_and_validate(doc);
    }

    #[test]
    fn accepts_federation_directive_definitions_with_nullable_arguments() {
        let doc = r#"
            type Query {
                a: Int
            }

            type T @key(fields: "id") {
                id: ID! @requires(fields: "x")
                x: Int @external
            }

            # Tests with the _FieldSet argument non-nullable
            scalar _FieldSet
            directive @key(fields: _FieldSet) on OBJECT | INTERFACE

            # Tests with the argument as String and non-nullable
            directive @requires(fields: String) on FIELD_DEFINITION
        "#;
        build_and_validate(doc);
    }

    #[test]
    fn accepts_federation_directive_definitions_with_fieldset_type_instead_of_underscore_fieldset()
    {
        // accepts federation directive definitions with "FieldSet" type instead of "_FieldSet"
        let doc = r#"
            type Query {
                a: Int
            }

            type T @key(fields: "id") {
                id: ID!
            }

            scalar FieldSet
            directive @key(fields: FieldSet) on OBJECT | INTERFACE
        "#;
        build_and_validate(doc);
    }

    #[test]
    fn rejects_federation_directive_definition_with_unknown_arguments() {
        let doc = r#"
            type Query {
                a: Int
            }

            type T @key(fields: "id", unknown: 42) {
                id: ID!
            }

            scalar _FieldSet
            directive @key(fields: _FieldSet!, unknown: Int) on OBJECT | INTERFACE
        "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@key": unknown/unsupported argument "unknown""#
            )]
        );
    }
}

mod shareable_tests {
    use apollo_federation::subgraph::test_utils::build_inner;

    use super::*;

    #[test]
    fn can_only_be_applied_to_fields_of_object_types() {
        let doc = r#"
            interface I {
                a: Int @shareable
            }
        "#;
        assert_errors!(
            build_for_errors(doc),
            [(
                "INVALID_SHAREABLE_USAGE",
                r#"[S] Invalid use of @shareable on field "I.a": only object type fields can be marked with @shareable"#
            )]
        );
    }

    #[test]
    fn rejects_duplicate_shareable_on_the_same_definition_declaration() {
        let doc = r#"
            type E @shareable @key(fields: "id") @shareable {
                id: ID!
                a: Int
            }
        "#;
        assert_errors!(
            build_for_errors(doc),
            [(
                "INVALID_SHAREABLE_USAGE",
                r#"[S] Invalid duplicate application of @shareable on the same type declaration of "E": @shareable is only repeatable on types so it can be used simultaneously on a type definition and its extensions, but it should not be duplicated on the same definition/extension declaration"#
            )]
        );
    }

    #[test]
    fn rejects_duplicate_shareable_on_the_same_extension_declaration() {
        let doc = r#"
            type E @shareable {
                id: ID!
                a: Int
            }

            extend type E @shareable @shareable {
                b: Int
            }
        "#;
        assert_errors!(
            build_for_errors(doc),
            [(
                "INVALID_SHAREABLE_USAGE",
                r#"[S] Invalid duplicate application of @shareable on the same type declaration of "E": @shareable is only repeatable on types so it can be used simultaneously on a type definition and its extensions, but it should not be duplicated on the same definition/extension declaration"#
            )]
        );
    }

    #[test]
    fn rejects_duplicate_shareable_on_a_field() {
        let doc = r#"
            type E {
                a: Int @shareable @shareable
            }
        "#;
        assert_errors!(
            build_for_errors(doc),
            [(
                "INVALID_SHAREABLE_USAGE",
                r#"[S] Invalid duplicate application of @shareable on field "E.a": @shareable is only repeatable on types so it can be used simultaneously on a type definition and its extensions, but it should not be duplicated on the same definition/extension declaration"#
            )]
        );
    }

    #[test]
    fn allows_shareable_on_declaration_and_extension_of_same_type() {
        let doc = r#"
            type E @shareable {
                id: ID!
                a: Int
            }

            extend type E @shareable {
                b: Int
            }
        "#;
        assert!(build_inner(doc, BuildOption::AsFed2).is_ok());
    }
}

mod interface_object_and_key_on_interfaces_validation_tests {
    use super::*;

    #[test]
    fn key_on_interfaces_require_key_on_all_implementations() {
        let doc = r#"
            interface I @key(fields: "id1") @key(fields: "id2") {
                id1: ID!
                id2: ID!
            }

            type A implements I @key(fields: "id2") {
                id1: ID!
                id2: ID!
                a: Int
            }

            type B implements I @key(fields: "id1") @key(fields: "id2") {
                id1: ID!
                id2: ID!
                b: Int
            }

            type C implements I @key(fields: "id2") {
                id1: ID!
                id2: ID!
                c: Int
            }
        "#;
        assert_errors!(
            build_for_errors(doc),
            [(
                "INTERFACE_KEY_NOT_ON_IMPLEMENTATION",
                r#"[S] Key @key(fields: "id1") on interface type "I" is missing on implementation types "A" and "C"."#
            )]
        );
    }

    #[test]
    fn key_on_interfaces_with_key_on_some_implementation_non_resolvable() {
        let doc = r#"
            interface I @key(fields: "id1") {
                id1: ID!
            }

            type A implements I @key(fields: "id1") {
                id1: ID!
                a: Int
            }

            type B implements I @key(fields: "id1") {
                id1: ID!
                b: Int
            }

            type C implements I @key(fields: "id1", resolvable: false) {
                id1: ID!
                c: Int
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [(
                "INTERFACE_KEY_NOT_ON_IMPLEMENTATION",
                r#"[S] Key @key(fields: "id1") on interface type "I" should be resolvable on all implementation types, but is declared with argument "@key(resolvable:)" set to false in type "C"."#
            )]
        );
    }

    #[test]
    fn ensures_order_of_fields_in_key_does_not_matter() {
        let doc = r#"
            interface I @key(fields: "a b c") {
                a: Int
                b: Int
                c: Int
            }

            type A implements I @key(fields: "c b a") {
                a: Int
                b: Int
                c: Int
            }

            type B implements I @key(fields: "a c b") {
                a: Int
                b: Int
                c: Int
            }

            type C implements I @key(fields: "a b c") {
                a: Int
                b: Int
                c: Int
            }
        "#;

        // Ensure no errors are returned
        build_and_validate(doc);
    }

    #[test]
    fn only_allow_interface_object_on_entity_types() {
        // There is no meaningful way to make @interfaceObject work on a value type at the moment,
        // because if you have an @interfaceObject, some other subgraph needs to be able to resolve
        // the concrete type, and that imply that you have key to go to that other subgraph. To be
        // clear, the @key on the @interfaceObject technically don't need to be "resolvable", and
        // the difference between no key and a non-resolvable key is arguably more of a convention
        // than a genuine mechanical difference at the moment, but still a good idea to rely on
        // that convention to help catching obvious mistakes early.
        let doc = r#"
            # This one shouldn't raise an error
            type A @key(fields: "id", resolvable: false) @interfaceObject {
                id: ID!
            }

            # This one should
            type B @interfaceObject {
                x: Int
            }
        "#;
        assert_errors!(
            build_for_errors(doc),
            [(
                "INTERFACE_OBJECT_USAGE_ERROR",
                r#"[S] The @interfaceObject directive can only be applied to entity types but type "B" has no @key in this subgraph."#
            )]
        );
    }
}

mod cost_tests {
    use super::*;

    #[test]
    fn rejects_cost_applications_on_interfaces() {
        let doc = r#"
            type Query {
                a: A
            }

            interface A {
                x: Int @cost(weight: 10)
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [(
                "COST_APPLIED_TO_INTERFACE_FIELD",
                r#"[S] @cost cannot be applied to interface "A.x""#
            )]
        );
    }
}

mod list_size_tests {
    use super::*;

    #[test]
    fn rejects_applications_on_non_lists_unless_it_uses_sized_fields() {
        let doc = r#"
            type Query {
                a1: A @listSize(assumedSize: 5)
                a2: A @listSize(assumedSize: 10, sizedFields: ["ints"])
            }

            type A {
                ints: [Int]
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [(
                "LIST_SIZE_APPLIED_TO_NON_LIST",
                r#"[S] "Query.a1" is not a list"#
            )]
        );
    }

    #[test]
    fn rejects_negative_assumed_size() {
        let doc = r#"
            type Query {
                a: [Int] @listSize(assumedSize: -5)
                b: [Int] @listSize(assumedSize: 0)
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [(
                "LIST_SIZE_INVALID_ASSUMED_SIZE",
                r#"[S] Assumed size of "Query.a" cannot be negative"#
            )]
        );
    }

    #[test]
    fn rejects_slicing_arguments_not_in_field_arguments() {
        let doc = r#"
            type Query {
                myField(something: Int): [String]
                    @listSize(slicingArguments: ["missing1", "missing2"])
                myOtherField(somethingElse: String): [Int]
                    @listSize(slicingArguments: ["alsoMissing"])
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [
                (
                    "LIST_SIZE_INVALID_SLICING_ARGUMENT",
                    r#"[S] Slicing argument "missing1" is not an argument of "Query.myField""#
                ),
                (
                    "LIST_SIZE_INVALID_SLICING_ARGUMENT",
                    r#"[S] Slicing argument "missing2" is not an argument of "Query.myField""#
                ),
                (
                    "LIST_SIZE_INVALID_SLICING_ARGUMENT",
                    r#"[S] Slicing argument "alsoMissing" is not an argument of "Query.myOtherField""#
                )
            ]
        );
    }

    #[test]
    fn rejects_slicing_arguments_not_int_or_int_non_null() {
        let doc = r#"
            type Query {
                sliced(
                    first: String
                    second: Int
                    third: Int!
                    fourth: [Int]
                    fifth: [Int]!
                ): [String]
                    @listSize(
                        slicingArguments: ["first", "second", "third", "fourth", "fifth"]
                    )
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [
                (
                    "LIST_SIZE_INVALID_SLICING_ARGUMENT",
                    r#"[S] Slicing argument "Query.sliced(first:)" must be Int or Int!"#
                ),
                (
                    "LIST_SIZE_INVALID_SLICING_ARGUMENT",
                    r#"[S] Slicing argument "Query.sliced(fourth:)" must be Int or Int!"#
                ),
                (
                    "LIST_SIZE_INVALID_SLICING_ARGUMENT",
                    r#"[S] Slicing argument "Query.sliced(fifth:)" must be Int or Int!"#
                )
            ]
        );
    }

    #[test]
    fn rejects_sized_fields_when_output_type_is_not_object() {
        let doc = r#"
            type Query {
                notObject: Int @listSize(assumedSize: 1, sizedFields: ["anything"])
                a: A @listSize(assumedSize: 5, sizedFields: ["ints"])
                b: B @listSize(assumedSize: 10, sizedFields: ["ints"])
            }

            type A {
                ints: [Int]
            }

            interface B {
                ints: [Int]
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [(
                "LIST_SIZE_INVALID_SIZED_FIELD",
                r#"[S] Sized fields cannot be used because "Int" is not a composite type"#
            )]
        );
    }

    #[test]
    fn rejects_sized_fields_not_in_output_type() {
        let doc = r#"
            type Query {
                a: A @listSize(assumedSize: 5, sizedFields: ["notOnA"])
            }

            type A {
                ints: [Int]
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [(
                "LIST_SIZE_INVALID_SIZED_FIELD",
                r#"[S] Sized field "notOnA" is not a field on type "A""#
            )]
        );
    }

    #[test]
    fn rejects_sized_fields_not_lists() {
        let doc = r#"
            type Query {
                a: A
                    @listSize(
                        assumedSize: 5
                        sizedFields: ["list", "nonNullList", "notList"]
                    )
            }

            type A {
                list: [String]
                nonNullList: [String]!
                notList: String
            }
        "#;

        assert_errors!(
            build_for_errors(doc),
            [(
                "LIST_SIZE_APPLIED_TO_NON_LIST",
                r#"[S] Sized field "A.notList" is not a list"#
            )]
        );
    }
}

mod tag_tests {
    use super::*;

    #[test]
    fn errors_on_tag_missing_required_argument() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

            directive @tag on FIELD_DEFINITION
        "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@tag": Missing required argument "name""#
            )]
        );
    }

    #[test]
    fn errors_on_tag_with_unknown_argument() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

            directive @tag(name: String!, foo: Int) repeatable on FIELD_DEFINITION | OBJECT
        "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@tag": unknown/unsupported argument "foo""#
            )]
        );
    }

    #[test]
    fn errors_on_tag_with_wrong_argument_type() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

            directive @tag(name: Int!) repeatable on FIELD_DEFINITION | OBJECT
        "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@tag": argument "name" should have type "String!" but found type "Int!""#
            )]
        );
    }

    #[test]
    fn errors_on_tag_with_wrong_locations() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

            directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | SCHEMA
        "#;
        assert_errors!(
            build_for_errors_with_option(doc, BuildOption::AsIs),
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@tag": "@tag" should have locations FIELD_DEFINITION, OBJECT, INTERFACE, UNION, ARGUMENT_DEFINITION, SCALAR, ENUM, ENUM_VALUE, INPUT_OBJECT, INPUT_FIELD_DEFINITION, but found (non-subset) FIELD_DEFINITION, OBJECT, SCHEMA"#
            )]
        );
    }

    #[test]
    fn allows_tag_with_valid_subset_of_locations() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

            type T @tag(name: "foo") { x: Int }

            directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE
        "#;
        let _ = build_and_validate(doc);
    }

    #[test]
    fn errors_on_invalid_symbols_in_tag_name() {
        let disallowed_symbols = [
            ' ', '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '{', '}', '[', ']', '|', ';',
            ':', '+', '=', '.', ',', '?', '`', '~',
        ];
        for symbol in disallowed_symbols {
            let doc = include_str!("fixtures/tag_validation_template.graphqls")
                .replace("{symbol}", &symbol.to_string());
            let err = build_for_errors_with_option(&doc, BuildOption::AsIs);

            assert_errors!(
                err,
                [
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema root has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Foo has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Foo.foo1 has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Foo.foo2(arg:) has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Bar has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Query.foo has invalid @tag directive value '{symbol}test' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Query.bar has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Query.baz has invalid @tag directive value 'test{symbol}test' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Bar.bar1 has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Bar.bar2(arg:) has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Baz has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element CustomScalar has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element TestEnum has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element TestEnum.VALUE1 has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element TestInput has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element TestInput.inputField1 has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element TestInput.inputField3 has invalid @tag directive value '{symbol}test' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element @custom(arg:) has invalid @tag directive value 'test{symbol}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                ]
            );
        }
    }

    #[test]
    fn errors_on_invalid_symbols_at_start_of_tag_name() {
        let disallowed_symbols = ['-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
        for symbol in disallowed_symbols {
            let doc = include_str!("fixtures/tag_validation_template.graphqls")
                .replace("{symbol}", &symbol.to_string());
            let err = build_for_errors_with_option(&doc, BuildOption::AsIs);

            assert_errors!(
                err,
                [
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element Query.foo has invalid @tag directive value '{symbol}test' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                    (
                        "INVALID_TAG_NAME",
                        &format!(
                            "[S] Schema element TestInput.inputField3 has invalid @tag directive value '{symbol}test' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                        )
                    ),
                ]
            );
        }
    }

    #[test]
    fn allows_valid_symbols_in_tag_name() {
        let allowed_symbols = [
            '_', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p',
            'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
        ];
        for symbol in allowed_symbols {
            let doc = include_str!("fixtures/tag_validation_template.graphqls")
                .replace("{symbol}", &symbol.to_string());
            // Build should succeed without errors
            let _ = build_and_validate(&doc);
        }
    }

    #[test]
    fn errors_when_tag_name_exceeds_length_limit() {
        // 128 chars is valid
        let valid = "a".repeat(128);
        let doc_valid = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

            type T @tag(name: "VALID_TAG") { x: Int }
            "#
        .replace("VALID_TAG", &valid);
        // Build should succeed without errors
        let _ = build_and_validate(&doc_valid);

        // 129 chars is invalid
        let invalid = "a".repeat(129);
        let doc_invalid = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

            type T @tag(name: "INVALID_TAG") { x: Int }
            "#
        .replace("INVALID_TAG", &invalid);
        let err = build_for_errors_with_option(&doc_invalid, BuildOption::AsIs);
        assert_errors!(
            err,
            [(
                "INVALID_TAG_NAME",
                &format!(
                    "[S] Schema element T has invalid @tag directive value '{invalid}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores."
                )
            )]
        );
    }
}
