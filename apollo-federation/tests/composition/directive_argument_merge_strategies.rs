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
    use crate::composition::assert_composition_errors;

    /* The following argument merging strategies are not currently being used by any
       public-facing directives and thus are not represented in this set of tests:
       - min
       - intersection
       - nullable_and
       - nullable_max
       - nullable_union
    */

    #[test]
    fn works_for_max_argument_merge_strategy() {
        // NOTE: @cost uses the MAX strategy for merging arguments,
        // So we'll use it as a proxy for the private @max directive

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
        // NOTE: @requiresScopes uses the UNION strategy for merging arguments,
        // So we'll use it as a proxy for the private @union directive

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
    fn errors_when_declaring_strategy_that_does_not_match_the_argument_type() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                extend schema @link(url: "https://specs.apollo.dev/foo/v0.1")

                type Query {
                    t: T
                }

                type T {
                    v: String @foo(value: "bar")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                extend schema @link(url: "https://specs.apollo.dev/foo/v0.1")

                type T {
                    v: String @foo(value: "bar")
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let expected_errors = [(
            "DIRECTIVE_DEFINITION_INVALID",
            r#"Invalid composition strategy MAX for argument @foo(value:) of type String; MAX only supports type(s) Int!"#,
        )];
        assert_composition_errors(&result, &expected_errors);
    }
}
