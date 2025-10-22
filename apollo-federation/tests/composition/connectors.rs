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

        assert_snapshot!(api_schema_string, @r###"
        type Query {
          resources: [Resource!]!
          widgets: [Widget!]!
        }

        type Resource {
          id: ID!
          name: String!
        }

        type Widget {
          id: ID!
          name: String!
        }
        "###);
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

        let errors = result.unwrap_err();
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

        let errors = result.unwrap_err();
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
}
