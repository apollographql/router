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

        assert_snapshot!(schema_string, @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION) @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@connect", "@source"]) @join__directive(name: "link", graphs: [WITH_CONNECTORS], args: {url: "https://specs.apollo.dev/connect/v0.1", import: ["@connect", "@source"]}) @join__directive(name: "source", graphs: [WITH_CONNECTORS], args: {name: "v1", http: {baseURL: "http://v1"}}) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

        directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

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

        enum join__Graph {
          WITH_CONNECTORS @join__graph(name: "with-connectors", url: "http://with-connectors")
        }

        scalar join__FieldSet

        scalar join__DirectiveArguments

        scalar join__FieldValue

        input join__ContextArgument {
          name: String!
          type: String!
          context: String!
          selection: join__FieldValue
        }

        type Query @join__type(graph: WITH_CONNECTORS) {
          resources: [Resource!]! @join__directive(name: "connect", graphs: [WITH_CONNECTORS], args: {source: "v1", http: {GET: "/resources"}, selection: ""})
        }

        type Resource @join__type(graph: WITH_CONNECTORS, key: "id") {
          id: ID!
          name: String!
        }
        "#);

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

        assert_snapshot!(schema_string, @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION) @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@source"]) @join__directive(name: "link", graphs: [WITH_CONNECTORS], args: {url: "https://specs.apollo.dev/connect/v0.1", import: ["@source"]}) @join__directive(name: "source", graphs: [WITH_CONNECTORS], args: {name: "v1", http: {baseURL: "http://v1"}}) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

        directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

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

        enum join__Graph {
          WITH_CONNECTORS @join__graph(name: "with-connectors", url: "http://with-connectors")
        }

        scalar join__FieldSet

        scalar join__DirectiveArguments

        scalar join__FieldValue

        input join__ContextArgument {
          name: String!
          type: String!
          context: String!
          selection: join__FieldValue
        }

        type Query @join__type(graph: WITH_CONNECTORS) {
          resources: [Resource!]! @join__directive(name: "connect", graphs: [WITH_CONNECTORS], args: {source: "v1", http: {GET: "/resources"}, selection: ""})
        }

        type Resource @join__type(graph: WITH_CONNECTORS, key: "id") {
          id: ID!
          name: String!
        }
        "#);

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

        assert_snapshot!(schema_string, @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION) @link(url: "https://specs.apollo.dev/connect/v0.1", as: "http", import: ["@source"]) @join__directive(name: "link", graphs: [WITH_CONNECTORS], args: {url: "https://specs.apollo.dev/connect/v0.1", as: "http", import: ["@source"]}) @join__directive(name: "source", graphs: [WITH_CONNECTORS], args: {name: "v1", http: {baseURL: "http://v1"}}) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

        directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

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

        enum join__Graph {
          WITH_CONNECTORS @join__graph(name: "with-connectors", url: "http://with-connectors")
        }

        scalar join__FieldSet

        scalar join__DirectiveArguments

        scalar join__FieldValue

        input join__ContextArgument {
          name: String!
          type: String!
          context: String!
          selection: join__FieldValue
        }

        type Query @join__type(graph: WITH_CONNECTORS) {
          resources: [Resource!]! @join__directive(name: "http", graphs: [WITH_CONNECTORS], args: {source: "v1", http: {GET: "/resources"}, selection: ""})
        }

        type Resource @join__type(graph: WITH_CONNECTORS, key: "id") {
          id: ID!
          name: String!
        }
        "#);

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

        assert_snapshot!(schema_string, @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION) @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@connect", "@source"]) @join__directive(name: "link", graphs: [WITH_CONNECTORS_V0_2_], args: {url: "https://specs.apollo.dev/connect/v0.2", import: ["@connect", "@source"]}) @join__directive(name: "link", graphs: [WITH_CONNECTORS_V0_1_], args: {url: "https://specs.apollo.dev/connect/v0.1", import: ["@connect", "@source"]}) @join__directive(name: "source", graphs: [WITH_CONNECTORS_V0_2_], args: {name: "v1", http: {baseURL: "http://v1", path: "", queryParams: ""}, errors: {message: "", extensions: ""}}) @join__directive(name: "source", graphs: [WITH_CONNECTORS_V0_1_], args: {name: "v1", http: {baseURL: "http://v1"}}) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

        directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

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

        enum join__Graph {
          WITH_CONNECTORS_V0_2_ @join__graph(name: "with-connectors-v0_2", url: "http://with-connectors-v0_2")
          WITH_CONNECTORS_V0_1_ @join__graph(name: "with-connectors-v0_1", url: "http://with-connectors-v0_1")
        }

        scalar join__FieldSet

        scalar join__DirectiveArguments

        scalar join__FieldValue

        input join__ContextArgument {
          name: String!
          type: String!
          context: String!
          selection: join__FieldValue
        }

        type Query @join__type(graph: WITH_CONNECTORS_V0_2_) @join__type(graph: WITH_CONNECTORS_V0_1_) {
          resources: [Resource!]! @join__field(graph: WITH_CONNECTORS_V0_2_) @join__directive(name: "connect", graphs: [WITH_CONNECTORS_V0_2_], args: {source: "v1", http: {GET: "/resources"}, selection: ""})
          widgets: [Widget!]! @join__field(graph: WITH_CONNECTORS_V0_1_) @join__directive(name: "connect", graphs: [WITH_CONNECTORS_V0_1_], args: {source: "v1", http: {GET: "/widgets"}, selection: ""})
        }

        type Resource @join__type(graph: WITH_CONNECTORS_V0_2_, key: "id") @join__directive(name: "connect", graphs: [WITH_CONNECTORS_V0_2_], args: {source: "v1", http: {GET: "/resources", path: "", queryParams: ""}, batch: {maxSize: 5}, errors: {message: "", extensions: ""}, selection: ""}) {
          id: ID!
          name: String!
        }

        type Widget @join__type(graph: WITH_CONNECTORS_V0_1_, key: "id") {
          id: ID!
          name: String!
        }
        "#);

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

        assert_snapshot!(schema_string, @r#"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION) @link(url: "https://specs.apollo.dev/connect/v0.1", as: "http", import: [{name: "@connect", as: "@http"}, {name: "@source", as: "@api"}]) @join__directive(name: "link", graphs: [WITH_CONNECTORS], args: {url: "https://specs.apollo.dev/connect/v0.1", as: "http", import: [{name: "@connect", as: "@http"}, {name: "@source", as: "@api"}]}) @join__directive(name: "api", graphs: [WITH_CONNECTORS], args: {name: "v1", http: {baseURL: "http://v1"}}) {
          query: Query
        }

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

        directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

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

        enum join__Graph {
          WITH_CONNECTORS @join__graph(name: "with-connectors", url: "http://with-connectors")
        }

        scalar join__FieldSet

        scalar join__DirectiveArguments

        scalar join__FieldValue

        input join__ContextArgument {
          name: String!
          type: String!
          context: String!
          selection: join__FieldValue
        }

        type Query @join__type(graph: WITH_CONNECTORS) {
          resources: [Resource!]! @join__directive(name: "http", graphs: [WITH_CONNECTORS], args: {source: "v1", http: {GET: "/resources"}, selection: ""})
        }

        type Resource @join__type(graph: WITH_CONNECTORS, key: "id") {
          id: ID!
          name: String!
        }
        "#);

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
}
