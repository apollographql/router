use apollo_federation::supergraph::Satisfiable;
use apollo_federation::supergraph::Supergraph;
use rstest::rstest;

use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Initial;
use apollo_federation::subgraph::typestate::Subgraph;

mod simple_cases {
    use super::*;

    #[test]
    fn simple_success_case() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_has_directive_definition(
            &result,
            "directive @foo(name: String!) on FIELD_DEFINITION",
        );

        let schema = result.schema().schema().to_string();
        assert!(
            schema.contains(r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#)
        );
        assert!(schema.contains(r#"subgraphA: String @foo(name: "a")"#));
    }

    #[test]
    fn simple_success_case_no_import() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0")"#,
            r#"@composeDirective(name: "@foo__bar")"#,
            "directive @foo__bar(name: String!) on FIELD_DEFINITION",
            r#"@foo__bar(name: "a")"#,
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_has_directive_definition(
            &result,
            "directive @foo__bar(name: String!) on FIELD_DEFINITION",
        );

        let schema = result.schema().schema().to_string();
        assert!(schema.contains(r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: [{ name: "@bar", as: "@foo_bar" }])"#));
        assert!(schema.contains(r#"subgraphA: String @foo__bar(name: "a")"#));
    }

    #[test]
    fn simple_success_case_renamed_compose_directive() {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
      extend schema
        @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", { name: "@composeDirective", as: "@apolloCompose" }])
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])
        @apolloCompose(name: "@foo")

        directive @foo(name: String!) on FIELD_DEFINITION
        type Query {
          a: User
        }
        type User @key(fields: "id") {
          id: Int
          a: String @foo(name: "a")
        }
    "#)
        .unwrap()
        .into_fed2_test_subgraph(true)
        .unwrap();
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_has_directive_definition(
            &result,
            "directive @foo(name: String!) on FIELD_DEFINITION",
        );

        let schema = result.schema().schema().to_string();
        assert!(
            schema.contains(r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#)
        );
        assert!(schema.contains(r#"subgraphA: String @foo(name: "a")"#));
    }
}

mod federation_directives {
    use super::*;

    #[rstest]
    #[case("@tag")]
    #[case("@inaccessible")]
    #[case("@authenticated")]
    #[case("@requiresScopes")]
    fn hints_for_default_composed_federation_directives(#[case] directive: &str) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            &format!("@composeDirective(name: \"{directive}\")"),
            "",
            "",
            "",
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_eq!(result.hints().len(), 1);
        let hint = result.hints().first().unwrap();
        assert_eq!(hint.code, "DIRECTIVE_COMPOSITION_INFO");
        assert_eq!(
            hint.message,
            format!(
                "Directive \"{directive}\" should not be explicitly composed since it is a federation directive composed by default"
            )
        );
    }

    #[rstest]
    #[case("@tag")]
    #[case("@inaccessible")]
    #[case("@authenticated")]
    #[case("@requiresScopes")]
    fn hints_for_renamed_default_composed_federation_directives(#[case] directive: &str) {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
      extend schema
        @link(url: "https://specs.apollo.dev/federation/v2.5", import: [{ name: "@key" }, { name: "@composeDirective" } , { name: "<DIRECTIVE>", as: "@apolloDirective" }])
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @composeDirective(name: "@apolloDirective")

        type Query {
          a: User
        }
        type User @key(fields: "id") {
          id: Int
          a: String
        }
    "#
        .replace("<DIRECTIVE>", directive)
        .as_str())
        .unwrap();

        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_eq!(result.hints().len(), 1);
        let hint = result.hints().first().unwrap();
        assert_eq!(hint.code, "DIRECTIVE_COMPOSITION_INFO");
        assert_eq!(
            hint.message,
            format!(
                "Directive \"@apolloDirective\" should not be explicitly composed since it is a federation directive composed by default"
            )
        );
    }

    #[rstest]
    #[case("@key")]
    #[case("@requires")]
    #[case("@provides")]
    #[case("@external")]
    #[case("@extends")]
    #[case("@shareable")]
    #[case("@override")]
    #[case("@composeDirective")]
    fn errors_for_federation_directives_with_nontrivial_compositions(#[case] directive: &str) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            &format!("@composeDirective(name: \"{directive}\")"),
            "",
            "",
            "",
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "Composing federation directive \"{directive}\" in subgraph \"subgraphA\" is not supported"
            )
        );
    }

    #[rstest]
    #[case("@key")]
    #[case("@requires")]
    #[case("@provides")]
    #[case("@external")]
    #[case("@extends")]
    #[case("@shareable")]
    #[case("@override")]
    #[case("@composeDirective")]
    fn errors_for_renamed_federation_directives_with_nontrivial_compositions(
        #[case] directive: &str,
    ) {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
      extend schema
        @link(url: "https://specs.apollo.dev/federation/v2.1", import: [{ name: "@key" }, { name: "@composeDirective" } , { name: "<DIRECTIVE>", as: "@apolloDirective" }])
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @composeDirective(name: "@apolloDirective")

        type Query {
          a: User
        }
        type User @key(fields: "id") {
          id: Int
          a: String
        }
    "#
        .replace("<DIRECTIVE>", directive)
        .as_str())
        .unwrap();

        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "Composing federation directive \"@apolloDirective\" in subgraph \"subgraphA\" is not supported"
            )
        );
    }

    #[rstest]
    #[case("@join__field")]
    #[case("@join__graph")]
    #[case("@join__implements")]
    #[case("@join__type")]
    #[case("@join__unionMember")]
    #[case("@join__enumValue")]
    fn errors_for_join_spec_directives(#[case] directive: &str) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)"#,
            &format!("@composeDirective(name: \"{directive}\")"),
            r#"
        directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

        scalar join__FieldSet

        enum join__Graph {
          WORLD @join__graph(name: "world", url: "https://world.api.com.invalid")
        }
        "#,
            "",
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "Composing federation directive \"{directive}\" in subgraph \"subgraphA\" is not supported"
            )
        );
    }
}

