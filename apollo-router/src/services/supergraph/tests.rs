use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use http::HeaderValue;
use tower::ServiceExt;
use tower_service::Service;

use crate::graphql;
use crate::plugin::test::MockSubgraph;
use crate::services::router::ClientRequestAccepts;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::Schema;
use crate::test_harness::MockedSubgraphs;
use crate::Configuration;
use crate::Context;
use crate::Notify;
use crate::TestHarness;

const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
        subscription: Subscription
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }
   type Query {
       currentUser: User @join__field(graph: USER)
   }

   type Subscription @join__type(graph: USER) {
        userWasCreated: User
   }

   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       activeOrganization: Organization
   }
   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id") {
       id: ID
       creatorUser: User
       name: String
       nonNullId: ID!
       suborga: [Organization]
   }"#;

#[tokio::test]
async fn nullability_formatting() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": null }}}}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query("query { currentUser { activeOrganization { id creatorUser { name } } } }")
        .context(defer_context())
        // Request building here
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn root_selection_set_statically_skipped() {
    let subgraphs = MockedSubgraphs(
        [
            ("user", MockSubgraph::default()),
            ("orga", MockSubgraph::default()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query("query { currentUser @skip(if: true) { id name activeOrganization { id } } }")
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn root_selection_set_with_fragment_statically_skipped() {
    let subgraphs = MockedSubgraphs(
        [
            ("user", MockSubgraph::default()),
            ("orga", MockSubgraph::default()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(
            r#"query {
  ...TestFragment
}

fragment TestFragment on Query {
  currentUser @skip(if: true) {
    id
    name
    activeOrganization {
      id
    }
  }
}"#,
        )
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn root_selection_set_with_inline_fragment_statically_skipped() {
    let subgraphs = MockedSubgraphs(
        [
            ("user", MockSubgraph::default()),
            ("orga", MockSubgraph::default()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(
            r#"query {
  ... on Query {
    currentUser @skip(if: true) {
        id
        name
        activeOrganization {
        id
        }
    }
  }
}"#,
        )
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn root_selection_with_several_fields_statically_skipped() {
    let subgraphs = MockedSubgraphs(
        [
            ("user", MockSubgraph::default()),
            ("orga", MockSubgraph::default()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(
            r#"query {
  ...TestFragment
  currentUser @skip(if: true) {
    id
    name
    activeOrganization {
      id
    }
  }
}

fragment TestFragment on Query {
  currentUser @skip(if: true) {
    id
    name
    activeOrganization {
      id
    }
  }
}"#,
        )
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn root_selection_skipped_with_other_fields() {
    const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
        subscription: Subscription
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }
   type Query {
       currentUser: User @join__field(graph: USER)
       otherUser: User @join__field(graph: USER)
   }

   type Subscription @join__type(graph: USER) {
        userWasCreated: User
   }

   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       activeOrganization: Organization
   }
   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id") {
       id: ID
       creatorUser: User
       name: String
       nonNullId: ID!
       suborga: [Organization]
   }"#;
    let subgraphs = MockedSubgraphs(
        [
            (
                "user",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{"query":"query($skip:Boolean=false){currentUser@skip(if:$skip){id name activeOrganization{id}}otherUser{id name}}","variables":{"skip":true}}},
                        serde_json::json! {{"data": {"otherUser": { "id": "2", "name": "test" }}}},
                    )
                    .build(),
            ),
            ("orga", MockSubgraph::default()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query("query ($skip: Boolean = false) { currentUser @skip(if: $skip) { id name activeOrganization { id } } otherUser { id name } }")
        .variable("skip", true)
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn root_selection_skipped() {
    let subgraphs = MockedSubgraphs(
        [
            ("user", MockSubgraph::default()),
            ("orga", MockSubgraph::default()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query("query ($skip: Boolean = false) { currentUser @skip(if: $skip) { id name activeOrganization { id } } }")
        .variable("skip", true)
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn root_selection_not_skipped() {
    let subgraphs = MockedSubgraphs(
        [
            ("user", MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{"query":"{currentUser{id name}}"}},
                        serde_json::json! {{"data": {"currentUser": { "id": "2", "name": "test" }}}},
                    )
                    .build()),
            ("orga", MockSubgraph::default()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query("query ($skip: Boolean = false) { currentUser @skip(if: $skip) { id name } }")
        .variable("skip", false)
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn root_selection_not_included() {
    let subgraphs = MockedSubgraphs(
        [
            ("user", MockSubgraph::default()),
            ("orga", MockSubgraph::default()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query("query ($include: Boolean = false) { currentUser @include(if: $include) { id name activeOrganization { id } } }")
        .variable("include", false)
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn nullability_bubbling() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": {} }}}}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query("query { currentUser { activeOrganization { nonNullId creatorUser { name } } } }")
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn errors_on_deferred_responses() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{__typename id}}"}},
                serde_json::json!{{"data": {"currentUser": { "__typename": "User", "id": "0" }}}}
            )
            .with_json(
                serde_json::json!{{
                    "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                    "variables": {
                        "representations":[{"__typename": "User", "id":"0"}]
                    }
                }},
                serde_json::json!{{
                    "data": {
                        "_entities": [{ "suborga": [
                        { "__typename": "User", "name": "AAA"},
                        ] }]
                    },
                    "errors": [
                        {
                            "message": "error user 0",
                            "path": ["_entities", 0],
                        }
                    ]
                    }}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query("query { currentUser { id  ...@defer { name } } }")
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn errors_from_primary_on_deferred_responses() {
    let schema = r#"
        schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
        {
          query: Query
        }

        directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        scalar link__Import
        enum link__Purpose {
          SECURITY
          EXECUTION
        }

        type Computer
          @join__type(graph: COMPUTERS)
        {
          id: ID!
          errorField: String
          nonNullErrorField: String!
        }

        scalar join__FieldSet

        enum join__Graph {
          COMPUTERS @join__graph(name: "computers", url: "http://localhost:4001/")
        }


        type Query
          @join__type(graph: COMPUTERS)
        {
          computer(id: ID!): Computer
        }"#;

    let subgraphs = MockedSubgraphs([
        ("computers", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{__typename id}}"}},
                serde_json::json!{{"data": {"currentUser": { "__typename": "User", "id": "0" }}}}
            )
            .with_json(
                serde_json::json!{{
                    "query":"{computer(id:\"Computer1\"){id errorField}}",
                }},
                serde_json::json!{{
                    "data": {
                        "computer": {
                            "id": "Computer1"
                        }
                    },
                    "errors": [
                        {
                            "message": "Error field",
                            "locations": [
                                {
                                    "line": 1,
                                    "column": 93
                                }
                            ],
                            "path": ["computer","errorField"],
                        }
                    ]
                    }}
            ).build()),
        ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            r#"query {
                computer(id: "Computer1") {
                  id
                  ...ComputerErrorField @defer
                }
              }
              fragment ComputerErrorField on Computer {
                errorField
              }"#,
        )
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn deferred_fragment_bounds_nullability() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Organization", "id":"1"},
                        {"__typename": "Organization", "id":"2"},
                        {"__typename": "Organization", "id":"3"}

                        ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ]
                },
                "errors": [
                    {
                        "message": "error orga 1",
                        "path": ["_entities", 0],
                    },
                    {
                        "message": "error orga 3",
                        "path": ["_entities", 2],
                    }
                ]
                }}
        ).build())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
                "query { currentUser { activeOrganization { id  suborga { id ...@defer { nonNullId } } } } }",
            )
            .build()
            .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn errors_on_incremental_responses() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Organization", "id":"1"},
                        {"__typename": "Organization", "id":"2"},
                        {"__typename": "Organization", "id":"3"}

                        ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ]
                },
                "errors": [
                    {
                        "message": "error orga 1",
                        "path": ["_entities", 0],
                    },
                    {
                        "message": "error orga 3",
                        "path": ["_entities", 2],
                    }
                ]
                }}
        ).build())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
                "query { currentUser { activeOrganization { id  suborga { id ...@defer { name } } } } }",
            )
            .build()
            .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn root_typename_with_defer() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Organization", "id":"1"},
                        {"__typename": "Organization", "id":"2"},
                        {"__typename": "Organization", "id":"3"}

                        ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ]
                }
                }}
        ).build())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query(
                "query { __typename currentUser { activeOrganization { id  suborga { id ...@defer { name } } } } }",
            )
            .build()
            .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let res = stream.next_response().await.unwrap();
    assert_eq!(
        res.data.as_ref().unwrap().get("__typename"),
        Some(&serde_json_bytes::Value::String("Query".into()))
    );
    insta::assert_json_snapshot!(res);

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn subscription_with_callback() {
    let mut notify = Notify::builder().build();
    let (handle, _) = notify
        .create_or_subscribe("TEST_TOPIC".to_string(), false)
        .await
        .unwrap();
    let subgraphs = MockedSubgraphs([
            ("user", MockSubgraph::builder().with_json(
                    serde_json::json!{{"query":"subscription{userWasCreated{name activeOrganization{__typename id}}}"}},
                    serde_json::json!{{"data": {"userWasCreated": { "__typename": "User", "id": "1", "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
                ).with_subscription_stream(handle.clone()).build()),
            ("orga", MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{id name}}}}",
                    "variables": {
                        "representations":[{"__typename": "Organization", "id":"0"}]
                    }
                }},
                serde_json::json!{{
                    "data": {
                        "_entities": [{ "suborga": [
                        { "__typename": "Organization", "id": "1", "name": "A"},
                        { "__typename": "Organization", "id": "2", "name": "B"},
                        { "__typename": "Organization", "id": "3", "name": "C"},
                        ] }]
                    },
                    }}
            ).build())
        ].into_iter().collect());

    let mut configuration: Configuration = serde_json::from_value(serde_json::json!({"include_subgraph_errors": { "all": true }, "subscription": { "enabled": true, "mode": {"callback": {"public_url": "http://localhost:4545/callback"}}}})).unwrap();
    configuration.notify = notify.clone();
    let service = TestHarness::builder()
        .configuration(Arc::new(configuration))
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
            .query(
                "subscription { userWasCreated { name activeOrganization { id  suborga { id name } } } }",
            )
            .context(subscription_context())
            .build()
            .unwrap();
    let mut stream = service.oneshot(request).await.unwrap();
    let res = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(res);
    notify.broadcast(graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": { "name": "test", "activeOrganization": { "__typename": "Organization", "id": "0" }}})).build()).await.unwrap();
    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    // error happened
    notify
        .broadcast(
            graphql::Response::builder()
                .error(
                    graphql::Error::builder()
                        .message("cannot fetch the name")
                        .extension_code("INVALID")
                        .build(),
                )
                .build(),
        )
        .await
        .unwrap();
    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn subscription_callback_schema_reload() {
    let mut notify = Notify::builder().build();
    let (handle, _) = notify
        .create_or_subscribe("TEST_TOPIC".to_string(), false)
        .await
        .unwrap();
    let orga_subgraph = MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{id name}}}}",
                    "variables": {
                        "representations":[{"__typename": "Organization", "id":"0"}]
                    }
                }},
                serde_json::json!{{
                    "data": {
                        "_entities": [{ "suborga": [
                        { "__typename": "Organization", "id": "1", "name": "A"},
                        { "__typename": "Organization", "id": "2", "name": "B"},
                        { "__typename": "Organization", "id": "3", "name": "C"},
                        ] }]
                    },
                    }}
            ).build().with_map_request(|req: subgraph::Request| {
                assert!(req.subgraph_request.headers().contains_key("x-test"));
                assert_eq!(req.subgraph_request.headers().get("x-test").unwrap(), HeaderValue::from_static("test"));
                req
            });
    let subgraphs = MockedSubgraphs([
            ("user", MockSubgraph::builder().with_json(
                    serde_json::json!{{"query":"subscription{userWasCreated{name activeOrganization{__typename id}}}"}},
                    serde_json::json!{{"data": {"userWasCreated": { "__typename": "User", "id": "1", "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
                ).with_subscription_stream(handle.clone()).build()),
            ("orga", orga_subgraph)
        ].into_iter().collect());

    let mut configuration: Configuration = serde_json::from_value(serde_json::json!({"include_subgraph_errors": { "all": true }, "headers": {"all": {"request": [{"propagate": {"named": "x-test"}}]}}, "subscription": { "enabled": true, "mode": {"callback": {"public_url": "http://localhost:4545/callback"}}}})).unwrap();
    configuration.notify = notify.clone();
    let configuration = Arc::new(configuration);
    let service = TestHarness::builder()
        .configuration(configuration.clone())
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
            .query(
                "subscription { userWasCreated { name activeOrganization { id  suborga { id name } } } }",
            )
            .header("x-test", "test")
            .context(subscription_context())
            .build()
            .unwrap();
    let mut stream = service.oneshot(request).await.unwrap();
    let res = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(res);
    notify.broadcast(graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": { "name": "test", "activeOrganization": { "__typename": "Organization", "id": "0" }}})).build()).await.unwrap();
    insta::assert_json_snapshot!(stream.next_response().await.unwrap());

    let new_schema = format!("{SCHEMA}  ");
    // reload schema
    let schema = Schema::parse_test(&new_schema, &configuration).unwrap();
    notify.broadcast_schema(Arc::new(schema));
    insta::assert_json_snapshot!(tokio::time::timeout(
        Duration::from_secs(1),
        stream.next_response()
    )
    .await
    .unwrap()
    .unwrap());
}

#[tokio::test]
async fn subscription_with_callback_with_limit() {
    let mut notify = Notify::builder().build();
    let (handle, _) = notify
        .create_or_subscribe("TEST_TOPIC".to_string(), false)
        .await
        .unwrap();
    let subgraphs = MockedSubgraphs([
            ("user", MockSubgraph::builder().with_json(
                    serde_json::json!{{"query":"subscription{userWasCreated{name activeOrganization{__typename id}}}"}},
                    serde_json::json!{{"data": {"userWasCreated": { "__typename": "User", "id": "1", "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
                ).with_subscription_stream(handle.clone()).build()),
            ("orga", MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{id name}}}}",
                    "variables": {
                        "representations":[{"__typename": "Organization", "id":"0"}]
                    }
                }},
                serde_json::json!{{
                    "data": {
                        "_entities": [{ "suborga": [
                        { "__typename": "Organization", "id": "1", "name": "A"},
                        { "__typename": "Organization", "id": "2", "name": "B"},
                        { "__typename": "Organization", "id": "3", "name": "C"},
                        ] }]
                    },
                    }}
            ).build())
        ].into_iter().collect());

    let mut configuration: Configuration = serde_json::from_value(serde_json::json!({"include_subgraph_errors": { "all": true }, "subscription": { "enabled": true, "max_opened_subscriptions": 1, "mode": {"callback": {"public_url": "http://localhost:4545/callback"}}}})).unwrap();
    configuration.notify = notify.clone();
    let mut service = TestHarness::builder()
        .configuration(Arc::new(configuration))
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
            .query(
                "subscription { userWasCreated { name activeOrganization { id  suborga { id name } } } }",
            )
            .context(subscription_context())
            .build()
            .unwrap();
    let mut stream = service.ready().await.unwrap().call(request).await.unwrap();
    let res = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(res);
    notify.broadcast(graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": { "name": "test", "activeOrganization": { "__typename": "Organization", "id": "0" }}})).build()).await.unwrap();
    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    // error happened
    notify
        .broadcast(
            graphql::Response::builder()
                .error(
                    graphql::Error::builder()
                        .message("cannot fetch the name")
                        .extension_code("INVALID")
                        .build(),
                )
                .build(),
        )
        .await
        .unwrap();
    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    let request = supergraph::Request::fake_builder()
            .query(
                "subscription { userWasCreated { name activeOrganization { id  suborga { id name } } } }",
            )
            .context(subscription_context())
            .build()
            .unwrap();
    let mut stream_2 = service.ready().await.unwrap().call(request).await.unwrap();
    let res = stream_2.next_response().await.unwrap();
    assert!(!res.errors.is_empty());
    insta::assert_json_snapshot!(res);
    drop(stream);
    drop(stream_2);
    let request = supergraph::Request::fake_builder()
            .query(
                "subscription { userWasCreated { name activeOrganization { id  suborga { id name } } } }",
            )
            .context(subscription_context())
            .build()
            .unwrap();
    // Wait a bit to ensure all the closed signals has been triggered
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut stream_2 = service.ready().await.unwrap().call(request).await.unwrap();
    let res = stream_2.next_response().await.unwrap();
    assert!(res.errors.is_empty());
}

#[tokio::test]
async fn subscription_without_header() {
    let subgraphs = MockedSubgraphs(HashMap::new());
    let configuration: Configuration = serde_json::from_value(serde_json::json!({"include_subgraph_errors": { "all": true }, "subscription": { "enabled": true, "mode": {"callback": {"public_url": "http://localhost:4545/callback"}}}})).unwrap();
    let service = TestHarness::builder()
        .configuration(Arc::new(configuration))
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
            .query(
                "subscription { userWasCreated { name activeOrganization { id  suborga { id name } } } }",
            )
            .build()
            .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let res = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(res);
}

#[tokio::test]
async fn root_typename_with_defer_and_empty_first_response() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Organization", "id":"1"},
                        {"__typename": "Organization", "id":"2"},
                        {"__typename": "Organization", "id":"3"}

                        ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ]
                }
                }}
        ).build())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query(
                "query { __typename ... @defer { currentUser { activeOrganization { id  suborga { id name } } } } }",
            )
            .build()
            .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let res = stream.next_response().await.unwrap();
    assert_eq!(
        res.data.as_ref().unwrap().get("__typename"),
        Some(&serde_json_bytes::Value::String("Query".into()))
    );

    // Must have 2 chunks
    let _ = stream.next_response().await.unwrap();
}

