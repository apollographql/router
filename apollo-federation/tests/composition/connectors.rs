use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;

#[cfg(test)]
mod tests {
    use apollo_federation::error::ErrorCode;

    use super::*;

    #[ignore = "until merge implementation completed"]
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
        assert!(result.is_ok(), "Composition should succeed, but got errors: {:?}", result.err());
        
        let supergraph = result.unwrap();
        let schema_string = supergraph.schema().schema().to_string();
        
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/link/v1.0")"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/connect/v0.2", for: EXECUTION)"#));
        
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "link""#));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "source""#));
        
        assert!(schema_string.contains("type Query"));
        assert!(schema_string.contains("resources: [Resource!]!"));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "connect""#));
        
        assert!(schema_string.contains("type Resource"));
        assert!(schema_string.contains(r#"@join__type(graph: WITH_CONNECTORS, key: "id")"#));
        assert!(schema_string.contains("id: ID!"));
        assert!(schema_string.contains("name: String!"));

        let api_schema_result = supergraph.to_api_schema(Default::default());
        assert!(api_schema_result.is_ok(), "API schema generation should succeed");
        
        let api_schema = api_schema_result.unwrap();
        let api_schema_string = api_schema.schema().to_string();
        
        assert!(api_schema_string.contains("type Query"));
        assert!(api_schema_string.contains("resources: [Resource!]!"));
        assert!(api_schema_string.contains("type Resource"));
        assert!(api_schema_string.contains("id: ID!"));
        assert!(api_schema_string.contains("name: String!"));
        
        assert!(!api_schema_string.contains("@join__"));
        assert!(!api_schema_string.contains("@link"));
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn does_not_require_importing_connect() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.10"
                    import: ["@key"]
                )
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
        assert!(result.is_ok(), "Composition should succeed, but got errors: {:?}", result.err());
        
        let supergraph = result.unwrap();
        let schema_string = supergraph.schema().schema().to_string();
        
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/link/v1.0")"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/connect/v0.2", for: EXECUTION)"#));
        
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "link""#));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "source""#));
        
        assert!(schema_string.contains("type Query"));
        assert!(schema_string.contains("resources: [Resource!]!"));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "connect""#));
        
        assert!(schema_string.contains("type Resource"));
        assert!(schema_string.contains(r#"@join__type(graph: WITH_CONNECTORS, key: "id")"#));
        assert!(schema_string.contains("id: ID!"));
        assert!(schema_string.contains("name: String!"));

        let api_schema_result = supergraph.to_api_schema(Default::default());
        assert!(api_schema_result.is_ok(), "API schema generation should succeed");
        
        let api_schema = api_schema_result.unwrap();
        let api_schema_string = api_schema.schema().to_string();
        
        assert!(api_schema_string.contains("type Query"));
        assert!(api_schema_string.contains("resources: [Resource!]!"));
        assert!(api_schema_string.contains("type Resource"));
        assert!(api_schema_string.contains("id: ID!"));
        assert!(api_schema_string.contains("name: String!"));
        
        assert!(!api_schema_string.contains("@join__"));
        assert!(!api_schema_string.contains("@link"));
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn using_as_alias() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.10"
                    import: ["@key"]
                )
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
        assert!(result.is_ok(), "Composition should succeed, but got errors: {:?}", result.err());
        
        let supergraph = result.unwrap();
        let schema_string = supergraph.schema().schema().to_string();
        
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/link/v1.0")"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/connect/v0.2", for: EXECUTION)"#));
        
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "link""#));
        assert!(schema_string.contains(r#"as: "http""#));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "source""#));
        
        assert!(schema_string.contains("type Query"));
        assert!(schema_string.contains("resources: [Resource!]!"));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "http""#));
        
        assert!(schema_string.contains("type Resource"));
        assert!(schema_string.contains(r#"@join__type(graph: WITH_CONNECTORS, key: "id")"#));
        assert!(schema_string.contains("id: ID!"));
        assert!(schema_string.contains("name: String!"));

        let api_schema_result = supergraph.to_api_schema(Default::default());
        assert!(api_schema_result.is_ok(), "API schema generation should succeed");
        
        let api_schema = api_schema_result.unwrap();
        let api_schema_string = api_schema.schema().to_string();
        
        assert!(api_schema_string.contains("type Query"));
        assert!(api_schema_string.contains("resources: [Resource!]!"));
        assert!(api_schema_string.contains("type Resource"));
        assert!(api_schema_string.contains("id: ID!"));
        assert!(api_schema_string.contains("name: String!"));
        
        assert!(!api_schema_string.contains("@join__"));
        assert!(!api_schema_string.contains("@link"));
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn composes_v0_2() {
        let with_connectors_v0_2 = ServiceDefinition {
            name: "with-connectors-v0_2",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.11"
                    import: ["@key"]
                )
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
                    url: "https://specs.apollo.dev/federation/v2.10"
                    import: ["@key"]
                )
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
        assert!(result.is_ok(), "Composition should succeed, but got errors: {:?}", result.err());
        
        let supergraph = result.unwrap();
        let schema_string = supergraph.schema().schema().to_string();
        
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/link/v1.0")"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/connect/v0.2", for: EXECUTION)"#));
        
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS_V0_1_], name: "link""#));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS_V0_2_], name: "link""#));
        assert!(schema_string.contains(r#"url: "https://specs.apollo.dev/connect/v0.1""#));
        assert!(schema_string.contains(r#"url: "https://specs.apollo.dev/connect/v0.2""#));
        
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS_V0_1_], name: "source""#));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS_V0_2_], name: "source""#));
        assert!(schema_string.contains(r#"http: {baseURL: "http://v1"}"#));
        assert!(schema_string.contains(r#"path: """#));
        assert!(schema_string.contains(r#"queryParams: """#));
        assert!(schema_string.contains(r#"errors: {message: "", extensions: ""}"#));
        
        assert!(schema_string.contains("WITH_CONNECTORS_V0_1_"));
        assert!(schema_string.contains("WITH_CONNECTORS_V0_2_"));
        
        assert!(schema_string.contains("type Query"));
        assert!(schema_string.contains("widgets: [Widget!]!"));
        assert!(schema_string.contains("resources: [Resource!]!"));
        
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS_V0_1_], name: "connect", args: {source: "v1", http: {GET: "/widgets"}"#));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS_V0_2_], name: "connect", args: {source: "v1", http: {GET: "/resources"}"#));
        
        assert!(schema_string.contains("type Query"));
        assert!(schema_string.contains("type Widget"));
        assert!(schema_string.contains("type Resource"));
        
        assert!(schema_string.contains(r#"batch: {maxSize: 5}"#));
        
        let api_schema_result = supergraph.to_api_schema(Default::default());
        assert!(api_schema_result.is_ok(), "API schema generation should succeed");
        
        let api_schema = api_schema_result.unwrap();
        let api_schema_string = api_schema.schema().to_string();
        
        // Verify API schema contains both types and fields
        assert!(api_schema_string.contains("type Query"));
        assert!(api_schema_string.contains("widgets: [Widget!]!"));
        assert!(api_schema_string.contains("resources: [Resource!]!"));
        assert!(api_schema_string.contains("type Widget"));
        assert!(api_schema_string.contains("type Resource"));
        assert!(api_schema_string.contains("id: ID!"));
        assert!(api_schema_string.contains("name: String!"));
        
        assert!(!api_schema_string.contains("@join__"));
        assert!(!api_schema_string.contains("@link"));
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn composes_with_renames() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.10"
                    import: ["@key"]
                )
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
        assert!(result.is_ok(), "Composition should succeed, but got errors: {:?}", result.err());
        
        let supergraph = result.unwrap();
        let schema_string = supergraph.schema().schema().to_string();
        
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/link/v1.0")"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)"#));
        assert!(schema_string.contains(r#"@link(url: "https://specs.apollo.dev/connect/v0.2", for: EXECUTION)"#));
        
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "link""#));
        assert!(schema_string.contains(r#"as: "http""#));
        assert!(schema_string.contains(r#"name: "@connect", as: "@http""#));
        assert!(schema_string.contains(r#"name: "@source", as: "@api""#));
        
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "api""#));
        
        assert!(schema_string.contains("type Query"));
        assert!(schema_string.contains("resources: [Resource!]!"));
        assert!(schema_string.contains(r#"@join__directive(graphs: [WITH_CONNECTORS], name: "http""#));
        
        assert!(schema_string.contains("type Resource"));
        assert!(schema_string.contains(r#"@join__type(graph: WITH_CONNECTORS, key: "id")"#));
        assert!(schema_string.contains("id: ID!"));
        assert!(schema_string.contains("name: String!"));

        let api_schema_result = supergraph.to_api_schema(Default::default());
        assert!(api_schema_result.is_ok(), "API schema generation should succeed");
        
        let api_schema = api_schema_result.unwrap();
        let api_schema_string = api_schema.schema().to_string();
        
        assert!(api_schema_string.contains("type Query"));
        assert!(api_schema_string.contains("resources: [Resource!]!"));
        assert!(api_schema_string.contains("type Resource"));
        assert!(api_schema_string.contains("id: ID!"));
        assert!(api_schema_string.contains("name: String!"));
        
        assert!(!api_schema_string.contains("@join__"));
        assert!(!api_schema_string.contains("@link"));
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn requires_http_arg_for_source() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.10"
                    import: ["@key"]
                )
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1")

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "")
                }

                type Resource {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors]);
        assert!(result.is_err(), "Composition should fail due to missing http arg");
        
        let errors = result.err().unwrap();
        assert_eq!(errors.len(), 1, "Should have exactly one error");
        
        let error = &errors[0];
        assert!(error.to_string().contains(r#"Directive "@source" argument "http""#));
        assert!(error.to_string().contains("is required, but it was not provided"));
        assert!(error.to_string().contains("[with-connectors]"));
        assert_eq!(error.code(), ErrorCode::InvalidGraphQL);
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn requires_http_arg_for_connect() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/federation/v2.10"
                    import: ["@key"]
                )
                @link(
                    url: "https://specs.apollo.dev/connect/v0.1"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: {baseURL: "http://127.0.0.1"})

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", selection: "")
                }

                type Resource {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[with_connectors]);
        assert!(result.is_err(), "Composition should fail due to missing http arg");
        
        let errors = result.err().unwrap();
        assert_eq!(errors.len(), 1, "Should have exactly one error");
        
        let error = &errors[0];
        assert!(error.to_string().contains(r#"Directive "@connect" argument "http""#));
        assert!(error.to_string().contains("is required, but it was not provided"));
        assert!(error.to_string().contains("[with-connectors]"));
        assert_eq!(error.code(), ErrorCode::InvalidGraphQL);
    }
    
}