mod inconsistent_feature_versions {
    use super::*;

    #[test]
    fn hints_when_mismatched_versions_are_not_composed() {
        let subgraph_a = generate_subgraph(
            r#"subgraphA"#,
            r#"@link(url: "https://specs.apollo.dev/foo/v5.0", import: ["@foo"])"#,
            "",
            r#"directive @foo(String!) on FIELD_DEFINITION"#,
            r#"@foo("a")"#,
        );
        let subgraph_b = generate_subgraph(
            r#"subgraphB"#,
            r#"@link(url: "https://specs.apollo.dev/foo/v2.0", import: ["@foo"])"#,
            "",
            r#"directive @foo(String!) on FIELD_DEFINITION"#,
            r#"@foo("b")"#,
        );
        let subgraph_c = generate_subgraph(
            r#"subgraphC"#,
            r#"@link(url: "https://specs.apollo.dev/foo/v3.0", import: ["@foo"])"#,
            "",
            r#"directive @foo(String!) on FIELD_DEFINITION"#,
            r#"@foo("")"#,
        );
        let subgraph_d = generate_subgraph(
            r#"subgraphD"#,
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            "",
            r#"directive @foo(String!) on FIELD_DEFINITION"#,
            r#"@foo("b")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b, subgraph_c, subgraph_d]).unwrap();
        assert_eq!(result.hints().len(), 1);
        let hint = result.hints().first().unwrap();
        assert_eq!(hint.code, "DIRECTIVE_COMPOSITION_INFO");
        assert_eq!(
            hint.message,
            r#"Non-composed core feature "https://specs.apollo.dev/foo" has major version mismatch across subgraphs"#
        );
    }

    #[rstest]
    #[case(r#"@link(url: "https://specs.apollo.dev/foo/v2.0", import: ["@foo"])"#)]
    #[case(r#"@link(url: "https://specs.apollo.dev/foo/v2.0", import: ["@bar"])"#)]
    fn errors_when_mismatched_major_versions_are_composed(#[case] link_text: &str) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            link_text,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "b")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Core feature "https://specs.apollo.dev/foo" requested to be merged has major version mismatch across subgraphs"#
        );
    }