#[tokio::test]
async fn root_typename_with_defer_in_defer() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id name}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        ).build())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query(
                "query { ...@defer { __typename currentUser { activeOrganization { id  suborga { id name } } } } }",
            )
            .build()
            .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let res = stream.next_response().await.unwrap();
    assert_eq!(res.errors, []);
    let res = stream.next_response().await.unwrap();
    assert_eq!(
        res.incremental
            .first()
            .unwrap()
            .data
            .as_ref()
            .unwrap()
            .get("__typename"),
        Some(&serde_json_bytes::Value::String("Query".into()))
    );
}

#[tokio::test]
async fn query_reconstruction() {
    let schema = r#"schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
    @link(url: "https://specs.apollo.dev/tag/v0.2")
    @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
  {
    query: Query
    mutation: Mutation
  }

  directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

  directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

  directive @join__graph(name: String!, url: String!) on ENUM_VALUE

  directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

  directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

  directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

  directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

  scalar join__FieldSet

  enum join__Graph {
    PRODUCTS @join__graph(name: "products", url: "http://products:4000/graphql")
    USERS @join__graph(name: "users", url: "http://users:4000/graphql")
  }

  scalar link__Import

  enum link__Purpose {
    SECURITY
    EXECUTION
  }

  type MakePaymentResult
    @join__type(graph: USERS)
  {
    id: ID!
    paymentStatus: PaymentStatus
  }

  type Mutation
    @join__type(graph: USERS)
  {
    makePayment(userId: ID!): MakePaymentResult!
  }


 type PaymentStatus
    @join__type(graph: USERS)
  {
    id: ID!
  }

  type Query
    @join__type(graph: PRODUCTS)
    @join__type(graph: USERS)
  {
    name: String
  }
  "#;

    // this test does not need to generate a valid response, it is only here to check
    // that the router does not panic when reconstructing the query for the deferred part
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            r#"mutation ($userId: ID!) {
                    makePayment(userId: $userId) {
                      id
                      ... @defer {
                        paymentStatus {
                          id
                        }
                      }
                    }
                  }"#,
        )
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

