use apollo_compiler::NodeStr;

#[cfg_attr(test, derive(Debug))]
pub(crate) struct SourceAPI {
    pub(crate) graph: NodeStr,
    pub(crate) name: NodeStr,
    pub(crate) http: HTTPSourceAPI,
}

#[cfg_attr(test, derive(Debug))]
pub(crate) struct HTTPSourceAPI {
    pub(crate) base_url: NodeStr,
    pub(crate) default: bool,
    pub(crate) headers: Vec<HTTPHeaderMapping>,
}

#[cfg_attr(test, derive(Debug))]
pub(crate) struct HTTPHeaderMapping {
    pub(crate) name: NodeStr,
    pub(crate) r#as: Option<NodeStr>,
    pub(crate) value: Option<NodeStr>,
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

    use crate::{
        schema::ValidFederationSchema,
        sources::connect::spec::definition::{
            CONNECT_DIRECTIVE_NAME_IN_SPEC, SOURCE_DIRECTIVE_NAME_IN_SPEC,
        },
    };

    const SUBGRAPH_SCHEMA: &str = r#"
        extend schema
         @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@connect", "@source"])
         @source(
           name: "json"
           http: { baseURL: "https://jsonplaceholder.typicode.com/" }
         )

        type Query {
          users: [User]
           @connect(
             source: "json"
             http: { GET: "/users" }
             selection: "id name"
           )

          posts: [Post]
           @connect(
             source: "json"
             http: { GET: "/posts" }
             selection: "id title body"
           )
        }

        type User {
          id: ID!
          name: String
        }

        type Post {
          id: ID!
          title: String
          body: String
        }
    "#;

    #[test]
    fn it_parses_at_source() {
        let schema_str = format!(
            "{}\n{}\n{}",
            SUBGRAPH_SCHEMA, TEMP_FEDERATION_DEFINITIONS, TEMP_SOURCE_DEFINITIONS
        );
        let schema = Schema::parse(schema_str, "schema.graphql").unwrap();

        let schema = ValidFederationSchema::new(schema.validate().unwrap()).unwrap();

        let actual_definition = schema
            .get_directive_definition(&SOURCE_DIRECTIVE_NAME_IN_SPEC)
            .unwrap()
            .get(&schema.schema())
            .unwrap();

        insta::assert_snapshot!(
            actual_definition.to_string(),
            @r###"
                """
                Defines connector configuration for reuse across multiple connectors.

                Exactly one of {http} must be present.
                """
                directive @source(name: String!, http: SourceHTTP) on SCHEMA
            "###
        );

        insta::assert_debug_snapshot!(
            schema
                .referencers()
                .get_directive(SOURCE_DIRECTIVE_NAME_IN_SPEC.as_str())
                .unwrap(),
            @r###"
                DirectiveReferencers {
                    schema: Some(
                        SchemaDefinitionPosition,
                    ),
                    scalar_types: {},
                    object_types: {},
                    object_fields: {},
                    object_field_arguments: {},
                    interface_types: {},
                    interface_fields: {},
                    interface_field_arguments: {},
                    union_types: {},
                    enum_types: {},
                    enum_values: {},
                    input_object_types: {},
                    input_object_fields: {},
                    directive_arguments: {},
                }
            "###
        );
    }

