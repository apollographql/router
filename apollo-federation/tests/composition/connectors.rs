use apollo_federation::ApiSchemaOptions;
use apollo_federation::connectors::expand::ExpansionResult;
use apollo_federation::connectors::expand::expand_connectors;
use insta::assert_snapshot;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_spec_and_join_directive_composes() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @r###"
        type Query {
          resources: [Resource!]!
        }

        type Resource {
          id: ID!
          name: String!
        }
        "###);
    }

    #[test]
    fn does_not_require_importing_connect() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @r###"
        type Query {
          resources: [Resource!]!
        }

        type Resource {
          id: ID!
          name: String!
        }
        "###);
    }

    #[test]
    fn using_as_alias() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    as: "http"
                    import: ["@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    resources: [Resource!]!
                    @http(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @r###"
        type Query {
          resources: [Resource!]!
        }

        type Resource {
          id: ID!
          name: String!
        }
        "###);
    }

    #[test]
    fn composes_v0_2() {
        let with_connectors_v0_2 = ServiceDefinition {
            name: "with-connectors-v0_2",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.2"
                    import: ["@connect", "@source"]
                )
                @source(
                  name: "v1"
                  http: {
                    baseURL: "http://v1"
                    path: ""
                    queryParams: ""
                  }
                  errors: { message: "" extensions: "" }
                )

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource @key(fields: "id")
                  @connect(
                    source: "v1"
                    http: {
                      GET: "/resources"
                      path: ""
                      queryParams: ""
                    }
                    batch: { maxSize: 5 }
                    errors: { message: "" extensions: "" }
                    selection: ""
                  ) {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let with_connectors_v0_1 = ServiceDefinition {
            name: "with-connectors-v0_1",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    widgets: [Widget!]!
                    @connect(source: "v1", http: { GET: "/widgets" }, selection: "")
                }

                type Widget @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors_v0_2, with_connectors_v0_1]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @"
        type Query {
          widgets: [Widget!]!
          resources: [Resource!]!
        }

        type Widget {
          id: ID!
          name: String!
        }

        type Resource {
          id: ID!
          name: String!
        }
        ");
    }

    #[test]
    fn composes_with_renames() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    as: "http"
                    import: [
                        { name: "@connect", as: "@http" }
                        { name: "@source", as: "@api" }
                    ]
                )
                @api(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    resources: [Resource!]!
                    @http(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @r###"
        type Query {
          resources: [Resource!]!
        }

        type Resource {
          id: ID!
          name: String!
        }
        "###);
    }
    #[test]
    #[ignore]
    fn requires_the_http_arg_for_source() {
        let subgraphs = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1")

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraphs]);

        // This should fail with error: [with-connectors] Directive "@source" argument "http"
        // of type "connect__SourceHTTP!" is required, but it was not provided.
        assert!(
            result.is_err(),
            "Composition should fail due to missing http argument in @source directive"
        );

        let errors = result.unwrap_err().errors;
        // Check that we have exactly 1 error
        assert_eq!(errors.len(), 1, "Should have exactly 1 error");

        let error = &errors[0];
        let error_message = format!("{:?}", error);

        // Check for the specific error message
        let expected_message = "[with-connectors] Directive \"@source\" argument \"http\" of type \"connect__SourceHTTP!\" is required, but it was not provided.";
        assert!(
            error_message.contains(expected_message),
            "Error message should match expected format. Got: {}",
            error_message
        );

        // Check for the error code (if available in the error structure)
        // Note: The exact error code structure may vary depending on the error type
        assert!(
            error_message.contains("@source") && error_message.contains("http"),
            "Error message should mention @source and http"
        );
    }

    #[test]
    #[ignore]
    fn requires_the_http_arg_for_connect() {
        let subgraphs = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://127.0.0.1" })

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", selection: "")
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraphs]);

        // This should fail with error: [with-connectors] Directive "@connect" argument "http"
        // of type "connect__ConnectHTTP!" is required, but it was not provided.
        assert!(
            result.is_err(),
            "Composition should fail due to missing http argument in @connect directive"
        );

        let errors = result.unwrap_err().errors;
        // Check that we have exactly 1 error
        assert_eq!(errors.len(), 1, "Should have exactly 1 error");

        let error = &errors[0];
        let error_message = format!("{:?}", error);

        // Check for the specific error message
        let expected_message = "[with-connectors] Directive \"@connect\" argument \"http\" of type \"connect__ConnectHTTP!\" is required, but it was not provided.";
        assert!(
            error_message.contains(expected_message),
            "Error message should match expected format. Got: {}",
            error_message
        );

        // Check for the error code (if available in the error structure)
        // Note: The exact error code structure may vary depending on the error type
        assert!(
            error_message.contains("@connect") && error_message.contains("http"),
            "Error message should mention @connect and http"
        );
    }

    #[test]
    fn composes_v0_3() {
        let with_connectors_v0_3 = ServiceDefinition {
            name: "with-connectors-v0_3",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.3"
                    import: ["@connect", "@source"]
                )
                @source(
                  name: "v1"
                  http: {
                    baseURL: "http://v1"
                    path: ""
                    queryParams: ""
                  }
                  errors: { message: "" extensions: "" }
                  isSuccess: ""
                )

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource @key(fields: "id")
                  @connect(
                    id: "conn_id", 
                    source: "v1"
                    http: {
                      GET: "/resources"
                      path: ""
                      queryParams: ""
                    }
                    batch: { maxSize: 5 }
                    errors: { message: "" extensions: "" }
                    isSuccess: ""
                    selection: ""
                  ) {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let with_connectors_v0_1 = ServiceDefinition {
            name: "with-connectors-v0_1",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    widgets: [Widget!]!
                    @connect(source: "v1", http: { GET: "/widgets" }, selection: "")
                }

                type Widget @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors_v0_3, with_connectors_v0_1]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string);
    }

    #[test]
    fn composes_v0_4() {
        let with_connectors_v0_3 = ServiceDefinition {
            name: "with-connectors-v0_3",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.3"
                    import: ["@connect", "@source"]
                )
                @source(
                  name: "v1"
                  http: {
                    baseURL: "http://v1"
                    path: ""
                    queryParams: ""
                  }
                  errors: { message: "" extensions: "" }
                  isSuccess: ""
                )

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource @key(fields: "id")
                  @connect(
                    id: "conn_id",
                    source: "v1"
                    http: {
                      GET: "/resources"
                      path: ""
                      queryParams: ""
                    }
                    batch: { maxSize: 5 }
                    errors: { message: "" extensions: "" }
                    isSuccess: ""
                    selection: ""
                  ) {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let with_connectors_v0_4 = ServiceDefinition {
            name: "with-connectors-v0_4",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.4"
                    import: ["@connect", "@source"]
                )
                @source(name: "v4", http: { baseURL: "http://v4" })

                type Query {
                    widgets: [Widget!]!
                    @connect(source: "v4", http: { GET: "/widgets" }, selection: "")
                }

                type Widget @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors_v0_3, with_connectors_v0_4]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string);
    }

    /// An executable directive, with argument types (an enum, a custom scalar, and a
    /// self-referential input object) that nothing else references. Every subgraph that defines
    /// the directive has to define those types too.
    const OWNERSHIP_DIRECTIVE_DEFS: &str = r#"
        directive @ownership(
            owners: [Owner!]
            since: DateTime
            filter: OwnershipFilter
        ) on FIELD

        enum Owner {
            ALICE
            BOB
        }

        scalar DateTime

        input OwnershipFilter {
            owner: Owner
            parent: OwnershipFilter
        }
    "#;

    fn ownership_connector_subgraph(connect_version: &str) -> String {
        format!(
            r#"
            extend schema
            @link(
                url: "https://specs.apollo.dev/connect/{connect_version}"
                import: ["@connect", "@source"]
            )
            @source(name: "v1", http: {{ baseURL: "http://v1" }})

            {OWNERSHIP_DIRECTIVE_DEFS}

            type Query {{
                resources: [Resource!]!
                @connect(source: "v1", http: {{ GET: "/resources" }}, selection: "id name")
            }}

            type Resource {{
                id: ID!
                name: String!
            }}
            "#
        )
    }

    /// RH-1375: connector expansion replaces a connector subgraph with synthetic subgraphs, and
    /// subgraph extraction copies executable directive definitions into every subgraph. If the
    /// synthetic subgraphs don't carry the directive's argument types, the expanded supergraph
    /// is inconsistent and satisfiability fails with "Cannot wrap an undefined/null type" (JS) or
    /// a dangling type reference (Rust).
    #[test]
    fn executable_directive_arg_types_survive_connector_expansion() {
        for connect_version in ["v0.2", "v0.4"] {
            let connector_type_defs = ownership_connector_subgraph(connect_version);
            let plain_type_defs = format!(
                r#"
                {OWNERSHIP_DIRECTIVE_DEFS}

                type Query {{
                    greeting: String
                }}
                "#
            );

            let with_connectors = ServiceDefinition {
                name: "with-connectors",
                type_defs: &connector_type_defs,
            };
            let plain = ServiceDefinition {
                name: "plain",
                type_defs: &plain_type_defs,
            };

            let supergraph =
                compose_as_fed2_subgraphs(&[with_connectors, plain]).unwrap_or_else(|e| {
                    panic!("Expected composition to succeed with connect/{connect_version}: {e:?}")
                });

            let schema_string = supergraph.schema().schema().to_string();
            assert!(
                schema_string.contains("directive @ownership("),
                "supergraph dropped the executable directive: {schema_string}"
            );

            // Mirror what the supergraph binary does after composition: expand the connectors,
            // then extract subgraphs from the expanded supergraph for satisfiability.
            let ExpansionResult::Expanded { raw_sdl, .. } =
                expand_connectors(&schema_string, &ApiSchemaOptions::default())
                    .expect("Expected connector expansion to succeed")
            else {
                panic!("Expected connector expansion to expand subgraphs");
            };

            apollo_federation::Supergraph::new_with_router_specs(&raw_sdl)
                .and_then(|expanded| expanded.extract_subgraphs())
                .unwrap_or_else(|e| {
                    panic!("Expected subgraph extraction to succeed with connect/{connect_version}: {e}")
                });
        }
    }

    /// Expanding a supergraph made only of connector subgraphs must keep its executable
    /// directives, and the types their arguments reference, so the router can still plan
    /// operations that use them.
    #[test]
    fn executable_directive_survives_all_connector_expansion() {
        for connect_version in ["v0.2", "v0.4"] {
            let connector_type_defs = ownership_connector_subgraph(connect_version);
            let with_connectors = ServiceDefinition {
                name: "with-connectors",
                type_defs: &connector_type_defs,
            };

            let supergraph = compose_as_fed2_subgraphs(&[with_connectors]).unwrap_or_else(|e| {
                panic!("Expected composition to succeed with connect/{connect_version}: {e:?}")
            });

            let ExpansionResult::Expanded { raw_sdl, .. } = expand_connectors(
                &supergraph.schema().schema().to_string(),
                &ApiSchemaOptions::default(),
            )
            .expect("Expected connector expansion to succeed") else {
                panic!("Expected connector expansion to expand subgraphs");
            };

            for expected in [
                "directive @ownership(",
                "enum Owner",
                "scalar DateTime",
                "input OwnershipFilter",
            ] {
                assert!(
                    raw_sdl.contains(expected),
                    "expanded supergraph dropped `{expected}` with connect/{connect_version}: {raw_sdl}"
                );
            }

            apollo_federation::Supergraph::new_with_router_specs(&raw_sdl)
                .and_then(|expanded| expanded.extract_subgraphs())
                .unwrap_or_else(|e| {
                    panic!("Expected subgraph extraction to succeed with connect/{connect_version}: {e}")
                });
        }
    }
}