// if a deferred response falls under a path that was nullified in the primary response,
// the deferred response must not be sent
#[tokio::test]
async fn filter_nullified_deferred_responses() {
    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder()
        .with_json(
            serde_json::json!{{"query":"{currentUser{__typename name id}}"}},
            serde_json::json!{{"data": {"currentUser": { "__typename": "User", "name": "Ada", "id": "1" }}}}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{org:activeOrganization{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "User", "id":"1"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        {
                            "org": {
                                "__typename": "Organization", "id": "2"
                            }
                        }
                    ]
                }
                }})
                .with_json(
                    serde_json::json!{{
                        "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                        "variables": {
                            "representations":[{"__typename": "User", "id":"3"}]
                        }
                    }},
                    serde_json::json!{{
                        "data": {
                            "_entities": [
                                {
                                    "name": "A"
                                }
                            ]
                        }
                        }})
       .build()),
        ("orga", MockSubgraph::builder()
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{creatorUser{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"2"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        {
                            "creatorUser": {
                                "__typename": "User", "id": "3"
                            }
                        }
                    ]
                }
                }})
                .with_json(
                    serde_json::json!{{
                        "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{nonNullId}}}",
                        "variables": {
                            "representations":[{"__typename": "Organization", "id":"2"}]
                        }
                    }},
                    serde_json::json!{{
                        "data": {
                            "_entities": [
                                {
                                    "nonNullId": null
                                }
                            ]
                        }
                        }}).build())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(
            r#"query {
                currentUser {
                    name
                    ... @defer {
                        org: activeOrganization {
                            id
                            nonNullId
                            ... @defer {
                                creatorUser {
                                    name
                                }
                            }
                        }
                    }
                }
            }"#,
        )
        .context(defer_context())
        .build()
        .unwrap();
    let mut response = service.oneshot(request).await.unwrap();

    let primary = response.next_response().await.unwrap();
    insta::assert_json_snapshot!(primary);

    let deferred = response.next_response().await.unwrap();
    insta::assert_json_snapshot!(deferred);

    // the last deferred response was replace with an empty response,
    // to still have one containing has_next = false
    let last = response.next_response().await.unwrap();
    insta::assert_json_snapshot!(last);
}

