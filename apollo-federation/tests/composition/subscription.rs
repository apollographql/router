use crate::composition::ServiceDefinition;
use crate::composition::test_helpers::compose_as_fed2_subgraphs;

#[ignore = "until merge implementation completed"]
#[test]
fn type_subscription_appears_in_the_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
                type Query {
                    me: User!
                }

                type Subscription {
                    onNewUser: User!
                }

                type User {
                    id: ID!
                    name: String!
                }
            "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
                type Query {
                    foo: Int
                }

                type Subscription {
                    bar: Int
                }
            "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]).unwrap();
    let schema = result.schema().schema().to_string();
    assert!(
        schema.contains(r#"onNewUser: User! @join__field(graph: SUBGRAPHA)"#),
        "expected Subscription.onNewUser to be owned by SUBGRAPHA with a join directive; schema was:\n{}",
        schema
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn directives_incompatible_with_subscriptions_wont_compose() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
                type Query {
                    me: User!
                }

                type Subscription {
                    onNewUser: User! @shareable
                }

                type User {
                    id: ID!
                    name: String!
                }
            "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
                type Query {
                    foo: Int
                }

                type Subscription {
                    bar: Int
                }
            "#,
    };

    let errors = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]).unwrap_err();
    assert_eq!(errors.len(), 1);
    let msg = errors.first().unwrap().to_string();
    assert_eq!(
        msg,
        "Fields on root level subscription object cannot be marked as shareable"
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn subscription_name_collisions_across_subgraphs_should_not_compose() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
                type Query {
                    me: User!
                }

                type Subscription {
                    onNewUser: User
                    foo: Int!
                }

                type User {
                    id: ID!
                    name: String!
                }
            "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
                type Query {
                    foo: Int
                }

                type Subscription {
                    foo: Int!
                }
            "#,
    };

    let errors = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]).unwrap_err();
    assert_eq!(errors.len(), 1);
    let msg = errors.first().unwrap().to_string();
    assert_eq!(
        msg,
        "Non-shareable field \"Subscription.foo\" is resolved from multiple subgraphs: it is resolved from subgraphs \"subgraphA\" and \"subgraphB\" and defined as non-shareable in all of them"
    );
}
