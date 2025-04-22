use apollo_federation::error::FederationError;
use apollo_federation::subgraph::SubgraphError;
use apollo_federation::subgraph::typestate::Subgraph;

fn build_for_errors(schema: &str, err_reason: &str) -> Result<Vec<Vec<String>>, SubgraphError> {
    let err = Subgraph::parse("S", "", schema)
        .expect("parses schema")
        .expand_links()
        .expect("expands links")
        .validate(true)
        .expect_err(err_reason);

    match err.error {
        FederationError::SingleFederationError(e) => {
            let err_code_definition = e.code().definition();
            let res = vec![vec![
                err_code_definition.code().to_string(),
                err_code_definition.doc_description().to_string(),
            ]];
            Ok(res)
        }
        FederationError::MultipleFederationErrors(e) => {
            let mut res = Vec::new();
            for error in e.errors {
                let err_code_definition = error.code().definition();
                res.push(vec![
                    err_code_definition.code().to_string(),
                    err_code_definition.doc_description().to_string(),
                ]);
            }
            Ok(res)
        }
        FederationError::AggregateFederationError(e) => {
            let res = vec![vec![e.code, e.message]];
            Ok(res)
        }
    }
}

fn is_unordered_eq(a: &Vec<Vec<String>>, b: &[Vec<String>]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // TODO: Implement a more efficient unordered comparison
    for item in a {
        if !b.contains(item) {
            return false;
        }
    }
    true
}

mod fieldset_based_directives {
    use super::*;

    #[test]
    #[ignore]
    fn rejects_field_defined_with_arguments_in_key() {
        let schema_str = r#"
            type Query {		
                t: T		
            }				  		
            type T @key(fields: "f") {		
                f(x: Int): Int		
            }	
        "#;
        let err = build_for_errors(schema_str, "rejects field defined with arguments in @key");

        assert_eq!(
            err.unwrap(),
            vec![
                vec![
                "KEY_FIELDS_HAS_ARGS".to_string(),
                r#"[S] On type "T", for @key(fields: "f"): field T.f cannot be included because it has arguments (fields with argument are not allowed in @key)"#.to_string(),
            ]],
        );
    }

    #[test]
    #[ignore]
    fn rejects_field_defined_with_arguments_in_provides() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "f")
            }

            type T {
                f(x: Int): Int @external
            }
        "#;
        let err = build_for_errors(
            schema_str,
            "rejects field defined with arguments in @provides",
        );

        assert_eq!(
            err.unwrap(),
            vec![
                vec![
                "PROVIDES_FIELDS_HAS_ARGS".to_string(),
                r#"[S] On field "Query.t", for @provides(fields: "f"): field T.f cannot be included because it has arguments (fields with argument are not allowed in @provides)"#.to_string(),
            ]]
        );
    }

    #[test]
    #[ignore]
    fn rejects_provides_on_non_external_fields() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "f")
            }

            type T {
                f: Int
            }
        "#;
        let err = build_for_errors(schema_str, "rejects @provides on non-external fields");

        assert_eq!(
            err.unwrap(),
            vec![
                vec![
                "PROVIDES_FIELDS_MISSING_EXTERNAL".to_string(),
                r#"[S] On field "Query.t", for @provides(fields: "f"): field "T.f" should not be part of a @provides since it is already provided by this subgraph (it is not marked @external)"#.to_string(),
            ]]
        );
    }

    #[test]
    #[ignore]
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
        let err = build_for_errors(schema_str, "rejects @requires on non-external fields");

        assert_eq!(
            err.unwrap(),
            vec![
                vec![
                "REQUIRES_FIELDS_MISSING_EXTERNAL".to_string(), 
                r#"[S] On field "T.g", for @requires(fields: "f"): field "T.f" should not be part of a @requires since it is already provided by this subgraph (it is not marked @external)"#.to_string(),
            ]]
        );
    }

    #[test]
    #[ignore]
    fn rejects_key_on_interfaces_in_all_specs() {
        for version in ["2.0", "2.1", "2.2"].iter() {
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

            let err = build_for_errors(
                &schema_str,
                &format!("rejects @key on interfaces in the {} spec", version),
            );

            assert_eq!(
                err.unwrap(),
                vec![
                    vec![
                    "KEY_UNSUPPORTED_ON_INTERFACE".to_string(),
                    r#"[S] Cannot use @key on interface "T": @key is not yet supported on interfaces"#.to_string(),
                ]],
                "Test failed for version {}",
                version
            );
        }
    }

    #[test]
    #[ignore]
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
        let err = build_for_errors(schema_str, "rejects @provides on interfaces");

        assert_eq!(
            err.unwrap(),
            vec![
                vec![
                "PROVIDES_UNSUPPORTED_ON_INTERFACE".to_string(),
                r#"[S] Cannot use @provides on field "T.f" of parent type "T": @provides is not yet supported within interfaces"#.to_string(),
            ]]
        );
    }

    #[test]
    #[ignore]
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
        let err = build_for_errors(schema_str, "rejects @requires on interfaces");

        assert!(
            is_unordered_eq(
                &err.unwrap(),
                &[
                    vec![
                    "REQUIRES_UNSUPPORTED_ON_INTERFACE".to_string(),
                    r#"[S] Cannot use @requires on field "T.g" of parent type "T": @requires is not yet supported within interfaces"#.to_string(),
                    ],
                    vec![
                    "EXTERNAL_ON_INTERFACE".to_string(),
                    r#"[S] Interface type field "T.f" is marked @external but @external is not allowed on interface fields (it is nonsensical)."#.to_string(),
                    ],
                ]
            )
        );
    }

    #[test]
    #[ignore]
    fn rejects_unused_external() {
        let schema_str = r#"
            type Query {
                t: T
            }

            type T {
                f: Int @external
            }
        "#;
        let err = build_for_errors(schema_str, "rejects unused @external");

        assert_eq!(
            err.unwrap(),
            vec![
                vec![
                "EXTERNAL_UNUSED".to_string(),
                r#"[S] Field "T.f" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#.to_string(),
            ]]
        );
    }

    #[test]
    #[ignore]
    fn rejects_provides_on_non_object_fields() {
        let schema_str = r#"
            type Query {
                t: T @provides(fields: "f")
            }

            type T {
                f: Int
            }
        "#;
        let err = build_for_errors(schema_str, "rejects @provides on non-object fields");

        assert_eq!(
            err.unwrap(),
            vec![
                vec![
                "PROVIDES_ON_NON_OBJECT_FIELD".to_string(),
                r#"[S] Invalid @provides directive on field "Query.t": field has type "Int" which is not a Composite Type"#.to_string(),
            ]]
        );
    }
}