#[tokio::test]
async fn reconstruct_deferred_query_under_interface() {
    let schema = r#"schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
            @link(url: "https://specs.apollo.dev/tag/v0.2")
            @link(url: "https://specs.apollo.dev/inaccessible/v0.2")
            {
                query: Query
            }

            directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
            directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
            directive @join__graph(name: String!, url: String!) on ENUM_VALUE
            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
            directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            scalar join__FieldSet
            enum join__Graph {
                USER @join__graph(name: "user", url: "http://localhost:4000/graphql")
            }
            scalar link__Import
            enum link__Purpose {
                SECURITY
                EXECUTION
            }
            type Query
            @join__type(graph: USER)
            {
            me: Identity @join__field(graph: USER)
            }
            interface Identity
            @join__type(graph: USER)
            {
            id: ID!
            name: String!
            }

            type User implements Identity
                @join__implements(graph: USER, interface: "Identity")
                @join__type(graph: USER, key: "id")
            {
                fullName: String! @join__field(graph: USER)
                id: ID!
                memberships: [UserMembership!]!  @join__field(graph: USER)
                name: String! @join__field(graph: USER)
            }
            type UserMembership
                @join__type(graph: USER)
                @tag(name: "platform-api")
            {
                """The organization that the user belongs to."""
                account: Account!
                """The user's permission level within the organization."""
                permission: UserPermission!
            }
            enum UserPermission
            @join__type(graph: USER)
            {
                USER
                ADMIN
            }
            type Account
            @join__type(graph: USER, key: "id")
            {
                id: ID! @join__field(graph: USER)
                name: String!  @join__field(graph: USER)
            }"#;

    let subgraphs = MockedSubgraphs([
            ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":"{me{__typename ...on User{id fullName memberships{permission account{__typename id}}}}}"}},
            serde_json::json!{{"data": {"me": {
                "__typename": "User",
                "id": 0,
                "fullName": "A",
                "memberships": [
                    {
                        "permission": "USER",
                        "account": {
                            "__typename": "Account",
                            "id": 1
                        }
                    }
                ]
            }}}}
        ) .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Account{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Account", "id": 1}
                    ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        { "__typename": "Account", "id": 1, "name": "B"}
                    ]
                }
            }}).build()),
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            r#"query {
                    me {
                      ... on User {
                        id
                        fullName
                        memberships {
                          permission
                          account {
                            ... on Account @defer {
                              name
                            }
                          }
                        }
                      }
                    }
                  }"#,
        )
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

fn subscription_context() -> Context {
    let context = Context::new();
    context.extensions().lock().insert(ClientRequestAccepts {
        multipart_subscription: true,
        ..Default::default()
    });

    context
}

fn defer_context() -> Context {
    let context = Context::new();
    context.extensions().lock().insert(ClientRequestAccepts {
        multipart_defer: true,
        ..Default::default()
    });

    context
}

#[tokio::test]
async fn interface_object_typename_rewrites() {
    let schema = r#"
            schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
            {
              query: Query
            }

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
            directive @join__graph(name: String!, url: String!) on ENUM_VALUE
            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            type A implements I
              @join__implements(graph: S1, interface: "I")
              @join__type(graph: S1, key: "id")
            {
              id: ID!
              x: Int
              z: Int
              y: Int @join__field
            }

            type B implements I
              @join__implements(graph: S1, interface: "I")
              @join__type(graph: S1, key: "id")
            {
              id: ID!
              x: Int
              w: Int
              y: Int @join__field
            }

            interface I
              @join__type(graph: S1, key: "id")
              @join__type(graph: S2, key: "id", isInterfaceObject: true)
            {
              id: ID!
              x: Int @join__field(graph: S1)
              y: Int @join__field(graph: S2)
            }

            scalar join__FieldSet

            enum join__Graph {
              S1 @join__graph(name: "S1", url: "s1")
              S2 @join__graph(name: "S2", url: "s2")
            }

            scalar link__Import

            enum link__Purpose {
              SECURITY
              EXECUTION
            }

            type Query
              @join__type(graph: S1)
              @join__type(graph: S2)
            {
              iFromS1: I @join__field(graph: S1)
              iFromS2: I @join__field(graph: S2)
            }
        "#;

    let query = r#"
          {
            iFromS1 {
              ... on A {
                y
              }
            }
          }
        "#;

    let subgraphs = MockedSubgraphs([
            ("S1", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "{iFromS1{__typename ...on A{__typename id}}}",
                    }},
                    serde_json::json! {{
                        "data": {"iFromS1":{"__typename":"A","id":"idA"}}
                    }},
                )
                .build()),
            ("S2", MockSubgraph::builder()
                // Note that this query below will only match if the input rewrite in the query plan is handled
                // correctly. Otherwise, the `representations` in the variables will have `__typename = A`
                // instead of `__typename = I`.
                .with_json(
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on I{y}}}",
                        "variables":{"representations":[{"__typename":"I","id":"idA"}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"y":42}]}
                    }},
                )
                .build()),
        ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();

    assert_eq!(
        serde_json::to_value(&response.data).unwrap(),
        serde_json::json!({ "iFromS1": { "y": 42 } }),
    );
}

#[tokio::test]
async fn interface_object_response_processing() {
    let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

          type Book implements Product
            @join__implements(graph: PRODUCTS, interface: "Product")
            @join__type(graph: PRODUCTS, key: "id")
          {
            id: ID!
            description: String
            price: Float
            pages: Int
            reviews: [Review!]! @join__field
          }

          scalar join__FieldSet

          enum join__Graph {
            PRODUCTS @join__graph(name: "products", url: "products")
            REVIEWS @join__graph(name: "reviews", url: "reviews")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Movie implements Product
            @join__implements(graph: PRODUCTS, interface: "Product")
            @join__type(graph: PRODUCTS, key: "id")
          {
            id: ID!
            description: String
            price: Float
            duration: Int
            reviews: [Review!]! @join__field
          }

          interface Product
            @join__type(graph: PRODUCTS, key: "id")
            @join__type(graph: REVIEWS, key: "id", isInterfaceObject: true)
          {
            id: ID!
            description: String @join__field(graph: PRODUCTS)
            price: Float @join__field(graph: PRODUCTS)
            reviews: [Review!]! @join__field(graph: REVIEWS)
          }

          type Query
            @join__type(graph: PRODUCTS)
            @join__type(graph: REVIEWS)
          {
            products: [Product!]! @join__field(graph: PRODUCTS)
            allReviewedProducts: [Product!]! @join__field(graph: REVIEWS)
            bestRatedProducts(limit: Int): [Product!]! @join__field(graph: REVIEWS)
          }

          type Review
            @join__type(graph: REVIEWS)
          {
            author: String
            text: String
            rating: Int
          }
        "#;

    let query = r#"
          {
            allReviewedProducts {
              id
              price
            }
          }
        "#;

    let subgraphs = MockedSubgraphs([
            ("products", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{__typename price}}}",
                        "variables": {"representations":[{"__typename":"Product","id":"1"},{"__typename":"Product","id":"2"}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"price":12.99},{"price":14.99}]}
                    }},
                )
                .build()),
            ("reviews", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "{allReviewedProducts{__typename id}}"
                    }},
                    serde_json::json! {{
                        "data": {"allReviewedProducts":[{"__typename":"Product","id":"1"},{"__typename":"Product","id":"2"}]}
                    }},
                )
                .build()),
        ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();

    assert_eq!(
        serde_json::to_value(&response.data).unwrap(),
        serde_json::json!({ "allReviewedProducts": [ {"id": "1", "price": 12.99}, {"id": "2", "price": 14.99} ]}),
    );
}

