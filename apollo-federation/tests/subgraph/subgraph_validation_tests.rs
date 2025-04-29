use apollo_federation::subgraph::SubgraphError;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::subgraph::typestate::Validated;

fn build_inner(schema_str: &str) -> Result<Subgraph<Validated>, SubgraphError> {
    let name = "S";
    Subgraph::parse(name, &format!("http://{name}"), schema_str)
        .expect("valid schema")
        .expand_links()
        .map_err(|e| SubgraphError::new(name, e))?
        .validate(true)
}

fn build_and_validate(schema_str: &str) -> Subgraph<Validated> {
    build_inner(schema_str).expect("expanded subgraph to be valid")
}

fn build_for_errors(schema: &str) -> Vec<(String, String)> {
    build_inner(schema)
        .expect_err("subgraph error was expected")
        .format_errors()
}

fn remove_indentation(s: &str) -> String {
    // count the last lines that are space-only
    let first_empty_lines = s.lines().take_while(|line| line.trim().is_empty()).count();
    let last_empty_lines = s
        .lines()
        .rev()
        .take_while(|line| line.trim().is_empty())
        .count();

    // lines without the space-only first/last lines
    let lines = s
        .lines()
        .skip(first_empty_lines)
        .take(s.lines().count() - first_empty_lines - last_empty_lines);

    // compute the indentation
    let indentation = lines
        .clone()
        .map(|line| line.chars().take_while(|c| *c == ' ').count())
        .min()
        .unwrap_or(0);

    // remove the indentation
    lines
        .map(|line| {
            line.trim_end()
                .chars()
                .skip(indentation)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// True if a and b contain the same error messages
fn check_errors(a: &[(String, String)], b: &[(&str, &str)]) -> Result<(), String> {
    if a.len() != b.len() {
        return Err(format!(
            "Mismatched error counts: {} != {}",
            a.len(),
            b.len()
        ));
    }

    // remove indentations from messages to ignore indentation differences
    let b_iter = b
        .iter()
        .map(|(code, message)| (*code, remove_indentation(message)));
    let diff: Vec<_> = a
        .iter()
        .map(|(code, message)| (code.as_str(), remove_indentation(message)))
        .zip(b_iter)
        .filter(|(a_i, b_i)| a_i.0 != b_i.0 || a_i.1 != b_i.1)
        .collect();
    if diff.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Mismatched errors:\n{}\n",
            diff.iter()
                .map(|(a_i, b_i)| { format!("- {}: {}\n+ {}: {}", b_i.0, b_i.1, a_i.0, a_i.1) })
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

macro_rules! assert_errors {
    ($a:expr, $b:expr) => {
        match check_errors(&$a, &$b) {
            Ok(()) => {
                // Success
            }
            Err(e) => {
                panic!("{e}")
            }
        }
    };
}

mod fieldset_based_directives {
    use super::*;

    #[test]
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
    fn rejects_key_on_interfaces_in_all_specs() {
        for version in ["2.0", "2.1", "2.2"] {
            let schema_str = format!(
                r#"
                extend schema
                @link(url: "https://specs.apollo.dev/federation/v{}", import: ["@key"])

                type Query {{
                t: T
                }}

                interface T @key(fields: "f") {{
                f: Int
                }}
            "#,
                version
            );
            let err = build_for_errors(&schema_str);

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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
                    r#"[S] Interface type field "T.f" is marked @external but @external is not allowed on interface fields (it is nonsensical)."#,
                ),
            ]
        );
    }

    #[test]
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
    fn rejects_provides_on_non_object_fields() {
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
                "PROVIDES_ON_NON_OBJECT_FIELD",
                r#"[S] Invalid @provides directive on field "Query.t": field has type "Int" which is not a Composite Type"#,
            )]
        );
    }

    #[test]
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
                r#"[S] On type "T", for @key(fields: ":f"): Syntax Error: Expected Name, found ":"."#,
            )]
        );
    }

    #[test]
    #[should_panic(expected = r#"Mismatched error counts: 1 != 2"#)]
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
                    r#"[S] On field "Query.t", for @provides(fields: "{{f}}"): Syntax Error: Expected Name, found "{"."#,
                ),
                (
                    "EXTERNAL_UNUSED",
                    r#"[S] Field "T.f" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#,
                ),
            ]
        );
    }

    #[test]
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
                r#"[S] On field "T.g", for @requires(fields: "f b"): Cannot query field "b" on type "T" (if the field is defined in another subgraph, you need to add it to this subgraph with @external)."#,
            )]
        );
    }

    #[test]
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
                r#"[S] On type "T", for @key(fields: "f"): field "T.f" is a Interface type which is not allowed in @key"#,
            )]
        );
    }

    #[test]
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
            ]
        );
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
        extended_schema: &'static str,
        extra_msg: &'static str,
    }

    #[test]
    #[should_panic(expected = r#"Mismatched error counts: 1 != 3"#)]
    fn has_suggestions_if_a_federation_directive_is_misspelled_in_all_schema_versions() {
        let schema_versions = [
            FedVersionSchemaParams {
                // fed1
                extended_schema: r#""#,
                extra_msg: " If so, note that it is a federation 2 directive but this schema is a federation 1 one. To be a federation 2 schema, it needs to @link to the federation specification v2.",
            },
            FedVersionSchemaParams {
                // fed2
                extended_schema: r#"
                    extend schema
                        @link(url: "https://specs.apollo.dev/federation/v2.0")
                    "#,
                extra_msg: "",
            },
        ];
        for fed_ver in schema_versions {
            let schema_str = format!(
                r#"{}
                    type T @keys(fields: "id") {{
                        id: Int @foo
                        foo: String @sharable
                    }}
                "#,
                fed_ver.extended_schema
            );
            let err = build_for_errors(&schema_str);

            assert_errors!(
                err,
                [
                    ("INVALID_GRAPHQL", r#"[S] Unknown directive "@foo"."#,),
                    (
                        "INVALID_GRAPHQL",
                        format!(
                            r#"[S] Unknown directive "@sharable". Did you mean "@shareable"?{}"#,
                            fed_ver.extra_msg
                        )
                        .as_str(),
                    ),
                    (
                        "INVALID_GRAPHQL",
                        r#"[S] Unknown directive "@keys". Did you mean "@key"?"#,
                    ),
                ]
            );
        }
    }

    #[test]
    #[should_panic(expected = r#"Mismatched errors:"#)]
    fn has_suggestions_if_a_fed2_directive_is_used_in_fed1() {
        let schema_str = r#"
            type T @key(fields: "id") {
                id: Int
                foo: String @shareable
            }
        "#;
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [(
                "INVALID_GRAPHQL",
                r#"[S] Unknown directive "@shareable". If you meant the \"@shareable\" federation 2 directive, note that this schema is a federation 1 schema. To be a federation 2 schema, it needs to @link to the federation specification v2."#,
            )]
        );
    }

    #[test]
    #[should_panic(expected = r#"Mismatched error counts: 1 != 2"#)]
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
        let err = build_for_errors(schema_str);

        assert_errors!(
            err,
            [
                (
                    "INVALID_GRAPHQL",
                    r#"[S] Unknown directive "@shareable". If you meant the \"@shareable\" federation directive, you should use fully-qualified name "@federation__shareable" or add "@shareable" to the \`import\` argument of the @link to the federation specification."#,
                ),
                (
                    "INVALID_GRAPHQL",
                    r#"[S] Unknown directive "@key". If you meant the "@key" federation directive, you should use "@myKey" as it is imported under that name in the @link to the federation specification of this schema."#,
                ),
            ]
        );
    }
}