    #[rstest]
    #[case(
        r#"composeDirective(name: "foo")"#,
        "https://specs.apollo.dev/foo/v1.4",
        "directive @foo(name: String!) on FIELD_DEFINITION | OBJECT"
    )]
    #[case(
        "",
        "https://specs.apollo.dev/foo/v1.0",
        "directive @foo(name: String!) on FIELD_DEFINITION"
    )]
    fn composes_mismatched_versions_with_latest_used_definition(
        #[case] compose_text_newer_link: &str,
        #[case] expected_link: &str,
        #[case] expected_definition: &str,
    ) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            r#"composeDirective(name: "foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.4", import: ["@foo"])"#,
            compose_text_newer_link,
            "directive @foo(name: String!) on FIELD_DEFINITION | OBJECT",
            r#"@foo(name: "b")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_eq!(result.hints().len(), 0);

        assert!(result.schema().schema().to_string().contains(expected_link));
        assert_has_directive_definition(&result, expected_definition);
    }
}

mod inconsistent_imports {
    use super::*;

    #[rstest]
    #[case(
        r#"
        directive @foo(name: String!) on FIELD_DEFINITION
        directive @bar(name: String!, address: String) on FIELD_DEFINITION | OBJECT
    "#
    )]
    #[case(
        r#"
        directive @foo(name: String!) on FIELD_DEFINITION
        directive @foo_bar(name: String!, address: String) on FIELD_DEFINITION | OBJECT
    "#
    )]
    fn composes_mismatched_imports_with_unqualified_name(#[case] directive_text: &str) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            directive_text,
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.1", import: ["@bar"])"#,
            r#"@composeDirective(name: "@bar")"#,
            r#"
            directive @foo(name: String!) on FIELD_DEFINITION
            directive @bar(name: String!, address: String) on FIELD_DEFINITION | OBJECT
            "#,
            r#"@bar(name: "b")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_has_directive_definition(
            &result,
            "directive @foo(name: String!) on FIELD_DEFINITION",
        );
        assert_has_directive_definition(
            &result,
            "directive @bar(name: String!, address: String) on FIELD_DEFINITION | OBJECT",
        );

        let schema = result.schema().schema().to_string();
        assert!(schema.contains(r#"subgraphA: String @foo(name: "a")"#));
        assert!(schema.contains(r#"subgraphB: String @bar(name: "b")"#));
    }

    #[test]
    fn hints_when_imported_with_mismatched_name_but_not_exported() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo", { name: "@bar", as: "@baz" }])"#,
            r#"@composeDirective(name: "@foo")"#,
            r#"
            directive @foo(name: String!) on FIELD_DEFINITION
            directive @baz(name: String!, address: String) on FIELD_DEFINITION | OBJECT
            "#,
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.1", import: ["@bar"])"#,
            r#"@composeDirective(name: "@bar")"#,
            r#"
            directive @foo(name: String!) on FIELD_DEFINITION
            directive @bar(name: String!, address: String) on FIELD_DEFINITION | OBJECT
            "#,
            r#"@bar(name: "b")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();

        assert_eq!(result.hints().len(), 1);
        let hint = result.hints().first().unwrap();
        assert_eq!(hint.code, "DIRECTIVE_COMPOSITION_WARN");
        assert_eq!(
            hint.message,
            r#"Composed directive "@bar" is named differently in a subgraph that doesn't export it. Consistent naming will be required to export it."#
        );

        assert_has_directive_definition(
            &result,
            "directive @foo(name: String!) on FIELD_DEFINITION",
        );
        assert_has_directive_definition(
            &result,
            "directive @bar(name: String!, address: String) on FIELD_DEFINITION | OBJECT",
        );

        let schema = result.schema().schema().to_string();
        assert!(schema.contains(r#"subgraphA: String @foo(name: "a")"#));
        assert!(schema.contains(r#"subgraphB: String @bar(name: "b")"#));
    }

    #[test]
    fn errors_when_exported_but_undefined() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.1", import: ["@bar"])"#,
            r#"@composeDirective(name: "@bar")"#,
            r#"
            directive @foo(name: String!) on FIELD_DEFINITION
            directive @bar(name: String!, address: String) on FIELD_DEFINITION | OBJECT
           "#,
            r#"@bar(name: "b")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Core feature "https://specs.apollo.dev/foo" in subgraph "subgraphA" does not have a directive definition for "@bar""#,
        );
    }

    #[test]
    fn errors_when_exported_but_not_imported() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            "",
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Directive "@foo" in subgraph "subgraphA" cannot be composed because it is not a member of a core feature"#
        );
    }

    #[test]
    fn errors_when_exported_with_mismatched_names() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: [{ name: "@foo", as: "@bar" }])"#,
            r#"@composeDirective(name: "@bar")"#,
            "directive @bar(name: String!) on FIELD_DEFINITION",
            r#"@bar(name: "b")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Composed directive is not named consistently in all subgraphs but "@foo" in subgraph "subgraphA" and "@bar" in subgraph "subgraphB""#,
        );
    }

    #[test]
    fn errors_when_exported_directive_is_imported_from_different_specs() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.apollo.dev/bar/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Composed directive "@foo" is not linked by the same core feature in every subgraph"#
        );
    }

    #[test]
    fn errors_when_different_exported_directives_have_the_same_name() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: [{ name: "@bar", as: "@foo" }])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Composed directive "@foo" does not refer to the same directive in every subgraph"#
        );
    }

    #[test]
    fn errors_when_exported_directives_conflict_with_federation_directives() {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective"])
                @link(url: "https://specs.apollo.dev/foo/v1.0", import: [{ name: "@foo", as: "@inaccessible" }])
                @composeDirective(name: "@inaccessible")

            directive @inaccessible(name: String!) on FIELD_DEFINITION
            type Query {
                a: User
            }
            type User @key(fields: "id") {
                id: Int
                a: String @inaccessible(name: "a")
            }
        "#).unwrap();
        let subgraph_b = Subgraph::parse("subgraphB", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", "@inaccessible"])
                @link(url: "https://specs.apollo.dev/link/v1.0")

            type Query {
                b: User
            }
            type User @key(fields: "id") {
                id: Int
                b: String @inaccessible
            }
        "#).unwrap();

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Directive "@inaccessible" in subgraph "subgraphA" cannot be composed because it conflicts with automatically composed federation directive "@inaccessible". Conflict exists in subgraph(s): (subgraphB)"#
        );
    }

    #[rstest]
    #[case("@join__field")]
    #[case("@join__graph")]
    #[case("@join__implements")]
    #[case("@join__type")]
    #[case("@join__unionMember")]
    #[case("@join__enumValue")]
    fn errors_when_exported_directives_conflict_with_join_spec_directives(#[case] directive: &str) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            &r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: [{ name: "@foo", as: "<DIRECTIVE>" }])"#.replace("<DIRECTIVE>", directive),
            &r#"@composeDirective(name: "<DIRECTIVE>")"#.replace("<DIRECTIVE>", directive),
            &r#"directive <DIRECTIVE>(name: String!) on FIELD_DEFINITION"#.replace("<DIRECTIVE>", directive),
            &r#"<DIRECTIVE>(name: "a")"#.replace("<DIRECTIVE>", directive),
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "Directive \"{directive}\" in subgraph \"subgraphA\" cannot be composed because it is not a member of a core feature"
            )
        );
    }
}