#[tokio::test]
async fn only_query_interface_object_subgraph() {
    // This test has 2 subgraphs, one with an interface and another with that interface
    // declared as an @interfaceObject. It then sends a query that can be entirely
    // fulfilled by the @interfaceObject subgraph (in particular, it doesn't request
    // __typename; if it did, it would force a query on the other subgraph to obtain
    // the actual implementation type).
    // The specificity here is that the final in-memory result will not have a __typename
    // _despite_ being the parent type of that result being an interface. Which is fine
    // since __typename is not requested, and so there is no need to known the actual
    // __typename, but this is something that never happen outside of @interfaceObject
    // (usually, results whose parent type is an abstract type (say an interface) are always
    // queried internally with their __typename). And so this test make sure that the
    // post-processing done by the router on the result handle this correctly.

    let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

          directive @join__graph(name: String!, url: String!) on ENUM_VALUE

          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

          type A implements I
            @join__implements(graph: S1, interface: "I")
            @join__type(graph: S1, key: "id")
          {
            id: ID!
            x: Int
            z: Int
            y: Int @join__field
          }

          type B implements I
            @join__implements(graph: S1, interface: "I")
            @join__type(graph: S1, key: "id")
          {
            id: ID!
            x: Int
            w: Int
            y: Int @join__field
          }

          interface I
            @join__type(graph: S1, key: "id")
            @join__type(graph: S2, key: "id", isInterfaceObject: true)
          {
            id: ID!
            x: Int @join__field(graph: S1)
            y: Int @join__field(graph: S2)
          }

          scalar join__FieldSet

          enum join__Graph {
            S1 @join__graph(name: "S1", url: "S1")
            S2 @join__graph(name: "S2", url: "S2")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Query
            @join__type(graph: S1)
            @join__type(graph: S2)
          {
            iFromS1: I @join__field(graph: S1)
            iFromS2: I @join__field(graph: S2)
          }
        "#;

    let query = r#"
          {
            iFromS2 {
              y
            }
          }
        "#;

    let subgraphs = MockedSubgraphs(
        [
            (
                "S1",
                MockSubgraph::builder()
                    // This test makes no queries to S1, only to S2
                    .build(),
            ),
            (
                "S2",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{
                            "query": "{iFromS2{y}}",
                        }},
                        serde_json::json! {{
                            "data": {"iFromS2":{"y":20}}
                        }},
                    )
                    .build(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();

    assert_eq!(
        serde_json::to_value(&response.data).unwrap(),
        serde_json::json!({ "iFromS2": { "y": 20 } }),
    );
}

#[tokio::test]
async fn aliased_subgraph_data_rewrites_on_root_fetch() {
    let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

          type A implements U
            @join__implements(graph: S1, interface: "U")
            @join__type(graph: S1, key: "g")
            @join__type(graph: S2, key: "g")
          {
            f: String @join__field(graph: S1, external: true) @join__field(graph: S2)
            g: String
          }

          type B implements U
            @join__implements(graph: S1, interface: "U")
            @join__type(graph: S1, key: "g")
            @join__type(graph: S2, key: "g")
          {
            f: String @join__field(graph: S1, external: true) @join__field(graph: S2)
            g: Int
          }

          scalar join__FieldSet

          enum join__Graph {
            S1 @join__graph(name: "S1", url: "s1")
            S2 @join__graph(name: "S2", url: "s2")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Query
            @join__type(graph: S1)
            @join__type(graph: S2)
          {
            us: [U] @join__field(graph: S1)
          }

          interface U
            @join__type(graph: S1)
          {
            f: String
          }
        "#;

    let query = r#"
          {
            us {
              f
            }
          }
        "#;

    let subgraphs = MockedSubgraphs([
            ("S1", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "{us{__typename ...on A{__typename g}...on B{__typename g__alias_0:g}}}",
                    }},
                    serde_json::json! {{
                        "data": {"us":[{"__typename":"A","g":"foo"},{"__typename":"B","g__alias_0":1}]},
                    }},
                )
                .build()),
            ("S2", MockSubgraph::builder()
                .with_json(
                    // Note that the query below will only match if the output rewrite in the query plan is handled
                    // correctly. Otherwise, the `representations` in the variables will not be able to find the
                    // field `g` for the `B` object, since it was returned as `g__alias_0` on the initial subgraph
                    // query above.
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on A{f}...on B{f}}}",
                        "variables":{"representations":[{"__typename":"A","g":"foo"},{"__typename":"B","g":1}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"f":"fA"},{"f":"fB"}]}
                    }},
                )
                .build()),
        ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();

    assert_eq!(
        serde_json::to_value(&response.data).unwrap(),
        serde_json::json!({"us": [{"f": "fA"}, {"f": "fB"}]}),
    );
}

#[tokio::test]
async fn aliased_subgraph_data_rewrites_on_non_root_fetch() {
    let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
          type A implements U
            @join__implements(graph: S1, interface: "U")
            @join__type(graph: S1, key: "g")
            @join__type(graph: S2, key: "g")
          {
            f: String @join__field(graph: S1, external: true) @join__field(graph: S2)
            g: String
          }

          type B implements U
            @join__implements(graph: S1, interface: "U")
            @join__type(graph: S1, key: "g")
            @join__type(graph: S2, key: "g")
          {
            f: String @join__field(graph: S1, external: true) @join__field(graph: S2)
            g: Int
          }

          scalar join__FieldSet

          enum join__Graph {
            S1 @join__graph(name: "S1", url: "s1")
            S2 @join__graph(name: "S2", url: "s2")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Query
            @join__type(graph: S1)
            @join__type(graph: S2)
          {
            t: T @join__field(graph: S2)
          }

          type T
            @join__type(graph: S1, key: "id")
            @join__type(graph: S2, key: "id")
          {
            id: ID!
            us: [U] @join__field(graph: S1)
          }

          interface U
            @join__type(graph: S1)
          {
            f: String
          }
        "#;

    let query = r#"
          {
            t {
              us {
                f
              }
            }
          }
        "#;

    let subgraphs = MockedSubgraphs([
            ("S1", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on T{us{__typename ...on A{__typename g}...on B{__typename g__alias_0:g}}}}}",
                        "variables":{"representations":[{"__typename":"T","id":"0"}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"us":[{"__typename":"A","g":"foo"},{"__typename":"B","g__alias_0":1}]}]},
                    }},
                )
                .build()),
            ("S2", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "{t{__typename id}}",
                    }},
                    serde_json::json! {{
                        "data": {"t":{"__typename":"T","id":"0"}},
                    }},
                )
                // Note that this query will only match if the output rewrite in the query plan is handled correctly. Otherwise,
                // the `representations` in the variables will not be able to find the field `g` for the `B` object, since it was
                // returned as `g__alias_0` on the (non-root) S1 query above.
                .with_json(
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on A{f}...on B{f}}}",
                        "variables":{"representations":[{"__typename":"A","g":"foo"},{"__typename":"B","g":1}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"f":"fA"},{"f":"fB"}]}
                    }},
                )
                .build()),
        ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();

    assert_eq!(
        serde_json::to_value(&response.data).unwrap(),
        serde_json::json!({"t": {"us": [{"f": "fA"}, {"f": "fB"}]}}),
    );
}

