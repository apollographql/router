use apollo_federation::subgraph::SubgraphError;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::subgraph::typestate::Validated;

enum BuildOption {
    AsIs,
    AsFed2,
}

fn build_inner(
    schema_str: &str,
    build_option: BuildOption,
) -> Result<Subgraph<Validated>, SubgraphError> {
    let name = "S";
    let subgraph =
        Subgraph::parse(name, &format!("http://{name}"), schema_str).expect("valid schema");
    let subgraph = if matches!(build_option, BuildOption::AsFed2) {
        subgraph
            .into_fed2_subgraph()
            .map_err(|e| SubgraphError::new(name, e))?
    } else {
        subgraph
    };
    subgraph
        .expand_links()
        .map_err(|e| SubgraphError::new(name, e))?
        .validate(true)
}

fn build_and_validate(schema_str: &str) -> Subgraph<Validated> {
    build_inner(schema_str, BuildOption::AsIs).expect("expanded subgraph to be valid")
}

fn build_for_errors_with_option(schema: &str, build_option: BuildOption) -> Vec<(String, String)> {
    build_inner(schema, build_option)
        .expect_err("subgraph error was expected")
        .format_errors()
}

/// Build subgraph expecting errors, assuming fed 2.
fn build_for_errors(schema: &str) -> Vec<(String, String)> {
    build_for_errors_with_option(schema, BuildOption::AsFed2)
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
            "Mismatched error counts: {} != {}\n\nexpected:\n{}\n\nactual:\n{}",
            b.len(),
            a.len(),
            b.iter()
                .map(|(code, msg)| { format!("- {}: {}", code, msg) })
                .collect::<Vec<_>>()
                .join("\n"),
            a.iter()
                .map(|(code, msg)| { format!("+ {}: {}", code, msg) })
                .collect::<Vec<_>>()
                .join("\n"),
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
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
        let err = build_for_errors_with_option(schema_str, BuildOption::AsIs);

        assert_errors!(
            err,
            [(
                "PROVIDES_ON_NON_OBJECT_FIELD",
                r#"[S] Invalid @provides directive on field "Query.t": field has type "Int" which is not a Composite Type"#,
            )]
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
        // Just making sure this don't error out.
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
    use super::*;

    #[test]
    #[should_panic(expected = r#"subgraph error was expected: "#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected: "#)]
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
    #[should_panic(expected = r#"subgraph error was expected: "#)]
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
}

mod interface_object_and_key_on_interfaces_validation_tests {
    use super::*;

    #[test]
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"subgraph error was expected:"#)]
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
    #[should_panic(expected = r#"Mismatched errors:"#)]
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
    #[should_panic(expected = r#"Mismatched errors:"#)]
    fn rejects_cost_applications_on_interfaces() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])

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
    #[should_panic(expected = r#"Mismatched errors:"#)]
    fn rejects_applications_on_non_lists_unless_it_uses_sized_fields() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

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
    #[should_panic(expected = r#"Mismatched errors:"#)]
    fn rejects_negative_assumed_size() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

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
    #[should_panic(expected = r#"Mismatched error counts:"#)]
    fn rejects_slicing_arguments_not_in_field_arguments() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

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
    #[should_panic(expected = r#"Mismatched error counts:"#)]
    fn rejects_slicing_arguments_not_int_or_int_non_null() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

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
    #[should_panic(expected = r#"Mismatched errors:"#)]
    fn rejects_sized_fields_when_output_type_is_not_object() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

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
    #[should_panic(expected = r#"Mismatched errors:"#)]
    fn rejects_sized_fields_not_in_output_type() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

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
    #[should_panic(expected = r#"Mismatched errors:"#)]
    fn rejects_sized_fields_not_lists() {
        let doc = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

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