mod validation {
    use super::*;

    #[rstest]
    #[case("@composeDirective")]
    #[case("@composeDirective(name: null)")]
    #[case(r#"@composeDirective(name: "")"#)]
    fn errors_when_name_argument_is_null_or_empty(#[case] compose_text: &str) {
        let subgraph_a = generate_subgraph("subgraphA", "", compose_text, "", "");
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Argument to @composeDirective in subgraph "subgraphA" cannot be NULL or an empty String"#
        );
    }

    #[test]
    fn errors_when_name_argument_is_missing_at_symbol() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            "",
            r#"@composeDirective(name: "foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Argument to @composeDirective in subgraph "subgraphA" must have a leading "@""#
        );
    }

    #[rstest]
    #[case("@foo", "@foo", "@fooz", r#"Did you mean "@foo" or "@cost"?"#)]
    #[case(
        r#"{ name: "@foo", as "@bar" }"#,
        "@bar",
        "@barz",
        r#"Did you mean "@bar" or "@tag"?"#
    )]
    fn errors_when_directive_does_not_exist(
        #[case] import: &str,
        #[case] name: &str,
        #[case] usage: &str,
        #[case] suggestion: &str,
    ) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            &r#"@link(url: "https://specs.apollo.dev/foo/v1.0", import: [<IMPORT>])"#
                .replace("<IMPORT>", import),
            &r#"@composeDirective(name: "<NAME>")"#.replace("<NAME>", name),
            &r#"directive <NAME>(name: String!) on FIELD_DEFINITION"#.replace("<NAME>", name),
            &r#"<NAME>(name: "a")"#.replace("<NAME>", usage),
        );
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap_err();
        assert_eq!(result.len(), 1);
        let error = result.first().unwrap();
        assert_eq!(
            error.code().definition().code().to_string(),
            "DIRECTIVE_COMPOSITION_ERROR"
        );
        assert_eq!(
            error.to_string(),
            r#"Could not find matching directive definition for argument to @composeDirective "<NAME>" in subgraph "subgraphA". <SUGGESTION>"#
                .replace("<NAME>", name)
                .replace("<SUGGESTION>", suggestion)
        );
    }
}