#[tokio::test]
async fn errors_on_nullified_paths() {
    let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
          directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

          scalar join__FieldSet

          enum join__Graph {
            S1 @join__graph(name: "S1", url: "s1")
            S2 @join__graph(name: "S2", url: "s2")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Query
          @join__type(graph: S1)
          {
            foo: Foo! @join__field(graph: S1)
          }

          type Foo
            @join__owner(graph: S1)
            @join__type(graph: S1)
          {
            id: ID! @join__field(graph: S1)
            bar: Bar! @join__field(graph: S1)
          }

          type Bar
          @join__owner(graph: S1)
          @join__type(graph: S1, key: "id")
          @join__type(graph: S2, key: "id") {
            id: ID! @join__field(graph: S1) @join__field(graph: S2)
            something: String @join__field(graph: S2)
          }
        "#;

    let query = r#"
          query Query {
            foo {
              id
              bar {
                id
                something
              }
            }
          }
        "#;

    let subgraphs = MockedSubgraphs([
        ("S1", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"query Query__S1__0{foo{id bar{__typename id}}}", "operationName": "Query__S1__0"}},
                serde_json::json!{{"data": {
                    "foo": {
                        "id": 1,
                        "bar": {
                            "__typename": "Bar",
                            "id": 2
                        }
                    }
                }}}
            )
          .build()),
        ("S2", MockSubgraph::builder()  .with_json(
            serde_json::json!{{
                "query":"query Query__S2__1($representations:[_Any!]!){_entities(representations:$representations){...on Bar{something}}}",
                "operationName": "Query__S2__1",
                "variables": {
                    "representations":[{"__typename": "Bar", "id": 2}]
                }
            }},
            serde_json::json!{{
                "data": {
                  "_entities": [
                    null
                  ]
                },
                "errors": [
                  {
                    "message": "Could not fetch bar",
                    "path": [
                      "_entities"
                    ],
                    "extensions": {
                      "code": "NOT_FOUND"
                    }
                  }
                ],
              }}
        ).build())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn missing_entities() {
    let subgraphs = MockedSubgraphs([
            ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{id activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "__typename": "User", "id": "0", "activeOrganization": { "__typename": "Organization", "id": "1" } } } }}
            ).build()),
            ("orga", MockSubgraph::builder().with_json(serde_json::json!{{"query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}","variables":{"representations":[{"__typename":"Organization","id":"1"}]}}},
                                                       serde_json::json!{{"data": {}, "errors":[{"message":"error"}]}}).build())
        ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query("query { currentUser { id  activeOrganization{ id name } } }")
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn no_typename_on_interface() {
    let subgraphs = MockedSubgraphs([
            ("animal", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"query dog__animal__0{dog{id name}}", "operationName": "dog__animal__0"}},
                serde_json::json!{{"data":{"dog":{"id":"4321","name":"Spot"}}}}
            ).with_json(
                serde_json::json!{{"query":"query dog__animal__0{dog{__typename id name}}", "operationName": "dog__animal__0"}},
                serde_json::json!{{"data":{"dog":{"__typename":"Dog","id":"8765","name":"Spot"}}}}
            ).with_json(
                serde_json::json!{{"query":"query dog__animal__0{dog{name id}}", "operationName": "dog__animal__0"}},
                serde_json::json!{{"data":{"dog":{"id":"0000","name":"Spot"}}}}
            ).build()),
        ].into_iter().collect());

    let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(
                r#"schema
                @core(feature: "https://specs.apollo.dev/core/v0.2"),
                @core(feature: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
              {
                query: Query
              }
              directive @core(as: String, feature: String!, for: core__Purpose) repeatable on SCHEMA
              directive @join__field(graph: join__Graph, provides: join__FieldSet, requires: join__FieldSet) on FIELD_DEFINITION
              directive @join__graph(name: String!, url: String!) on ENUM_VALUE
              directive @join__owner(graph: join__Graph!) on INTERFACE | OBJECT
              directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on INTERFACE | OBJECT

              interface Animal {
                id: String!
              }

              type Dog implements Animal {
                id: String!
                name: String!
              }

              type Query {
                animal: Animal! @join__field(graph: ANIMAL)
                dog: Dog! @join__field(graph: ANIMAL)
              }

              enum core__Purpose {
                """
                `EXECUTION` features provide metadata necessary to for operation execution.
                """
                EXECUTION

                """
                `SECURITY` features provide metadata necessary to securely resolve fields.
                """
                SECURITY
              }

              scalar join__FieldSet

              enum join__Graph {
                ANIMAL @join__graph(name: "animal" url: "http://localhost:8080/query")
              }
              "#,
            )
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            "query dog {
                dog {
                  ...on Animal {
                    id
                    ...on Dog {
                      name
                    }
                  }
                }
              }",
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();

    let no_typename = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(no_typename);

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            "query dog {
                dog {
                  ...on Animal {
                    id
                    __typename
                    ...on Dog {
                      name
                    }
                  }
                }
              }",
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();

    let with_typename = stream.next_response().await.unwrap();
    assert_eq!(
        with_typename
            .data
            .clone()
            .unwrap()
            .get("dog")
            .unwrap()
            .get("name")
            .unwrap(),
        no_typename
            .data
            .clone()
            .unwrap()
            .get("dog")
            .unwrap()
            .get("name")
            .unwrap(),
        "{:?}\n{:?}",
        with_typename,
        no_typename
    );
    insta::assert_json_snapshot!(with_typename);

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            "query dog {
                    dog {
                        ...on Dog {
                            name
                            ...on Animal {
                                id
                            }
                        }
                    }
                }",
        )
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();

    let with_reversed_fragments = stream.next_response().await.unwrap();
    assert_eq!(
        with_reversed_fragments
            .data
            .clone()
            .unwrap()
            .get("dog")
            .unwrap()
            .get("name")
            .unwrap(),
        no_typename
            .data
            .clone()
            .unwrap()
            .get("dog")
            .unwrap()
            .get("name")
            .unwrap(),
        "{:?}\n{:?}",
        with_reversed_fragments,
        no_typename
    );
    insta::assert_json_snapshot!(with_reversed_fragments);
}

#[tokio::test]
async fn aliased_typename_on_fragments() {
    let subgraphs = MockedSubgraphs([
            ("animal", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"query test__animal__0{dog{name nickname barkVolume}}", "operationName": "test__animal__0"}},
                serde_json::json!{{"data":{"dog":{"name":"Spot", "nickname": "Spo", "barkVolume": 7}}}}
            ).build()),
        ].into_iter().collect());

    let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(
                r#"schema
                @core(feature: "https://specs.apollo.dev/core/v0.2"),
                @core(feature: "https://specs.apollo.dev/join/v0.1", for: EXECUTION)
              {
                query: Query
              }
              directive @core(as: String, feature: String!, for: core__Purpose) repeatable on SCHEMA
              directive @join__field(graph: join__Graph, provides: join__FieldSet, requires: join__FieldSet) on FIELD_DEFINITION
              directive @join__graph(name: String!, url: String!) on ENUM_VALUE
              directive @join__owner(graph: join__Graph!) on INTERFACE | OBJECT
              directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
              directive @join__unionMember(
                graph: join__Graph!
                member: String!
              ) repeatable on UNION

              interface Animal {
                id: String!
              }

              type Dog implements Animal {
                id: String!
                name: String!
                nickname: String!
                barkVolume: Int
              }

              type Cat implements Animal {
                id: String!
                name: String!
                nickname: String!
                meowVolume: Int
              }

              type Query {
                animal: Animal! @join__field(graph: ANIMAL)
                dog: Dog! @join__field(graph: ANIMAL)
              }

              enum core__Purpose {
                """
                `EXECUTION` features provide metadata necessary to for operation execution.
                """
                EXECUTION

                """
                `SECURITY` features provide metadata necessary to securely resolve fields.
                """
                SECURITY
              }

              union CatOrDog
                @join__type(graph: ANIMAL)
                @join__unionMember(graph: ANIMAL, member: "Dog")
                @join__unionMember(graph: ANIMAL, member: "Cat") =
                  Cat | Dog

              scalar join__FieldSet

              enum join__Graph {
                ANIMAL @join__graph(name: "animal" url: "http://localhost:8080/query")
              }
              "#,
            )
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            "query test { dog { ...petFragment } } fragment petFragment on CatOrDog { ... on Dog { name nickname barkVolume } ... on Cat { name nickname meowVolume } }",
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();

    let aliased_typename = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(aliased_typename);
}

