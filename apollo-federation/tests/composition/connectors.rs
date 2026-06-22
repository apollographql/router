use insta::assert_snapshot;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;
use super::test_helpers::as_fed2_subgraphs;

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

    /// Repro for RH-1375: composing (via the connectors expansion path) two
    /// subgraphs that both define an executable directive whose argument types
    /// (an enum, a custom scalar, and a — self-referential — input object) are
    /// referenced nowhere else. Connector expansion replaces the connector
    /// subgraph with synthetic subgraphs; those types must be declared as
    /// members of the synthetic subgraphs (via `@join__type` / `@join__enumValue`
    /// / `@join__field`) so the expanded supergraph stays valid through
    /// subgraph extraction during satisfiability. Pre-fix, the expanded
    /// supergraph declared the types absent from the synthetic subgraphs while
    /// extraction copied the directive (which references them) into every
    /// subgraph, producing an invalid subgraph and a composition error.
    #[test]
    fn executable_directive_enum_arg_survives_connector_expansion() {
        use apollo_federation::composition::CompositionOptions;
        use apollo_federation::composition::compose_with_connectors;

        // Shared definitions: an executable directive whose arguments cover all
        // three input-type kinds, plus those types. `OwnershipFilter` is
        // self-referential to exercise the recursive input-object path.
        let shared_defs = r#"
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

        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: format!(
                r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: {{ baseURL: "http://v1" }})

                {shared_defs}

                type Query {{
                    resources: [Resource!]!
                    @connect(source: "v1", http: {{ GET: "/resources" }}, selection: "id name")
                }}

                type Resource @key(fields: "id") {{
                    id: ID!
                    name: String!
                }}
            "#
            )
            .leak(),
        };

        let plain = ServiceDefinition {
            name: "plain",
            type_defs: format!(
                r#"
                {shared_defs}

                type Query {{
                    greeting: String
                }}
            "#
            )
            .leak(),
        };

        let subgraphs = as_fed2_subgraphs(&[with_connectors, plain])
            .expect("Expected fed2 subgraph conversion to succeed");

        let result = compose_with_connectors(subgraphs, CompositionOptions::default());

        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();
        for expected in ["enum Owner", "scalar DateTime", "input OwnershipFilter"] {
            assert!(
                schema_string.contains(expected),
                "expanded supergraph dropped `{expected}`: {schema_string}"
            );
        }
    }
}
