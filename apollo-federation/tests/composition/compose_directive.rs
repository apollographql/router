use apollo_compiler::Name;
use apollo_compiler::coord;
use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Initial;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::supergraph::Satisfiable;
use apollo_federation::supergraph::Supergraph;
use rstest::rstest;

mod simple_cases {
    use super::*;

    #[test]
    fn simple_success_case() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
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

        let schema = result.schema().schema();
        assert!(
            schema
                .to_string()
                .contains(r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#),
            "Schema does not contain expected @link directive"
        );

        let subgraph_a_field = coord!(User.subgraphA).lookup_field(schema).unwrap();
        let foo_directive = subgraph_a_field
            .directives
            .iter()
            .find(|d| d.name == "foo")
            .expect("Expected @foo directive to be present on User.subgraphA");
        assert_eq!(foo_directive.to_string(), r#"@foo(name: "a")"#);
    }

    #[test]
    fn simple_success_case_no_import() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0")"#,
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

        let schema = result.schema().schema();
        assert!(
            schema.to_string().contains(r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: [{name: "@bar", as: "@foo__bar"}])"#),
            "Schema does not contain expected @link directive"
        );

        let subgraph_a_field = coord!(User.subgraphA).lookup_field(schema).unwrap();
        let foo_bar_directive = subgraph_a_field
            .directives
            .iter()
            .find(|d| d.name == "foo__bar")
            .expect("Expected @foo__bar directive to be present on User.subgraphA");
        assert_eq!(foo_bar_directive.to_string(), r#"@foo__bar(name: "a")"#);
    }

    #[test]
    fn simple_success_case_renamed_compose_directive() {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
      extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", { name: "@composeDirective", as: "@apolloCompose" }])
        @link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])
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
        .unwrap();
        let subgraph_b = generate_subgraph("subgraphB", "", "", "", "");

        let result = compose(vec![subgraph_a, subgraph_b]).unwrap();
        assert_has_directive_definition(
            &result,
            "directive @foo(name: String!) on FIELD_DEFINITION",
        );

        let schema = result.schema().schema();
        assert!(
            schema
                .to_string()
                .contains(r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#),
            "Schema does not contain expected @link directive"
        );

        let user_a_field = coord!(User.a).lookup_field(schema).unwrap();
        let foo_directive = user_a_field
            .directives
            .iter()
            .find(|d| d.name == "foo")
            .expect("Expected @foo directive to be present on User.a");
        assert_eq!(foo_directive.to_string(), r#"@foo(name: "a")"#);
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
                "Directive \"{directive}\" should not be explicitly manually composed since it is a federation directive composed by default"
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
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/federation/v2.5", import: [{ name: "@key" }, { name: "@composeDirective" } , { name: "<DIRECTIVE>", as: "@apolloDirective" }])
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
                "Directive \"@apolloDirective\" should not be explicitly manually composed since it is a federation directive composed by default"
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

    // TODO: We're not handling importing the same directive twice in the same way as JS
    #[rstest]
    // #[case("@key")]
    #[case("@requires")]
    #[case("@provides")]
    #[case("@external")]
    #[case("@extends")]
    #[case("@shareable")]
    #[case("@override")]
    // #[case("@composeDirective")]
    fn errors_for_renamed_federation_directives_with_nontrivial_compositions(
        #[case] directive: &str,
    ) {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
      extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/federation/v2.1", import: [{ name: "@key" }, { name: "@composeDirective" } , { name: "<DIRECTIVE>", as: "@apolloDirective" }])
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
            r#"@link(url: "https://specs.custom.dev/foo/v5.0", import: ["@foo"])"#,
            "",
            r#"directive @foo(arg: String!) on FIELD_DEFINITION"#,
            r#"@foo(arg: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            r#"subgraphB"#,
            r#"@link(url: "https://specs.custom.dev/foo/v2.0", import: ["@foo"])"#,
            "",
            r#"directive @foo(arg: String!) on FIELD_DEFINITION"#,
            r#"@foo(arg: "b")"#,
        );
        let subgraph_c = generate_subgraph(
            r#"subgraphC"#,
            r#"@link(url: "https://specs.custom.dev/foo/v3.0", import: ["@foo"])"#,
            "",
            r#"directive @foo(arg: String!) on FIELD_DEFINITION"#,
            r#"@foo(arg: "")"#,
        );
        let subgraph_d = generate_subgraph(
            r#"subgraphD"#,
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
            "",
            r#"directive @foo(arg: String!) on FIELD_DEFINITION"#,
            r#"@foo(arg: "b")"#,
        );

        let result = compose(vec![subgraph_a, subgraph_b, subgraph_c, subgraph_d]).unwrap();
        assert_eq!(result.hints().len(), 1);
        let hint = result.hints().first().unwrap();
        assert_eq!(hint.code, "DIRECTIVE_COMPOSITION_INFO");
        assert_eq!(
            hint.message,
            r#"Non-composed core feature "https://specs.custom.dev/foo" has major version mismatch across subgraphs"#
        );
    }

    #[rstest]
    #[case(r#"@link(url: "https://specs.custom.dev/foo/v2.0", import: ["@foo"])"#)]
    #[case(r#"@link(url: "https://specs.custom.dev/foo/v2.0", import: ["@bar"])"#)]
    fn errors_when_mismatched_major_versions_are_composed(#[case] link_text: &str) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
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
            r#"Core feature "https://specs.custom.dev/foo" requested to be merged has major version mismatch across subgraphs"#
        );
    }