#[tokio::test]
async fn multiple_interface_types() {
    let schema = r#"
      schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
        query: Query
      }

      directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

      directive @join__field(
        graph: join__Graph
        requires: join__FieldSet
        provides: join__FieldSet
        type: String
        external: Boolean
        override: String
        usedOverridden: Boolean
      ) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

      directive @join__graph(name: String!, url: String!) on ENUM_VALUE

      directive @join__implements(
        graph: join__Graph!
        interface: String!
      ) repeatable on OBJECT | INTERFACE

      directive @join__type(
        graph: join__Graph!
        key: join__FieldSet
        extension: Boolean! = false
        resolvable: Boolean! = true
        isInterfaceObject: Boolean! = false
      ) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

      directive @join__unionMember(
        graph: join__Graph!
        member: String!
      ) repeatable on UNION

      directive @link(
        url: String
        as: String
        for: link__Purpose
        import: [link__Import]
      ) repeatable on SCHEMA

      directive @tag(
        name: String!
      ) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

      enum link__Purpose {
        EXECUTION
        SECURITY
      }

      scalar join__FieldSet
      scalar link__Import

      enum join__Graph {
        GRAPH1 @join__graph(name: "graph1", url: "http://localhost:8080/graph1")
      }

      type Query @join__type(graph: GRAPH1) {
        root(id: ID!): Root @join__field(graph: GRAPH1)
      }

      type Root @join__type(graph: GRAPH1, key: "id") {
        id: ID!
        operation(a: Int, b: Int): OperationResult!
      }

      union OperationResult
        @join__type(graph: GRAPH1)
        @join__unionMember(graph: GRAPH1, member: "Operation") =
          Operation

      type Operation @join__type(graph: GRAPH1) {
        id: ID!
        item: [OperationItem!]!
      }

      interface OperationItem @join__type(graph: GRAPH1) {
        type: OperationType!
      }

      enum OperationType @join__type(graph: GRAPH1) {
        ADD_ARGUMENT @join__enumValue(graph: GRAPH1)
      }

      interface OperationItemRootType implements OperationItem
        @join__implements(graph: GRAPH1, interface: "OperationItem")
        @join__type(graph: GRAPH1) {
        rootType: String!
        type: OperationType!
      }

      interface OperationItemStuff implements OperationItem
        @join__implements(graph: GRAPH1, interface: "OperationItem")
        @join__type(graph: GRAPH1) {
        stuff: String!
        type: OperationType!
      }

      type OperationAddArgument implements OperationItem & OperationItemStuff & OperationItemValue
        @join__implements(graph: GRAPH1, interface: "OperationItem")
        @join__implements(graph: GRAPH1, interface: "OperationItemStuff")
        @join__implements(graph: GRAPH1, interface: "OperationItemValue")
        @join__type(graph: GRAPH1) {
        stuff: String!
        type: OperationType!
        value: String!
      }

      interface OperationItemValue implements OperationItem
        @join__implements(graph: GRAPH1, interface: "OperationItem")
        @join__type(graph: GRAPH1) {
        type: OperationType!
        value: String!
      }

      type OperationRemoveSchemaRootOperation implements OperationItem & OperationItemRootType
        @join__implements(graph: GRAPH1, interface: "OperationItem")
        @join__implements(graph: GRAPH1, interface: "OperationItemRootType")
        @join__type(graph: GRAPH1) {
        rootType: String!
        type: OperationType!
      }
      "#;

    let query = r#"fragment OperationItemFragment on OperationItem {
            __typename
            ... on OperationItemStuff {
              __typename
              stuff
            }
            ... on OperationItemRootType {
              __typename
              rootType
            }
          }
          query MyQuery($id: ID!, $a: Int, $b: Int) {
            root(id: $id) {
              __typename
              operation(a: $a, b: $b) {
                __typename
                ... on Operation {
                  __typename
                  item {
                    __typename
                    ...OperationItemFragment
                    ... on OperationItemStuff {
                      __typename
                      stuff
                    }
                    ... on OperationItemValue {
                      __typename
                      value
                    }
                  }
                  id
                }
              }
              id
            }
          }"#;

    let subgraphs = MockedSubgraphs([
            // The response isn't interesting to us,
            // we just need to make sure the query makes it through parsing and validation
            ("graph1", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"query MyQuery__graph1__0($id:ID!$a:Int$b:Int){root(id:$id){__typename operation(a:$a b:$b){__typename ...on Operation{__typename item{__typename ...on OperationItemStuff{__typename stuff}...on OperationItemRootType{__typename rootType}...on OperationItemValue{__typename value}}id}}id}}", "operationName": "MyQuery__graph1__0", "variables":{"id":"1234","a":1,"b":2}}},
                serde_json::json!{{"data": null }}
            ).build()),
            ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(query)
        .variables(
            serde_json_bytes::json! {{ "id": "1234", "a": 1, "b": 2}}
                .as_object()
                .unwrap()
                .clone(),
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    assert_eq!(serde_json_bytes::Value::Null, response.data.unwrap());
}

#[tokio::test]
async fn id_scalar_can_overflow_i32() {
    // Hack to let the first subgraph fetch contain an ID variable:
    // ```
    // type Query {
    //     user(id: ID!): User @join__field(graph: USER)
    // }
    // ```
    assert!(SCHEMA.contains("currentUser:"));
    let schema = SCHEMA.replace("currentUser:", "user(id: ID!):");

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(&schema)
        .subgraph_hook(|_subgraph_name, _service| {
            tower::service_fn(|request: subgraph::Request| async move {
                let id = &request.subgraph_request.body().variables["id"];
                Err(format!("$id = {id}").into())
            })
            .boxed()
        })
        .build_supergraph()
        .await
        .unwrap();

    let large: i64 = 1 << 53;
    let large_plus_one = large + 1;
    // f64 rounds since it doesn’t have enough mantissa bits
    assert!(large_plus_one as f64 as i64 == large);
    // i64 of course doesn’t round
    assert!(large_plus_one != large);

    let request = supergraph::Request::fake_builder()
        .query("query($id: ID!) { user(id: $id) { name }}")
        .variable("id", large_plus_one)
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    // The router did not panic or respond with an early validation error.
    // Instead it did a subgraph fetch, which recieved the correct ID variable without rounding:
    assert_eq!(
        response.errors[0].extensions["reason"].as_str().unwrap(),
        "$id = 9007199254740993"
    );
    assert_eq!(large_plus_one.to_string(), "9007199254740993");
}

