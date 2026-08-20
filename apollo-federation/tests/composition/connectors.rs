use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::supergraph::HintLevel;
use insta::assert_snapshot;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose;
use super::compose_as_fed2_connectors_subgraphs;

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
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_connectors_subgraphs(&[with_connectors]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @"
        type Query {
          resources: [Resource!]!
          resource(id: ID!): Resource
        }

        type Resource {
          id: ID!
          name: String!
        }
        ");
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
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_connectors_subgraphs(&[with_connectors]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @"
        type Query {
          resources: [Resource!]!
          resource(id: ID!): Resource
        }

        type Resource {
          id: ID!
          name: String!
        }
        ");
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
                    @http(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @http(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_connectors_subgraphs(&[with_connectors]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @"
        type Query {
          resources: [Resource!]!
          resource(id: ID!): Resource
        }

        type Resource {
          id: ID!
          name: String!
        }
        ");
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
                    path: "$(['v1'])"
                    queryParams: "$({ locale: 'en' })"
                  }
                  errors: { message: "error.message" extensions: "code: error.code" }
                )

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id")
                  @connect(
                    source: "v1"
                    http: {
                      GET: "/resources"
                      path: "$(['v1'])"
                      queryParams: "$({ locale: 'en' })"
                    }
                    batch: { maxSize: 5 }
                    errors: { message: "error.message" extensions: "code: error.code" }
                    selection: "id name"
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
                    @connect(source: "v1", http: { GET: "/widgets" }, selection: "id name")

                    widget(id: ID!): Widget
                    @connect(source: "v1", http: { GET: "/widgets/{$args.id}" }, selection: "id name", entity: true)
                }

                type Widget @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result =
            compose_as_fed2_connectors_subgraphs(&[with_connectors_v0_2, with_connectors_v0_1]);
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
          widget(id: ID!): Widget
          resources: [Resource!]!
          resource(id: ID!): Resource
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
                    @http(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @http(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_connectors_subgraphs(&[with_connectors]);
        let supergraph = result.expect("Expected composition to succeed");
        let schema_string = supergraph.schema().schema().to_string();

        assert_snapshot!(schema_string);

        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("Expected API schema generation to succeed");
        let api_schema_string = api_schema.schema().to_string();

        assert_snapshot!(api_schema_string, @"
        type Query {
          resources: [Resource!]!
          resource(id: ID!): Resource
        }

        type Resource {
          id: ID!
          name: String!
        }
        ");
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
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_connectors_subgraphs(&[subgraphs]);

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
                    @connect(source: "v1", selection: "id name")
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result = compose_as_fed2_connectors_subgraphs(&[subgraphs]);

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
                    path: "$(['v1'])"
                    queryParams: "$({ locale: 'en' })"
                  }
                  errors: { message: "error.message" extensions: "code: error.code" }
                  isSuccess: "$status->eq(200)"
                )

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id")
                  @connect(
                    id: "conn_id", 
                    source: "v1"
                    http: {
                      GET: "/resources"
                      path: "$(['v1'])"
                      queryParams: "$({ locale: 'en' })"
                    }
                    batch: { maxSize: 5 }
                    errors: { message: "error.message" extensions: "code: error.code" }
                    isSuccess: "$status->eq(200)"
                    selection: "id name"
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
                    @connect(source: "v1", http: { GET: "/widgets" }, selection: "id name")

                    widget(id: ID!): Widget
                    @connect(source: "v1", http: { GET: "/widgets/{$args.id}" }, selection: "id name", entity: true)
                }

                type Widget @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result =
            compose_as_fed2_connectors_subgraphs(&[with_connectors_v0_3, with_connectors_v0_1]);
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
                    path: "$(['v1'])"
                    queryParams: "$({ locale: 'en' })"
                  }
                  errors: { message: "error.message" extensions: "code: error.code" }
                  isSuccess: "$status->eq(200)"
                )

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id")
                  @connect(
                    id: "conn_id",
                    source: "v1"
                    http: {
                      GET: "/resources"
                      path: "$(['v1'])"
                      queryParams: "$({ locale: 'en' })"
                    }
                    batch: { maxSize: 5 }
                    errors: { message: "error.message" extensions: "code: error.code" }
                    isSuccess: "$status->eq(200)"
                    selection: "id name"
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
                    @connect(source: "v4", http: { GET: "/widgets" }, selection: "id name")

                    widget(id: ID!): Widget
                    @connect(source: "v4", http: { GET: "/widgets/{$args.id}" }, selection: "id name", entity: true)
                }

                type Widget @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let result =
            compose_as_fed2_connectors_subgraphs(&[with_connectors_v0_3, with_connectors_v0_4]);
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
    fn connectors_validation_errors_fail_composition() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.2"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

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

        let result = compose_as_fed2_connectors_subgraphs(&[with_connectors]);

        assert_composition_errors(
            &result,
            &[(
                "INVALID_SELECTION",
                "`@connect(selection:)` on `Query.resources` is empty",
            )],
        );
    }

    /// Connectors validations run against the schema document the user wrote, so the locations
    /// they report have to point back into it.
    #[test]
    fn connectors_validation_errors_keep_subgraph_locations() {
        let type_defs = r#"
extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.11", import: ["@key"])
  @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@connect", "@source"])
  @source(name: "v1", http: { baseURL: "http://v1" })

type Query {
  resources: [Resource!]! @connect(source: "v1", http: { GET: "/resources" }, selection: "")
}

type Resource {
  id: ID!
  name: String!
}
        "#;
        let subgraph =
            Subgraph::parse("with-connectors", "http://with-connectors", type_defs).unwrap();

        let failure = compose(vec![subgraph]).expect_err("Expected composition to fail");
        let locations = failure.errors[0].locations();

        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].subgraph, "with-connectors");
        // The empty `selection` argument is on line 8 of the subgraph document.
        assert_eq!(locations[0].range.start.line, 8);
    }

    /// Connectors warnings are reported as composition hints rather than failing the composition.
    #[test]
    fn connectors_validation_warnings_are_reported_as_hints() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.2"
                    import: ["@connect"]
                )

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")
                }

                type Resource {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let failure = compose_as_fed2_connectors_subgraphs(&[with_connectors])
            .expect_err("Expected composition to fail");

        assert!(
            failure.hints.iter().any(|hint| {
                hint.code() == "NO_SOURCE_IMPORT" && matches!(hint.level(), HintLevel::Warn)
            }),
            "Expected a NO_SOURCE_IMPORT hint, got: {:?}",
            failure
                .hints
                .iter()
                .map(|hint| hint.code())
                .collect::<Vec<_>>()
        );
    }

    /// `@override(from:)` pointing at a connector-enabled subgraph is rejected.
    #[test]
    fn override_on_connector_subgraph_is_rejected() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.2"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let plain = ServiceDefinition {
            name: "plain",
            type_defs: r#"
                type Query {
                    other: String
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String! @override(from: "with-connectors")
                }
            "#,
        };

        let result = compose_as_fed2_connectors_subgraphs(&[with_connectors, plain]);

        assert_composition_errors(
            &result,
            &[(
                "OVERRIDE_ON_CONNECTOR",
                r#"Field "Resource.name" on subgraph "plain" is trying to override connector-enabled subgraph "with-connectors", which is not yet supported. See https://go.apollo.dev/connectors/limitations#override-is-partially-unsupported"#,
            )],
        );
    }

    /// The check resolves `@override` through the federation spec, so a subgraph that imports it
    /// under an alias is still caught. The pre-Rust implementation matched the directive name
    /// against a hardcoded list and missed this case entirely.
    ///
    /// Built with `compose` directly rather than the fed2 helper, since both subgraphs need to
    /// bring their own federation `@link`.
    #[test]
    fn override_on_connector_subgraph_is_rejected_when_aliased() {
        let with_connectors = Subgraph::parse(
            "with-connectors",
            "http://with-connectors",
            r#"
extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.11", import: ["@key"])
  @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@connect", "@source"])
  @source(name: "v1", http: { baseURL: "http://v1" })

type Query {
  resources: [Resource!]! @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")
  resource(id: ID!): Resource
    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
}

type Resource @key(fields: "id") {
  id: ID!
  name: String!
}
            "#,
        )
        .unwrap();

        // `@override` is imported under an alias here.
        let plain = Subgraph::parse(
            "plain",
            "http://plain",
            r#"
extend schema
  @link(
    url: "https://specs.apollo.dev/federation/v2.11"
    import: ["@key", { name: "@override", as: "@replaces" }]
  )

type Query {
  other: String
}

type Resource @key(fields: "id") {
  id: ID!
  name: String! @replaces(from: "with-connectors")
}
            "#,
        )
        .unwrap();

        let failure =
            compose(vec![with_connectors, plain]).expect_err("Expected composition to fail");

        let codes: Vec<_> = failure
            .errors
            .iter()
            .map(|e| e.code().definition().code())
            .collect();
        assert!(
            codes.contains(&"OVERRIDE_ON_CONNECTOR"),
            "Expected an OVERRIDE_ON_CONNECTOR error, got: {codes:?}"
        );
    }

    /// `@override` against a subgraph without connectors is unaffected.
    #[test]
    fn override_on_non_connector_subgraph_is_allowed() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.2"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id", entity: true)
                }

                type Resource @key(fields: "id") {
                    id: ID!
                }
            "#,
        };

        let owner = ServiceDefinition {
            name: "owner",
            type_defs: r#"
                type Query {
                    widgets: [Widget!]!
                }

                type Widget @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let overrider = ServiceDefinition {
            name: "overrider",
            type_defs: r#"
                type Query {
                    other: String
                }

                type Widget @key(fields: "id") {
                    id: ID!
                    name: String! @override(from: "owner")
                }
            "#,
        };

        compose_as_fed2_connectors_subgraphs(&[with_connectors, owner, overrider])
            .expect("Expected composition to succeed");
    }

    /// Override errors are only reported once merging has succeeded, so that a merge failure isn't
    /// buried under connectors noise.
    #[test]
    fn override_on_connector_is_not_reported_when_merge_fails() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.2"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        // `Resource.name` is an Int here but a String above, which fails the merge.
        let plain = ServiceDefinition {
            name: "plain",
            type_defs: r#"
                type Query {
                    other: String
                }

                type Resource @key(fields: "id") {
                    id: ID!
                    name: Int! @override(from: "with-connectors")
                }
            "#,
        };

        let failure = compose_as_fed2_connectors_subgraphs(&[with_connectors, plain])
            .expect_err("Expected composition to fail");

        let codes: Vec<_> = failure
            .errors
            .iter()
            .map(|e| e.code().definition().code())
            .collect();
        assert!(
            !codes.contains(&"OVERRIDE_ON_CONNECTOR"),
            "Merge errors should be reported alone, got: {codes:?}"
        );
        assert!(!codes.is_empty(), "Expected merge errors");
    }

    /// Merge hints have to survive the connector-expansion detour: satisfiability runs against a
    /// supergraph parsed from expanded SDL, which carries none of them.
    #[test]
    fn merge_hints_survive_connector_expansion() {
        let with_connectors = ServiceDefinition {
            name: "with-connectors",
            type_defs: r#"
                extend schema
                @link(
                    url: "https://specs.apollo.dev/connect/v0.2"
                    import: ["@connect", "@source"]
                )
                @source(name: "v1", http: { baseURL: "http://v1" })

                type Query {
                    resources: [Resource!]!
                    @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")

                    resource(id: ID!): Resource
                    @connect(source: "v1", http: { GET: "/resources/{$args.id}" }, selection: "id name", entity: true)
                }

                "A description only this subgraph has"
                type Resource @key(fields: "id") {
                    id: ID!
                    name: String!
                }
            "#,
        };

        let plain = ServiceDefinition {
            name: "plain",
            type_defs: r#"
                type Query {
                    other: String
                }

                "A different description"
                type Resource @key(fields: "id") {
                    id: ID!
                }
            "#,
        };

        let supergraph = compose_as_fed2_connectors_subgraphs(&[with_connectors, plain])
            .expect("Expected composition to succeed");

        let codes: Vec<_> = supergraph.hints().iter().map(|h| h.code()).collect();
        assert!(
            codes.contains(&"INCONSISTENT_DESCRIPTION"),
            "Expected the merge hint to survive expansion, got: {codes:?}"
        );
    }
}