mod composition {
    use super::*;

    #[test]
    fn composes_custom_tag_directive_when_renamed() {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", "@tag"])
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://custom.dev/tag/v1.0", import: [{ name: "@tag", as: "@mytag"}])
                @composeDirective(name: "@mytag")

            directive @mytag(name: String!, prop: String!) on FIELD_DEFINITION | OBJECT
            type Query {
                a: User
            }
            type User @key(fields: "id") {
                id: Int
                a: String @mytag(name: "a", prop: "b")
                b: String @tag(name: "c")
            }
        "#).unwrap();
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_has_directive_definition(
            &result,
            "directive @tag(name: String!) on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA",
        );
        assert_has_directive_definition(
            &result,
            "directive @mytag(name: String!, prop: String!) on FIELD_DEFINITION | OBJECT",
        );

        let schema = result.schema().schema().to_string();
        assert!(schema.contains(r#"a: String @mytag(name: "a", prop: "b")"#));
        assert!(schema.contains(r#"b: String @tag(name: "c")"#));
        assert!(schema.contains(
            r#"@link(url: "https://custom.dev/tag/v1.0", import: [{ name: "@tag", as: "@mytag"}])"#
        ));
    }

    #[test]
    fn composes_custom_tag_when_federation_tag_is_renamed() {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", {name: "@tag", as: "@mytag"}])
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://custom.dev/tag/v1.0", import: ["@tag"])
                @composeDirective(name: "@tag")

            directive @tag(name: String!, prop: String!) on FIELD_DEFINITION | OBJECT
                type Query {
                a: User
            }
            type User @key(fields: "id") {
                id: Int
                a: String @tag(name: "a", prop: "b")
                b: String @mytag(name: "c")
            }
        "#).unwrap();
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_has_directive_definition(
            &result,
            "directive @mytag(name: String!) on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA",
        );
        assert_has_directive_definition(
            &result,
            "directive @tag(name: String!, prop: String!) on FIELD_DEFINITION | OBJECT",
        );

        let schema = result.schema().schema().to_string();
        assert!(schema.contains(r#"a: String @tag(name: "a", prop: "b")"#));
        assert!(schema.contains(r#"b: String @mytag(name: "c")"#));
        assert!(schema.contains(r#"@link(url: "https://custom.dev/tag/v1.0", import: ["@tag"])"#));
    }

    #[test]
    fn composes_repeatable_custom_directives() {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
            extend schema @composeDirective(name: "@auth")
              @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", "@shareable"])
              @link(url: "https://custom.dev/auth/v1.0", import: ["@auth"])
            directive @auth(scope: String!) repeatable on FIELD_DEFINITION

            type Query {
              shared: String @shareable @auth(scope: "VIEWER")
            }
        "#).unwrap();
        let subgraph_b = Subgraph::parse("subgraphB", "", r#"
            extend schema @composeDirective(name: "@auth")
              @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", "@shareable"])
              @link(url: "https://custom.dev/auth/v1.0", import: ["@auth"])
            directive @auth(scope: String!) repeatable on FIELD_DEFINITION

            type Query {
              shared: String @shareable @auth(scope: "ADMIN")
            }
        "#).unwrap();

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        let schema = result.schema().schema().to_string();
        assert!(
            schema.contains(
                r#"shared: String @shareable @auth(scope: "VIEWER") @auth(scope: "ADMIN")"#
            )
        )
    }

    #[test]
    fn composes_custom_directive_with_nullable_array_arguments() {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
            extend schema @composeDirective(name: "@auth")
              @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", "@shareable"])
              @link(url: "https://custom.dev/auth/v1.0", import: ["@auth"])
            directive @auth(scope: [String!]) repeatable on FIELD_DEFINITION

            type Query {
              shared: String @shareable @auth(scope: "VIEWER")
            }
        "#).unwrap();
        let subgraph_b = Subgraph::parse("subgraphB", "", r#"
            extend schema @composeDirective(name: "@auth")
              @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", "@shareable"])
              @link(url: "https://custom.dev/auth/v1.0", import: ["@auth"])
            directive @auth(scope: [String!]) repeatable on FIELD_DEFINITION

            type Query {
              shared: String @shareable @auth
            }
        "#).unwrap();

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        let schema = result.schema().schema().to_string();
        assert!(schema.contains(r#"shared: String @shareable @auth(scope: ["VIEWER"]) @auth"#));
    }
}

fn generate_subgraph(
    name: &str,
    link_text: &str,
    compose_text: &str,
    directive_text: &str,
    usage: &str,
) -> Subgraph<Initial> {
    let schema = r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@composeDirective"])
            <LINK_TEXT>
            <COMPOSE_TEXT>

        <DIRECTIVE_TEXT>
        type Query {
            <NAME>: User
        }

        type User @key(fields: "id") {
            id: Int
            <NAME>: String <USAGE>
        }
    "#
    .replace("<LINK_TEXT>", link_text)
    .replace("<COMPOSE_TEXT>", compose_text)
    .replace("<DIRECTIVE_TEXT>", directive_text)
    .replace("<NAME>", name)
    .replace("<USAGE>", usage);

    Subgraph::parse(name, "", schema.as_str()).unwrap()
}

fn assert_has_directive_definition(
    supergraph: &Supergraph<Satisfiable>,
    expected_definition: &str,
) {
    let directive_name = expected_definition
        .chars()
        .skip_while(|x| *x != '@')
        .skip(1)
        .take_while(|x| *x != '(' && !x.is_whitespace())
        .collect::<String>();
    let directive_name = apollo_compiler::Name::new_unchecked(directive_name.as_str());
    let definition = supergraph
        .schema()
        .schema()
        .directive_definitions
        .get(&directive_name)
        .unwrap()
        .to_string();
    assert_eq!(definition, expected_definition)
}