#[tokio::test]
async fn interface_object_typename() {
    let schema = r#"schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
  {
    query: Query
  }

  directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

  directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

  directive @join__graph(name: String!, url: String!) on ENUM_VALUE

  directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

  directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

  directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

  directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

  directive @owner(
    """Subgraph owner who owns this definition."""
    subgraph: String!
  ) on ARGUMENT_DEFINITION | ENUM | ENUM_VALUE | FIELD_DEFINITION | INPUT_OBJECT | INPUT_FIELD_DEFINITION | INTERFACE | OBJECT | SCALAR | UNION

  scalar join__FieldSet

  enum join__Graph {
    A @join__graph(name: "A", url: "https://localhost:4001")
    B @join__graph(name: "B", url: "https://localhost:4002")
  }

  scalar link__Import

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

  type ContactWrapper @join__type(graph: A) {
    inner: Contact!
  }

  interface Contact
    @join__type(graph: A)
    @join__type(graph: B, key: "id displayName", isInterfaceObject: true)
  {
    id: ID!
    displayName: String!
    country: String @join__field(graph: B)
  }

  type Person implements Contact
    @join__implements(graph: A, interface: "Contact")
    @join__type(graph: A, key: "id")
  {
    id: ID!
    displayName: String!
    country: String @join__field
  }

  type Query
    @join__type(graph: A)
  {
    searchContacts(name: String): [ContactWrapper!]! @join__field(graph: A)
  }
      "#;

    let subgraphs = MockedSubgraphs(
        [
            (
                "A",
                MockSubgraph::builder().with_json(
                    serde_json::json!{{"query":"{searchContacts(name:\"max\"){inner{__typename id displayName}}}"}},
                    serde_json::json!{{"data": {
                        "searchContacts": [
                            {
                                "inner": {
                                    "__typename": "Person",
                                    "displayName": "Max",
                                    "id": "0"
                                }
                            }
                        ]
                    } }}
                ).build(),
            ),
            (
                "B",
                MockSubgraph::builder().with_json(
                        serde_json::json!{{
                            "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Contact{__typename country}}}",
                            "variables": {
                                "representations": [
                                    {
                                        "__typename":"Contact",
                                        "id":"0",
                                        "displayName": "Max",
                                    }
                                ]
                            }
                        }},
                        serde_json::json!{{"data": {
                            "_entities": [{
                                "__typename":"Contact",
                                "country": "Fr"
                            }]
                         } }}
                    ).with_json(
                        serde_json::json!{{
                            "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Contact{country}}}",
                            "variables": {
                                "representations": [
                                    {
                                        "__typename":"Contact",
                                        "id":"0",
                                        "displayName": "Max",
                                    }
                                ]
                            }
                        }},
                        serde_json::json!{{"data": {
                            "_entities": [{
                                "country": "Fr"
                            }]
                         } }}
                    ).build(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            // this works
            /*r#"{
                    searchContacts(name: "max") {
                        inner {
                          __typename
                          ...on Contact {
                              __typename
                              country
                          }
                        }
                    }
                  }"#,*/
                  // this works too
                  /*
                  r#"{
              searchContacts(name: "max") {
                  inner {
                    ...F
                  }
              }
            }
            fragment F on Contact {
              country
            }"#,
                   */
            // this does not
            r#"{
        searchContacts(name: "max") {
            inner {
            __typename
              ...F
            }
        }
      }
      fragment F on Contact {
        __typename
        country
      }"#,
        )
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}

#[tokio::test]
async fn fragment_reuse() {
    const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }
   type Query {
       me: User @join__field(graph: USER)
   }

   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       organizations: [Organization] @join__field(graph: ORGA)
   }
   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id") {
       id: ID
       name: String
   }"#;

    let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{
                  "query":"query Query__user__0($a:Boolean!=true$b:Boolean!=true){me{name ...on User@include(if:$a){__typename id}...on User@include(if:$b){__typename id}}}",
                  "operationName": "Query__user__0"
                }},
                serde_json::json!{{"data": {"me": { "name": "Ada", "__typename": "User", "id": "1" }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
          serde_json::json!{{
            "query":"query Query__orga__1($representations:[_Any!]!$a:Boolean!=true$b:Boolean!=true){_entities(representations:$representations){...F@include(if:$a)...F@include(if:$b)}}fragment F on User{organizations{id name}}",
            "operationName": "Query__orga__1",
            "variables":{"representations":[{"__typename":"User","id":"1"}]}
          }},
          serde_json::json!{{"data": {"_entities": [{ "organizations": [{"id": "2", "name": "Apollo"}] }]}}}
      ).build())
    ].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(
            r#"query Query($a: Boolean! = true, $b: Boolean! = true) {
            me {
              name
              ...F @include(if: $a)
              ...F @include(if: $b)
            }
          }
          fragment F on User {
            organizations {
              id
              name
            }
          }"#,
        )
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn abstract_types_in_requires() {
    let schema = r#"schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
  {
    query: Query
  }

  directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

  directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

  directive @join__graph(name: String!, url: String!) on ENUM_VALUE

  directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

  directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

  directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

  directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

  scalar join__FieldSet

  enum join__Graph {
    A @join__graph(name: "A", url: "https://localhost:4001")
    B @join__graph(name: "B", url: "https://localhost:4002")
  }

  scalar link__Import

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

  type Query
    @join__type(graph: A)
  {
    entity: Entity @join__field(graph: A)
  }

  type Entity
    @join__type(graph: A, key: "id")
    @join__type(graph: B, key: "id")
  {
    id: ID
    if: If @join__field(graph: A) @join__field(graph: B, external: true)
    un: Un @join__field(graph: A) @join__field(graph: B, external: true)
    fieldWithDependencies: String
      @join__field(graph: B, requires: "if { i ... on IfA { a } } un { ... on UnZ { z } }")
  }

  interface If @join__type(graph: A) @join__type(graph: B) {
    i: ID
  }

  type IfA implements If
    @join__type(graph: A)
    @join__type(graph: B)
    @join__implements(graph: A, interface: "If")
    @join__implements(graph: B, interface: "If")
  {
    i: ID
    a: ID
  }

  type UnZ @join__type(graph: A) @join__type(graph: B) {
    z: ID
  }

  union Un
    @join__type(graph: A)
    @join__type(graph: B)
    @join__unionMember(graph: A, member: "UnZ")
    @join__unionMember(graph: B, member: "UnZ") =
      UnZ
      "#;

    let subgraphs = MockedSubgraphs(
        [
            (
                "A",
                MockSubgraph::builder().with_json(
                    serde_json::json!({"query":"{entity{__typename id if{__typename i ...on IfA{a}}un{__typename ...on UnZ{z}}}}"}),
                    serde_json::json!{{"data": {
                        "entity": {
                            "__typename": "Entity",
                            "id": "1",
                            "if": {
                              "__typename": "IfA",
                              "i": "i",
                              "a": "a"
                            },
                            "un": {
                              "__typename": "UnZ",
                              "z": "z"
                            }
                        }
                    } }}
                ).build(),
            ),
            (
                "B",
                MockSubgraph::builder().with_json(
                    serde_json::json!{{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Entity{fieldWithDependencies}}}",
                        "variables": {
                          "representations": [
                            {
                              "__typename": "Entity",
                              "id": "1",
                              "if": {
                                "i": "i",
                                "a": "a"
                              },
                              "un": {
                                "z": "z"
                              }
                            }
                          ]
                        }
                    }},
                    serde_json::json!{{"data": {
                        "_entities": [{
                            "__typename":"Entity",
                            "fieldWithDependencies": "success"
                        }]
                    } }}
                ).build(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
            "query {
            entity {
              fieldWithDependencies
            }
          }
          ",
        )
        .build()
        .unwrap();

    let mut stream = service.oneshot(request).await.unwrap();
    insta::assert_json_snapshot!(stream.next_response().await.unwrap());
}