// PORT_NOTE: Corresponds to '@core/@link handling' tests in JS
#[cfg(test)]
mod link_handling_tests {
    use super::*;

    // TODO(FED-543): Remaining directive definitions should be added to the schema
    #[allow(dead_code)]
    const EXPECTED_FULL_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
    {
      query: Query
    }

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

    type T
      @key(fields: "k")
    {
      k: ID!
    }

    enum link__Purpose {
      """
      \`SECURITY\` features provide metadata necessary to securely resolve fields.
      """
      SECURITY

      """
      \`EXECUTION\` features provide metadata necessary for operation execution.
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

        // TODO(FED-543): `subgraph` is supposed to be compared against `EXPECTED_FULL_SCHEMA`, but
        //                it's failing due to missing directive definitions. So, we use
        //                `insta::assert_snapshot` for now.
        // assert_eq!(subgraph.schema_string(), EXPECTED_FULL_SCHEMA);
        insta::assert_snapshot!(subgraph.schema_string(), @r###"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @federation__shareable on OBJECT | FIELD_DEFINITION

        directive @federation__override(from: String!) on FIELD_DEFINITION

        directive @federation__tag repeatable on ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

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
        "###);
    }

    // TODO: FED-428
    #[test]
    #[should_panic(
        expected = r#"InvalidLinkDirectiveUsage { message: "Invalid use of @link in schema: the @link specification itself (\"https://specs.apollo.dev/link/v1.0\") is applied multiple times" }"#
    )]
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

        assert_eq!(subgraph.schema_string(), EXPECTED_FULL_SCHEMA);
    }

    // TODO: FED-428
    #[test]
    #[should_panic(
        expected = r#"InvalidLinkDirectiveUsage { message: "Invalid use of @link in schema: the @link specification itself (\"https://specs.apollo.dev/link/v1.0\") is applied multiple times" }"#
    )]
    fn is_valid_if_a_schema_is_complete_from_the_get_go() {
        let subgraph = build_and_validate(EXPECTED_FULL_SCHEMA);
        assert_eq!(subgraph.schema_string(), EXPECTED_FULL_SCHEMA);
    }

    // TODO: FED-428
    #[test]
    #[should_panic(
        expected = r#"InvalidLinkDirectiveUsage { message: "Invalid use of @link in schema: the @link specification itself (\"https://specs.apollo.dev/link/v1.0\") is applied multiple times" }"#
    )]
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

    // TODO: FED-428
    #[test]
    #[should_panic(
        expected = r#"InvalidLinkDirectiveUsage { message: "Invalid use of @link in schema: the @link specification itself (\"https://specs.apollo.dev/link/v1.0\") is applied multiple times" }"#
    )]
    fn allows_known_directives_with_incomplete_but_compatible_definitions() {
        let docs = [
            // @key has a `resolvable` argument in its full definition, but it is optional.
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
            // @inaccessible can be put in a bunch of locations, but you're welcome to restrict
            // yourself to just fields.
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
            // @key is repeatable, but you're welcome to restrict yourself to never repeating it.
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
        let errors = build_for_errors(
            // @external is not allowed on 'schema' and likely never will.
            r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type T @key(fields: "k") {
                k: ID!
            }

            directive @federation__external(
                reason: String
            ) on OBJECT | FIELD_DEFINITION | SCHEMA
            "#,
        );

        assert_errors!(
            errors,
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@federation__external": "@federation__external" should have locations OBJECT, FIELD_DEFINITION, but found (non-subset) OBJECT, FIELD_DEFINITION, SCHEMA"#,
            )]
        );
    }

    #[test]
    fn errors_on_invalid_non_repeatable_directive_marked_repeatable() {
        let errors = build_for_errors(
            r#"
                extend schema
                  @link(url: "https://specs.apollo.dev/federation/v2.0" import: ["@key"])

                type T @key(fields: "k") {
                  k: ID!
                }

                directive @federation__external repeatable on OBJECT | FIELD_DEFINITION
            "#,
        );
        assert_errors!(
            errors,
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@federation__external": "@federation__external" should not be repeatable"#,
            )]
        );
    }

    #[test]
    fn errors_on_unknown_argument_of_known_directive() {
        let errors = build_for_errors(
            r#"
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type T @key(fields: "k") {
              k: ID!
            }

            directive @federation__external(foo: Int) on OBJECT | FIELD_DEFINITION
            "#,
        );
        assert_errors!(
            errors,
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@federation__external": unknown/unsupported argument "foo""#,
            )]
        );
    }

    #[test]
    fn errors_on_invalid_type_for_a_known_argument() {
        let errors = build_for_errors(
            r#"
              extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

              type T @key(fields: "k") {
                k: ID!
              }

              directive @key(
                fields: String!
                resolvable: String
              ) repeatable on OBJECT | INTERFACE
            "#,
        );
        assert_errors!(
            errors,
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@key": argument "resolvable" should have type "Boolean" but found type "String""#,
            )]
        );
    }

    #[test]
    fn errors_on_a_required_argument_defined_as_optional() {
        let errors = build_for_errors(
            r#"
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
            "#,
        );
        assert_errors!(
            errors,
            [(
                "DIRECTIVE_DEFINITION_INVALID",
                r#"[S] Invalid definition for directive "@key": argument "fields" should have type "federation__FieldSet!" but found type "federation__FieldSet""#,
            )]
        );
    }

    #[test]
    fn errors_on_invalid_definition_for_link_purpose() {
        let errors = build_for_errors(
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type T {
                    k: ID!
                }

                enum link__Purpose {
                    EXECUTION
                    RANDOM
                }
            "#,
        );
        assert_errors!(
            errors,
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

        // TODO: Test for fed1
    }
}
