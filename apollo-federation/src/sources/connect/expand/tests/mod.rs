use insta::{assert_debug_snapshot, assert_snapshot};

use crate::sources::connect::expand::{expand_connectors, ExpansionResult};

#[test]
fn it_expands_a_supergraph() {
    let to_expand = include_str!("./schemas/simple.graphql");
    let ExpansionResult::Expanded {
        raw_sdl,
        api_schema,
        connectors_by_service_name,
    } = expand_connectors(to_expand).unwrap()
    else {
        panic!("expected expansion to actually expand subgraphs");
    };

    assert_snapshot!(api_schema, @r###"
    directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

    directive @stream(label: String, if: Boolean! = true, initialCount: Int = 0) on FIELD

    type Query {
      users: [User]
      user(id: ID!): User
    }

    type User {
      id: ID!
      a: String
      b: String
      c: String
      d: String
    }
    "###);
    assert_debug_snapshot!(connectors_by_service_name, @r###"
    {
        "connectors_Query_users_0": Connector {
            id: ConnectId {
                label: "connectors.example http: Get ",
                subgraph_name: "connectors",
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(Query.users),
                    directive_name: "connect",
                    directive_index: 0,
                },
            },
            transport: HttpJson(
                HttpJsonTransport {
                    base_url: "http://example",
                    path_template: URLPathTemplate {
                        path: [],
                        query: {},
                    },
                    method: Get,
                    headers: {},
                    body: None,
                },
            ),
            selection: Named(
                SubSelection {
                    selections: [
                        Field(
                            None,
                            "id",
                            None,
                        ),
                        Field(
                            None,
                            "a",
                            None,
                        ),
                    ],
                    star: None,
                },
            ),
            entity: false,
            on_root_type: true,
        },
        "connectors_Query_user_0": Connector {
            id: ConnectId {
                label: "connectors.example http: Get /{$args.id!}",
                subgraph_name: "connectors",
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(Query.user),
                    directive_name: "connect",
                    directive_index: 0,
                },
            },
            transport: HttpJson(
                HttpJsonTransport {
                    base_url: "http://example",
                    path_template: URLPathTemplate {
                        path: [
                            ParameterValue {
                                parts: [
                                    Var(
                                        VariableExpression {
                                            var_path: "$args.id",
                                            batch_separator: None,
                                            required: true,
                                        },
                                    ),
                                ],
                            },
                        ],
                        query: {},
                    },
                    method: Get,
                    headers: {},
                    body: None,
                },
            ),
            selection: Named(
                SubSelection {
                    selections: [
                        Field(
                            None,
                            "id",
                            None,
                        ),
                        Field(
                            None,
                            "a",
                            None,
                        ),
                        Field(
                            None,
                            "b",
                            None,
                        ),
                    ],
                    star: None,
                },
            ),
            entity: false,
            on_root_type: true,
        },
        "connectors_User_d_1": Connector {
            id: ConnectId {
                label: "connectors.example http: Get /{$this.c!}/d",
                subgraph_name: "connectors",
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: Object(User.d),
                    directive_name: "connect",
                    directive_index: 1,
                },
            },
            transport: HttpJson(
                HttpJsonTransport {
                    base_url: "http://example",
                    path_template: URLPathTemplate {
                        path: [
                            ParameterValue {
                                parts: [
                                    Var(
                                        VariableExpression {
                                            var_path: "$this.c",
                                            batch_separator: None,
                                            required: true,
                                        },
                                    ),
                                ],
                            },
                            ParameterValue {
                                parts: [
                                    Text(
                                        "d",
                                    ),
                                ],
                            },
                        ],
                        query: {},
                    },
                    method: Get,
                    headers: {},
                    body: None,
                },
            ),
            selection: Path(
                Var(
                    "$",
                    Empty,
                ),
            ),
            entity: false,
            on_root_type: false,
        },
    }
    "###);
    assert_snapshot!(raw_sdl, @r###"
    schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
      query: Query
    }

    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

    directive @join__graph(name: String!, url: String!) on ENUM_VALUE

    directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on ENUM | INPUT_OBJECT | INTERFACE | OBJECT | SCALAR | UNION

    directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

    directive @join__implements(graph: join__Graph!, interface: String!) repeatable on INTERFACE | OBJECT

    directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

    directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

    enum link__Purpose {
      """
      SECURITY features provide metadata necessary to securely resolve fields.
      """
      SECURITY
      """EXECUTION features provide metadata necessary for operation execution."""
      EXECUTION
    }

    scalar link__Import

    scalar join__FieldSet

    enum join__Graph {
      CONNECTORS_QUERY_USER_0 @join__graph(name: "connectors_Query_user_0", url: "none")
      CONNECTORS_QUERY_USERS_0 @join__graph(name: "connectors_Query_users_0", url: "none")
      CONNECTORS_USER_D_1 @join__graph(name: "connectors_User_d_1", url: "none")
      GRAPHQL @join__graph(name: "graphql", url: "https://graphql")
    }

    type User @join__type(graph: CONNECTORS_QUERY_USER_0) @join__type(graph: CONNECTORS_QUERY_USERS_0) @join__type(graph: CONNECTORS_USER_D_1) @join__type(graph: GRAPHQL) {
      id: ID!
      a: String
      b: String
      d: String
      c: String
    }

    type Query @join__type(graph: CONNECTORS_QUERY_USER_0) @join__type(graph: CONNECTORS_QUERY_USERS_0) @join__type(graph: CONNECTORS_USER_D_1) @join__type(graph: GRAPHQL) {
      user(id: ID!): User @join__field(graph: CONNECTORS_QUERY_USER_0)
      users: [User] @join__field(graph: CONNECTORS_QUERY_USERS_0)
      _: ID @join__field(graph: CONNECTORS_USER_D_1)
    }
    "###);
}