    #[rstest]
    #[case(
        r#"@composeDirective(name: "@foo")"#,
        "https://specs.custom.dev/foo/v1.4",
        "directive @foo(name: String!) on FIELD_DEFINITION | OBJECT"
    )]
    #[case(
        "",
        "https://specs.custom.dev/foo/v1.0",
        "directive @foo(name: String!) on FIELD_DEFINITION"
    )]
    fn composes_mismatched_versions_with_latest_used_definition(
        #[case] compose_text_newer_link: &str,
        #[case] expected_link: &str,
        #[case] expected_definition: &str,
    ) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.custom.dev/foo/v1.4", import: ["@foo"])"#,
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
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            directive_text,
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.custom.dev/foo/v1.1", import: ["@bar"])"#,
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

        let schema = result.schema().schema();

        let subgraph_a_field = coord!(User.subgraphA).lookup_field(schema).unwrap();
        let foo_directive = subgraph_a_field
            .directives
            .iter()
            .find(|d| d.name == "foo")
            .expect("Expected @foo directive to be present on User.subgraphA");
        assert_eq!(foo_directive.to_string(), r#"@foo(name: "a")"#);

        let subgraph_b_field = coord!(User.subgraphB).lookup_field(schema).unwrap();
        let bar_directive = subgraph_b_field
            .directives
            .iter()
            .find(|d| d.name == "bar")
            .expect("Expected @bar directive to be present on User.subgraphB");
        assert_eq!(bar_directive.to_string(), r#"@bar(name: "b")"#);
    }

    #[test]
    fn hints_when_imported_with_mismatched_name_but_not_exported() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo", { name: "@bar", as: "@baz" }])"#,
            r#"@composeDirective(name: "@foo")"#,
            r#"
            directive @foo(name: String!) on FIELD_DEFINITION
            directive @baz(name: String!, address: String) on FIELD_DEFINITION | OBJECT
            "#,
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.custom.dev/foo/v1.1", import: ["@bar"])"#,
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

        let schema = result.schema().schema();

        let subgraph_a_field = coord!(User.subgraphA).lookup_field(schema).unwrap();
        let foo_directive = subgraph_a_field
            .directives
            .iter()
            .find(|d| d.name == "foo")
            .expect("Expected @foo directive to be present on User.subgraphA");
        assert_eq!(foo_directive.to_string(), r#"@foo(name: "a")"#);

        let subgraph_b_field = coord!(User.subgraphB).lookup_field(schema).unwrap();
        let bar_directive = subgraph_b_field
            .directives
            .iter()
            .find(|d| d.name == "bar")
            .expect("Expected @bar directive to be present on User.subgraphB");
        assert_eq!(bar_directive.to_string(), r#"@bar(name: "b")"#);
    }

    #[ignore = "validation not yet implemented - needs to check that @composeDirective references exist in linked spec"]
    #[test]
    fn errors_when_exported_but_undefined() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.custom.dev/foo/v1.1", import: ["@bar"])"#,
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
            r#"Core feature "https://specs.custom.dev/foo" in subgraph "subgraphA" does not have a directive definition for "@bar""#,
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
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: [{ name: "@foo", as: "@bar" }])"#,
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

        // There's some non-determinism in the serialization order here. We'll need to figure that
        // out, but for now we just check both orders.
        assert!(
            &[
                r#"Composed directive is not named consistently in all subgraphs but "@foo" in subgraph "subgraphA" and "@bar" in subgraph "subgraphB""#.to_string(),
                r#"Composed directive is not named consistently in all subgraphs but "@bar" in subgraph "subgraphB" and "@foo" in subgraph "subgraphA""#.to_string(),
            ].contains(&error.to_string()),
            "Unexpected error message: {error}",
        );
    }

    #[test]
    fn errors_when_exported_directive_is_imported_from_different_specs() {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphB",
            r#"@link(url: "https://specs.custom.dev/bar/v1.0", import: ["@foo"])"#,
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
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: ["@foo"])"#,
            r#"@composeDirective(name: "@foo")"#,
            "directive @foo(name: String!) on FIELD_DEFINITION",
            r#"@foo(name: "a")"#,
        );
        let subgraph_b = generate_subgraph(
            "subgraphA",
            r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: [{ name: "@bar", as: "@foo" }])"#,
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
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective"])
                @link(url: "https://specs.custom.dev/foo/v1.0", import: [{ name: "@foo", as: "@inaccessible" }])
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
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", "@inaccessible"])

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

    /*
    * We need to understand why this test was set up this way in the original source. It explicitly
    * adds a definition for the `@join__x` directive that it's defining (as an alias for `@foo`).
    * So, the error saying it isn't part of a core feature, when it's clearly linked, seems wrong.
    * Maybe JS silently ignores definitions starting with `@join__`?
    *
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
            &r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: [{ name: "@foo", as: "<DIRECTIVE>" }])"#.replace("<DIRECTIVE>", directive),
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
    */
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
            r#"Argument to @composeDirective "foo" in subgraph "subgraphA" must have a leading "@""#
        );
    }

    #[rstest]
    #[case(
        r#""@foo""#,
        "@foo",
        "@fooz",
        r#"[subgraphA] Error: cannot find directive `@fooz` in this document
    ╭─[ subgraphA:14:31 ]
    │
 14 │             subgraphA: String @fooz(name: "a")
    │                               ────────┬───────  
    │                                       ╰───────── directive not defined
