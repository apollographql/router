use super::ServiceDefinition;
use super::assert_hints_equal;
use super::compose_as_fed2_subgraphs;
use super::print_sdl;

#[cfg(test)]
mod tests {
    use apollo_compiler::coord;
    use apollo_federation::supergraph::CompositionHint;
    use test_log::test;

    use super::*;

    /* The following argument merging strategies are not currently being used by any
       public-facing directives and thus are not represented in this set of tests:
       - min
       - intersection
    */

    #[test]
    fn works_for_max_argument_merge_strategy() {
        // NOTE: @cost uses the MAX strategy for merging arguments

        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    t: T
                }

                type T
                  @key(fields: "k")
                  @cost(weight: 3)
                {
                    k: ID @cost(weight: 1)
                }
                "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T
                  @key(fields: "k")
                  @cost(weight: 2)
                {
                    k: ID @cost(weight: 5)
                    a: Int
                    b: String @cost(weight: 4)
                }
                "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let result_sg = result.expect("Composition should succeed");

        // Check expected hints
        let expected_hints = vec![
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @cost is applied to "T" in multiple subgraphs with different arguments. Merging strategies used by arguments: { weight: MAX }"#,
                ),
                locations: Vec::new(),
            },
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @cost is applied to "T.k" in multiple subgraphs with different arguments. Merging strategies used by arguments: { weight: MAX }"#,
                ),
                locations: Vec::new(),
            },
        ];
        assert_hints_equal(result_sg.hints(), &expected_hints);

        // Check merging using MAX succeeded
        let schema = result_sg.schema().schema();
        let sdl = print_sdl(schema);
        assert!(sdl.contains(r#"@link(url: "https://specs.apollo.dev/cost/v0.1")"#));

        let t = coord!(T)
            .lookup(schema)
            .expect("T should be defined on the supergraph");
        let t_cost_directive = t
            .directives()
            .iter()
            .find(|d| d.name == "cost")
            .expect("@cost directive should be present on T");
        assert_eq!(t_cost_directive.to_string(), r#"@cost(weight: 3)"#);

        let k = coord!(T.k)
            .lookup_field(schema)
            .expect("T.k should be defined on the supergraph");
        let k_cost_directive = k
            .directives
            .iter()
            .find(|d| d.name == "cost")
            .expect("@cost directive should be present on T.k");
        assert_eq!(k_cost_directive.to_string(), r#"@cost(weight: 5)"#);

        let b = coord!(T.b)
            .lookup_field(schema)
            .expect("T.b should be defined on the supergraph");
        let b_cost_directive = b
            .directives
            .iter()
            .find(|d| d.name == "cost")
            .expect("@cost directive should be present on T.b");
        assert_eq!(b_cost_directive.to_string(), r#"@cost(weight: 4)"#);
    }

    #[test]
    fn works_for_union_argument_merge_strategy() {
        // NOTE: @requiresScopes uses the UNION strategy for merging arguments

        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
            type Query {
              t: T!
            }

            type T
              @key(fields: "k")
              @requiresScopes(scopes: ["foo", "bar"])
            {
              k: ID @requiresScopes(scopes: [])
            }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
            type T
              @key(fields: "k")
              @requiresScopes(scopes: ["foo"])
            {
              k: ID @requiresScopes(scopes: ["v1", "v2"])
              a: Int
              b: String @requiresScopes(scopes: ["x"])
            }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let result_sg = result.expect("Composition should succeed");

        // Check expected hints
        let expected_hints = vec![
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @requiresScopes is applied to "T" in multiple subgraphs with different arguments. Merging strategies used by arguments: { scopes: UNION }"#,
                ),
                locations: Vec::new(),
            },
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @requiresScopes is applied to "T.k" in multiple subgraphs with different arguments. Merging strategies used by arguments: { scopes: UNION }"#,
                ),
                locations: Vec::new(),
            },
        ];
        assert_hints_equal(result_sg.hints(), &expected_hints);

        // Check merging using UNION succeeded
        let schema = result_sg.schema().schema();
        let sdl = print_sdl(schema);
        assert!(sdl.contains(
            r#"@link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)"#
        ));

        let t = coord!(T)
            .lookup(schema)
            .expect("T should be defined on the supergraph");
        let t_requires_scopes_directive = t
            .directives()
            .iter()
            .find(|d| d.name == "requiresScopes")
            .expect("@requiresScopes directive should be present on T");
        assert_eq!(
            t_requires_scopes_directive.to_string(),
            r#"@requiresScopes(scopes: ["foo", "bar"])"#
        );

        let k = coord!(T.k)
            .lookup_field(schema)
            .expect("T.k should be defined on the supergraph");
        let k_requires_scopes_directive = k
            .directives
            .iter()
            .find(|d| d.name == "requiresScopes")
            .expect("@requiresScopes directive should be present on T.k");
        assert_eq!(
            k_requires_scopes_directive.to_string(),
            r#"@requiresScopes(scopes: ["v1", "v2"])"#
        );

        let b = coord!(T.b)
            .lookup_field(schema)
            .expect("T.b should be defined on the supergraph");
        let b_requires_scopes_directive = b
            .directives
            .iter()
            .find(|d| d.name == "requiresScopes")
            .expect("@requiresScopes directive should be present on T.b");
        assert_eq!(
            b_requires_scopes_directive.to_string(),
            r#"@requiresScopes(scopes: ["x"])"#
        )
    }

    #[test]
    fn works_for_nullable_and_argument_merge_strategy() {
        // NOTE: @listSize(requireOneSlicingArgument:) is merged using the NULLABLE_AND strategy

        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    t: [T!] @shareable @listSize(requireOneSlicingArgument: false)
                }

                type T @key(fields: "k") {
                    k: [ID] @listSize(requireOneSlicingArgument: true)
                    c: [Int]! @shareable @listSize(requireOneSlicingArgument: null)
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type Query {
                    t: [T!] @shareable @listSize(requireOneSlicingArgument: true)
                }

                type T @key(fields: "k") {
                    k: [ID] @listSize(requireOneSlicingArgument: null)
                    a: String
                    b: [Int] @listSize(requireOneSlicingArgument: false)
                    c: [Int]! @shareable @listSize(requireOneSlicingArgument: null)
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let result_sg = result.expect("Composition should succeed");

        // Check expected hints
        let expected_hints = vec![
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @listSize is applied to "Query.t" in multiple subgraphs with different arguments. Merging strategies used by arguments: { assumedSize: NULLABLE_MAX, slicingArguments: NULLABLE_UNION, sizedFields: NULLABLE_UNION, requireOneSlicingArgument: NULLABLE_AND }"#,
                ),
                locations: Vec::new(),
            },
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @listSize is applied to "T.k" in multiple subgraphs with different arguments. Merging strategies used by arguments: { assumedSize: NULLABLE_MAX, slicingArguments: NULLABLE_UNION, sizedFields: NULLABLE_UNION, requireOneSlicingArgument: NULLABLE_AND }"#,
                ),
                locations: Vec::new(),
            },
        ];
        assert_hints_equal(result_sg.hints(), &expected_hints);

        // Check merging using NULLABLE_AND succeeded
        let schema = result_sg.schema().schema();
        let sdl = print_sdl(schema);
        assert!(sdl.contains(
            r#"@link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])"#
        ));

        let t = coord!(Query.t)
            .lookup_field(schema)
            .expect("Query.t should be defined on the supergraph");
        let t_requires_scopes_directive = t
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T");
        assert_eq!(
            t_requires_scopes_directive.to_string(),
            r#"@listSize(requireOneSlicingArgument: false)"#
        );

        let k = coord!(T.k)
            .lookup_field(schema)
            .expect("T.k should be defined on the supergraph");
        let k_requires_scopes_directive = k
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T.k");
        assert_eq!(
            k_requires_scopes_directive.to_string(),
            r#"@listSize(requireOneSlicingArgument: true)"#
        );

        let b = coord!(T.b)
            .lookup_field(schema)
            .expect("T.b should be defined on the supergraph");
        let b_requires_scopes_directive = b
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T.b");
        assert_eq!(
            b_requires_scopes_directive.to_string(),
            r#"@listSize(requireOneSlicingArgument: false)"#
        );

        let c = coord!(T.c)
            .lookup_field(schema)
            .expect("T.b should be defined on the supergraph");
        let c_requires_scopes_directive = c
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T.c");
        assert_eq!(
            c_requires_scopes_directive.to_string(),
            r#"@listSize(requireOneSlicingArgument: null)"#
        );
    }

    #[test]
    fn works_for_nullable_max_argument_merge_strategy() {
        // NOTE: @listSize(assumedSize:) is merged using the NULLABLE_MAX strategy

        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    t: [T!] @shareable @listSize(assumedSize: 20)
                }

                type T @key(fields: "k") {
                    k: [ID] @listSize(assumedSize: 1)
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type Query {
                    t: [T!] @shareable @listSize(assumedSize: 10)
                }

                type T @key(fields: "k") {
                    k: [ID] @listSize(assumedSize: 3)
                    a: String
                    b: [Int] @listSize(assumedSize: null)
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let result_sg = result.expect("Composition should succeed");

        // Check expected hints
        let expected_hints = vec![
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @listSize is applied to "Query.t" in multiple subgraphs with different arguments. Merging strategies used by arguments: { assumedSize: NULLABLE_MAX, slicingArguments: NULLABLE_UNION, sizedFields: NULLABLE_UNION, requireOneSlicingArgument: NULLABLE_AND }"#,
                ),
                locations: Vec::new(),
            },
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @listSize is applied to "T.k" in multiple subgraphs with different arguments. Merging strategies used by arguments: { assumedSize: NULLABLE_MAX, slicingArguments: NULLABLE_UNION, sizedFields: NULLABLE_UNION, requireOneSlicingArgument: NULLABLE_AND }"#,
                ),
                locations: Vec::new(),
            },
        ];
        assert_hints_equal(result_sg.hints(), &expected_hints);

        // Check merging using NULLABLE_MAX succeeded
        let schema = result_sg.schema().schema();
        let sdl = print_sdl(schema);
        assert!(sdl.contains(
            r#"@link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])"#
        ));

        let t = coord!(Query.t)
            .lookup_field(schema)
            .expect("Query.t should be defined on the supergraph");
        let t_requires_scopes_directive = t
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T");
        assert_eq!(
            t_requires_scopes_directive.to_string(),
            r#"@listSize(assumedSize: 20, requireOneSlicingArgument: true)"#
        );

        let k = coord!(T.k)
            .lookup_field(schema)
            .expect("T.k should be defined on the supergraph");
        let k_requires_scopes_directive = k
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T.k");
        assert_eq!(
            k_requires_scopes_directive.to_string(),
            r#"@listSize(assumedSize: 3, requireOneSlicingArgument: true)"#
        );

        let b = coord!(T.b)
            .lookup_field(schema)
            .expect("T.b should be defined on the supergraph");
        let b_requires_scopes_directive = b
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T.b");
        assert_eq!(
            b_requires_scopes_directive.to_string(),
            r#"@listSize(assumedSize: null, requireOneSlicingArgument: true)"#
        );
    }

    #[test]
    fn works_for_nullable_union_argument_merge_strategy() {
        // NOTE: @listSize(slicingArguments: & sizedFields:) are merged using the NULLABLE_UNION strategy

        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    t: [T!] @shareable
                }

                type Cursor @shareable {
                    page: [Item!]
                    nextPageToken: [String]
                }

                type Item @key(fields: "id") {
                  id: ID
                }

                type T @key(fields: "a") @shareable {
                    k(first: Int): [ID] @listSize(slicingArguments: ["first"])
                    a: Int
                    b: Cursor! @listSize(sizedFields: ["page"])
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type Cursor @shareable {
                    page: [Item!]
                    nextPageToken: [String]
                }

                type Item @key(fields: "id") {
                    id: ID
                }

                type T @key(fields: "a") @shareable {
                    k(last: Int): [ID] @listSize(slicingArguments: ["last"])
                    a: Int
                    b: Cursor! @listSize(sizedFields: ["nextPageToken"])
                    c: Cursor! @listSize(sizedFields: ["nextPageToken"])
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let result_sg = result.expect("Composition should succeed");

        // Check expected hints
        let expected_hints = vec![
            CompositionHint {
                code: String::from("INCONSISTENT_ARGUMENT_PRESENCE"),
                message: String::from(
                    r#"Optional argument "T.k(first:)" will not be included in the supergraph as it does not appear in all subgraphs: it is defined in subgraph "Subgraph1" but not in subgraph "Subgraph2"."#,
                ),
                locations: Vec::new(),
            },
            CompositionHint {
                code: String::from("INCONSISTENT_ARGUMENT_PRESENCE"),
                message: String::from(
                    r#"Optional argument "T.k(last:)" will not be included in the supergraph as it does not appear in all subgraphs: it is defined in subgraph "Subgraph2" but not in subgraph "Subgraph1"."#,
                ),
                locations: Vec::new(),
            },
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @listSize is applied to "T.k" in multiple subgraphs with different arguments. Merging strategies used by arguments: { assumedSize: NULLABLE_MAX, slicingArguments: NULLABLE_UNION, sizedFields: NULLABLE_UNION, requireOneSlicingArgument: NULLABLE_AND }"#,
                ),
                locations: Vec::new(),
            },
            CompositionHint {
                code: String::from("MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS"),
                message: String::from(
                    r#"Directive @listSize is applied to "T.b" in multiple subgraphs with different arguments. Merging strategies used by arguments: { assumedSize: NULLABLE_MAX, slicingArguments: NULLABLE_UNION, sizedFields: NULLABLE_UNION, requireOneSlicingArgument: NULLABLE_AND }"#,
                ),
                locations: Vec::new(),
            },
        ];
        assert_hints_equal(result_sg.hints(), &expected_hints);

        // Check merging using NULLABLE_UNION succeeded
        let schema = result_sg.schema().schema();
        let sdl = print_sdl(schema);
        assert!(sdl.contains(
            r#"@link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])"#
        ));

        coord!(Query.t)
            .lookup_field(schema)
            .expect("Query.t should be defined on the supergraph");

        let k = coord!(T.k)
            .lookup_field(schema)
            .expect("T.k should be defined on the supergraph");
        let k_requires_scopes_directive = k
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T.k");
        assert_eq!(
            k_requires_scopes_directive.to_string(),
            r#"@listSize(slicingArguments: ["first", "last"], requireOneSlicingArgument: true)"#
        );

        let b = coord!(T.b)
            .lookup_field(schema)
            .expect("T.b should be defined on the supergraph");
        let b_requires_scopes_directive = b
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T.b");
        assert_eq!(
            b_requires_scopes_directive.to_string(),
            r#"@listSize(sizedFields: ["page", "nextPageToken"], requireOneSlicingArgument: true)"#
        );

        let c = coord!(T.c)
            .lookup_field(schema)
            .expect("T.b should be defined on the supergraph");
        let c_requires_scopes_directive = c
            .directives
            .iter()
            .find(|d| d.name == "listSize")
            .expect("@listSize directive should be present on T.c");
        assert_eq!(
            c_requires_scopes_directive.to_string(),
            r#"@listSize(sizedFields: ["nextPageToken"], requireOneSlicingArgument: true)"#
        );
    }
}