    #[test]
    fn it_parses_at_connect() {
        let schema_str = format!(
            "{}\n{}\n{}",
            SUBGRAPH_SCHEMA, TEMP_FEDERATION_DEFINITIONS, TEMP_SOURCE_DEFINITIONS
        );
        let schema = Schema::parse(schema_str, "schema.graphql").unwrap();

        let schema = ValidFederationSchema::new(schema.validate().unwrap()).unwrap();

        let actual_definition = schema
            .get_directive_definition(&CONNECT_DIRECTIVE_NAME_IN_SPEC)
            .unwrap()
            .get(&schema.schema())
            .unwrap();

        insta::assert_snapshot!(
            actual_definition.to_string(),
            @r###"
              """
              Defines a connector as the implementation of a field.

              Exactly one of {http} must be present.
              """
              directive @connect(
                """
                Optionally connects a @source directive for shared connector configuration.
                Must match the `name:` argument of a @source directive in this schema.
                """
                source: String,
                """Defines HTTP configuration for this connector."""
                http: ConnectHTTP,
                """
                Uses the JSONSelection syntax to define a mapping of connector response
                to GraphQL schema.
                """
                selection: JSONSelection,
                """
                Marks this connector as a canonical resolver for an entity (uniquely
                identified domain model.) If true, the connector must be defined on a
                field of the Query type.
                """
                entity: Boolean = false,
              ) on FIELD_DEFINITION
        "###
        );

        let fields = schema
            .referencers()
            .get_directive(CONNECT_DIRECTIVE_NAME_IN_SPEC.as_str())
            .unwrap()
            .object_fields
            .iter()
            .map(|f| f.get(&schema.schema()).unwrap().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        insta::assert_snapshot!(
            fields,
            @r###"
                users: [User] @connect(source: "json", http: {GET: "/users"}, selection: "id name")
                posts: [Post] @connect(source: "json", http: {GET: "/posts"}, selection: "id title body")
            "###
        );
    }

    static TEMP_FEDERATION_DEFINITIONS: &str = r#"
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        scalar link__Import

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
    "#;

    static TEMP_SOURCE_DEFINITIONS: &str = r#"
        """
        Defines a connector as the implementation of a field.

        Exactly one of {http} must be present.
        """
        directive @connect(
          """
          Optionally connects a @source directive for shared connector configuration.
          Must match the `name:` argument of a @source directive in this schema.
          """
          source: String

          """
          Defines HTTP configuration for this connector.
          """
          http: ConnectHTTP

          """
          Uses the JSONSelection syntax to define a mapping of connector response
          to GraphQL schema.
          """
          selection: JSONSelection

          """
          Marks this connector as a canonical resolver for an entity (uniquely
          identified domain model.) If true, the connector must be defined on a
          field of the Query type.
          """
          entity: Boolean = false
        ) on FIELD_DEFINITION

        """
        HTTP configuration for a connector.

        Exactly one of {GET,POST,PATCH,PUT,DELETE} must be present.
        """
        input ConnectHTTP {
          """
          URL template for GET requests to an HTTP endpoint.

          Can be a full URL or a partial path. If it's a partial path, it will
          be appended to an associated `baseURL` from the related @source.
          """
          GET: URLPathTemplate

          "Same as GET but for POST requests"
          POST: URLPathTemplate

          "Same as GET but for PATCH requests"
          PATCH: URLPathTemplate

          "Same as GET but for PUT requests"
          PUT: URLPathTemplate

          "Same as GET but for DELETE requests"
          DELETE: URLPathTemplate

          """
          Define a request body using JSONSelection. Selections can include
          values from field arguments using `$args.argName` and from fields on the
          parent type using `$this.fieldName`.
          """
          body: JSONSelection

          """
          Configuration for headers to attach to the request.

          Takes precedence over headers defined on the associated @source.
          """
          headers: [HTTPHeaderMapping!]
        }

        """
        At most one of {as,value} can be present.
        """
        input HTTPHeaderMapping {
          "The name of the incoming HTTP header to propagate to the endpoint"
          name: String!

          "If present, this defines the name of the header in the endpoint request"
          as: String

          "If present, this defines values for the headers in the endpoint request"
          value: [String]
        }

        """
        Defines connector configuration for reuse across multiple connectors.

        Exactly one of {http} must be present.
        """
        directive @source(
          name: String!

          http: SourceHTTP
        ) on SCHEMA

        """
        Common HTTP configuration for connectors.
        """
        input SourceHTTP {
          """
          If the URL path template in a connector is not a valid URL, it will be appended
          to this URL. Must be a valid URL.
          """
          baseURL: String!

          """
          Common headers from related connectors.
          """
          headers: [HTTPHeaderMapping!]
        }

        """
        A string containing a "JSON Selection", which defines a mapping from one JSON-like
        shape to another JSON-like shape.

        Example: ".data { id: user_id name account: { id: account_id } }"
        """
        scalar JSONSelection @specifiedBy(url: "...")

        """
        A string that declares a URL path with values interpolated inside `{}`.

        Example: "/product/{$this.id}/reviews?count={$args.count}"
        """
        scalar URLPathTemplate @specifiedBy(url: "...")
    "#;
}