────╯
Did you mean "@foo"?
"#
    )]
    #[case(
        r#"{ name: "@foo", as: "@bar" }"#,
        "@bar",
        "@barz",
        r#"[subgraphA] Error: cannot find directive `@barz` in this document
    ╭─[ subgraphA:14:31 ]
    │
 14 │             subgraphA: String @barz(name: "a")
    │                               ────────┬───────  
    │                                       ╰───────── directive not defined
────╯
Did you mean "@bar"?
"#
    )]
    fn errors_when_directive_does_not_exist(
        #[case] import: &str,
        #[case] name: &str,
        #[case] usage: &str,
        #[case] expected_message: &str,
    ) {
        let subgraph_a = generate_subgraph(
            "subgraphA",
            &r#"@link(url: "https://specs.custom.dev/foo/v1.0", import: [<IMPORT>])"#
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
            "INVALID_GRAPHQL"
        );
        assert_eq!(error.to_string(), expected_message);
    }
}

mod composition {
    use super::*;

    #[ignore = "needs implementation - should allow custom @tag alongside federation @tag when federation one is not renamed"]
    #[test]
    fn composes_custom_tag_directive_when_renamed() {
        let subgraph_a = Subgraph::parse("subgraphA", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/federation/v2.1", import: ["@key", "@composeDirective", "@tag"])
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

        let schema = result.schema().schema();

        let field_a = coord!(User.a).lookup_field(schema).unwrap();
        let mytag_directive = field_a
            .directives
            .iter()
            .find(|d| d.name == "mytag")
            .expect("Expected @mytag directive on User.a");
        assert_eq!(
            mytag_directive.to_string(),
            r#"@mytag(name: "a", prop: "b")"#
        );

        let field_b = coord!(User.b).lookup_field(schema).unwrap();
        let tag_directive = field_b
            .directives
            .iter()
            .find(|d| d.name == "tag")
            .expect("Expected @tag directive on User.b");
        assert_eq!(tag_directive.to_string(), r#"@tag(name: "c")"#);

        assert!(schema.to_string().contains(
            r#"@link(url: "https://custom.dev/tag/v1.0", import: [{ name: "@tag", as: "@mytag"}])"#
        ));
    }

    #[ignore = "needs implementation - should allow custom @tag when federation @tag is renamed to different name"]
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

        let schema = result.schema().schema();

        let field_a = coord!(User.a).lookup_field(schema).unwrap();
        let tag_directive = field_a
            .directives
            .iter()
            .find(|d| d.name == "tag")
            .expect("Expected @tag directive on User.a");
        assert_eq!(tag_directive.to_string(), r#"@tag(name: "a", prop: "b")"#);

        let field_b = coord!(User.b).lookup_field(schema).unwrap();
        let mytag_directive = field_b
            .directives
            .iter()
            .find(|d| d.name == "mytag")
            .expect("Expected @mytag directive on User.b");
        assert_eq!(mytag_directive.to_string(), r#"@mytag(name: "c")"#);

        assert!(
            schema
                .to_string()
                .contains(r#"@link(url: "https://custom.dev/tag/v1.0", import: ["@tag"])"#)
        );
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
        let schema = result.schema().schema();

        let shared_field = coord!(Query.shared).lookup_field(schema).unwrap();
        let auth_directives: Vec<_> = shared_field
            .directives
            .iter()
            .filter(|d| d.name == "auth")
            .collect();

        assert_eq!(
            auth_directives.len(),
            2,
            "Expected 2 @auth directives on Query.shared"
        );

        assert_eq!(auth_directives[0].to_string(), r#"@auth(scope: "VIEWER")"#);
        assert_eq!(auth_directives[1].to_string(), r#"@auth(scope: "ADMIN")"#);
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
        let schema = result.schema().schema();

        let shared_field = coord!(Query.shared).lookup_field(schema).unwrap();
        let auth_directives: Vec<_> = shared_field
            .directives
            .iter()
            .filter(|d| d.name == "auth")
            .collect();

        assert_eq!(
            auth_directives.len(),
            2,
            "Expected 2 @auth directives on Query.shared"
        );

        assert_eq!(auth_directives[0].to_string(), r#"@auth(scope: "VIEWER")"#);
        assert_eq!(auth_directives[1].to_string(), "@auth");
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

    Subgraph::parse(name, "", schema.as_str())
        .unwrap()
        .into_fed2_test_subgraph(true, false)
        .unwrap()
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
    let directive_name = Name::new_unchecked(directive_name.as_str());
    let definition = supergraph
        .schema()
        .schema()
        .directive_definitions
        .get(&directive_name)
        .unwrap()
        .to_string();
    assert_eq!(definition, expected_definition)
}
