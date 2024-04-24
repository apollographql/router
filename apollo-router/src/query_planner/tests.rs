use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use apollo_compiler::name;
use futures::StreamExt;
use http::Method;
use router_bridge::planner::UsageReporting;
use serde_json_bytes::json;
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;

use super::DeferredNode;
use super::Depends;
use super::FlattenNode;
use super::OperationKind;
use super::PlanNode;
use super::Primary;
use super::QueryPlan;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::plugin;
use crate::plugin::test::MockSubgraph;
use crate::query_planner;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::SubgraphOperation;
use crate::query_planner::BridgeQueryPlanner;
use crate::request;
use crate::services::subgraph_service::MakeSubgraphService;
use crate::services::supergraph;
use crate::services::SubgraphResponse;
use crate::services::SubgraphServiceFactory;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Configuration;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;

macro_rules! test_query_plan {
    () => {
        include_str!("testdata/query_plan.json")
    };
}

macro_rules! test_schema {
    () => {
        include_str!("testdata/schema.graphql")
    };
}

#[test]
fn query_plan_from_json() {
    let query_plan: PlanNode = serde_json::from_str(test_query_plan!()).unwrap();
    insta::assert_debug_snapshot!(query_plan);
}

#[test]
fn service_usage() {
    assert_eq!(
        serde_json::from_str::<PlanNode>(test_query_plan!())
            .unwrap()
            .service_usage()
            .collect::<Vec<_>>(),
        vec!["product", "books", "product", "books", "product"]
    );
}

/// This test panics in the product subgraph. HOWEVER, this does not result in a panic in the
/// test, since the buffer() functionality in the tower stack "loses" the panic and we end up
/// with a closed service.
///
/// See: https://github.com/tower-rs/tower/issues/455
///
/// The query planner reports the failed subgraph fetch as an error with a reason of "service
/// closed", which is what this test expects.
#[tokio::test]
#[should_panic(expected = "this panic should be propagated to the test harness")]
async fn mock_subgraph_service_withf_panics_should_be_reported_as_service_closed() {
    let query_plan: QueryPlan = QueryPlan {
        root: serde_json::from_str(test_query_plan!()).unwrap(),
        formatted_query_plan: Default::default(),
        query: Arc::new(Query::empty()),
        usage_reporting: UsageReporting {
            stats_report_key: "this is a test report key".to_string(),
            referenced_fields_by_type: Default::default(),
        }
        .into(),
    };

    let mut mock_products_service = plugin::test::MockSubgraphService::new();
    mock_products_service.expect_call().times(1).withf(|_| {
        panic!("this panic should be propagated to the test harness");
    });
    mock_products_service.expect_clone().return_once(|| {
        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service.expect_call().times(1).withf(|_| {
            panic!("this panic should be propagated to the test harness");
        });
        mock_products_service
    });

    let (sender, _) = tokio::sync::mpsc::channel(10);
    let sf = Arc::new(SubgraphServiceFactory {
        services: Arc::new(HashMap::from([(
            "product".into(),
            Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
        )])),
        plugins: Default::default(),
    });

    let result = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &Arc::new(Schema::parse_test(test_schema!(), &Default::default()).unwrap()),
            sender,
            None,
            &None,
            None,
        )
        .await;
    assert_eq!(result.errors.len(), 1);
    let reason: String =
        serde_json_bytes::from_value(result.errors[0].extensions.get("reason").unwrap().clone())
            .unwrap();
    assert_eq!(reason, "service closed".to_string());
}

#[tokio::test]
async fn fetch_includes_operation_name() {
    let query_plan: QueryPlan = QueryPlan {
        root: serde_json::from_str(test_query_plan!()).unwrap(),
        formatted_query_plan: Default::default(),
        usage_reporting: UsageReporting {
            stats_report_key: "this is a test report key".to_string(),
            referenced_fields_by_type: Default::default(),
        }
        .into(),
        query: Arc::new(Query::empty()),
    };

    let succeeded: Arc<AtomicBool> = Default::default();
    let inner_succeeded = Arc::clone(&succeeded);

    let mut mock_products_service = plugin::test::MockSubgraphService::new();
    mock_products_service.expect_clone().return_once(|| {
        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service
            .expect_call()
            .times(1)
            .withf(move |request| {
                let matches = request.subgraph_request.body().operation_name
                    == Some("topProducts_product_0".into());
                inner_succeeded.store(matches, Ordering::SeqCst);
                matches
            })
            .returning(|_| Ok(SubgraphResponse::fake_builder().build()));
        mock_products_service
    });

    let (sender, _) = tokio::sync::mpsc::channel(10);

    let sf = Arc::new(SubgraphServiceFactory {
        services: Arc::new(HashMap::from([(
            "product".into(),
            Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
        )])),
        plugins: Default::default(),
    });

    let _response = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &Arc::new(Schema::parse_test(test_schema!(), &Default::default()).unwrap()),
            sender,
            None,
            &None,
            None,
        )
        .await;

    assert!(succeeded.load(Ordering::SeqCst), "incorrect operation name");
}

#[tokio::test]
async fn fetch_makes_post_requests() {
    let query_plan: QueryPlan = QueryPlan {
        root: serde_json::from_str(test_query_plan!()).unwrap(),
        formatted_query_plan: Default::default(),
        usage_reporting: UsageReporting {
            stats_report_key: "this is a test report key".to_string(),
            referenced_fields_by_type: Default::default(),
        }
        .into(),
        query: Arc::new(Query::empty()),
    };

    let succeeded: Arc<AtomicBool> = Default::default();
    let inner_succeeded = Arc::clone(&succeeded);

    let mut mock_products_service = plugin::test::MockSubgraphService::new();

    mock_products_service.expect_clone().return_once(|| {
        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service
            .expect_call()
            .times(1)
            .withf(move |request| {
                let matches = request.subgraph_request.method() == Method::POST;
                inner_succeeded.store(matches, Ordering::SeqCst);
                matches
            })
            .returning(|_| Ok(SubgraphResponse::fake_builder().build()));
        mock_products_service
    });

    let (sender, _) = tokio::sync::mpsc::channel(10);

    let sf = Arc::new(SubgraphServiceFactory {
        services: Arc::new(HashMap::from([(
            "product".into(),
            Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
        )])),
        plugins: Default::default(),
    });

    let _response = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &Arc::new(Schema::parse_test(test_schema!(), &Default::default()).unwrap()),
            sender,
            None,
            &None,
            None,
        )
        .await;

    assert!(
        succeeded.load(Ordering::SeqCst),
        "subgraph requests must be http post"
    );
}

#[tokio::test]
async fn defer() {
    // plan for { t { x ... @defer { y } }}
    let query_plan: QueryPlan = QueryPlan {
            formatted_query_plan: Default::default(),
            root: PlanNode::Defer {
                primary: Primary {
                    subselection: Some("{ t { x } }".to_string()),
                    node: Some(Box::new(PlanNode::Fetch(FetchNode {
                        service_name: "X".into(),
                        requires: vec![],
                        variable_usages: vec![],
                        operation: SubgraphOperation::from_string("{ t { id __typename x } }"),
                        operation_name: Some("t".into()),
                        operation_kind: OperationKind::Query,
                        id: Some("fetch1".into()),
                        input_rewrites: None,
                        output_rewrites: None,
                        schema_aware_hash: Default::default(),
                        authorization: Default::default(),
                    }))),
                },
                deferred: vec![DeferredNode {
                    depends: vec![Depends {
                        id: "fetch1".into(),
                    }],
                    label: None,
                    query_path: Path(vec![PathElement::Key("t".to_string(), None)]),
                    subselection: Some("{ y }".to_string()),
                    node: Some(Arc::new(PlanNode::Flatten(FlattenNode {
                        path: Path(vec![PathElement::Key("t".to_string(), None)]),
                        node: Box::new(PlanNode::Fetch(FetchNode {
                            service_name: "Y".into(),
                            requires: vec![query_planner::selection::Selection::InlineFragment(
                                query_planner::selection::InlineFragment {
                                    type_condition: Some(name!("T")),
                                    selections: vec![
                                        query_planner::selection::Selection::Field(
                                            query_planner::selection::Field {
                                                alias: None,
                                                name: name!("id"),
                                                selections: None,
                                            },
                                        ),
                                        query_planner::selection::Selection::Field(
                                            query_planner::selection::Field {
                                                alias: None,
                                                name: name!("__typename"),
                                                selections: None,
                                            },
                                        ),
                                    ],
                                },
                            )],
                            variable_usages: vec![],
                            operation: SubgraphOperation::from_string(
                                "query($representations:[_Any!]!){_entities(representations:$representations){...on T{y}}}"
                            ),
                            operation_name: None,
                            operation_kind: OperationKind::Query,
                            id: Some("fetch2".into()),
                            input_rewrites: None,
                            output_rewrites: None,
                            schema_aware_hash: Default::default(),
                            authorization: Default::default(),
                        })),
                    }))),
                }],
            },
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            }.into(),
            query: Arc::new(Query::empty()),
        };

    let mut mock_x_service = plugin::test::MockSubgraphService::new();
    mock_x_service.expect_clone().return_once(|| {
        let mut mock_x_service = plugin::test::MockSubgraphService::new();
        mock_x_service
            .expect_call()
            .times(1)
            .withf(move |_request| true)
            .returning(|_| {
                Ok(SubgraphResponse::fake_builder()
                    .data(serde_json::json! {{
                        "t": {"id": 1234,
                        "__typename": "T",
                         "x": "X"
                        }
                    }})
                    .build())
            });
        mock_x_service
    });

    let mut mock_y_service = plugin::test::MockSubgraphService::new();
    mock_y_service.expect_clone().return_once(|| {
        let mut mock_y_service = plugin::test::MockSubgraphService::new();
        mock_y_service
            .expect_call()
            .times(1)
            .withf(move |_request| true)
            .returning(|_| {
                Ok(SubgraphResponse::fake_builder()
                    .data(serde_json::json! {{
                        "_entities": [{"y": "Y", "__typename": "T"}]
                    }})
                    .build())
            });
        mock_y_service
    });

    let (sender, receiver) = tokio::sync::mpsc::channel(10);

    let schema = include_str!("testdata/defer_schema.graphql");
    let schema = Arc::new(Schema::parse_test(schema, &Default::default()).unwrap());
    let sf = Arc::new(SubgraphServiceFactory {
        services: Arc::new(HashMap::from([
            (
                "X".into(),
                Arc::new(mock_x_service) as Arc<dyn MakeSubgraphService>,
            ),
            (
                "Y".into(),
                Arc::new(mock_y_service) as Arc<dyn MakeSubgraphService>,
            ),
        ])),
        plugins: Default::default(),
    });

    let response = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &schema,
            sender,
            None,
            &None,
            None,
        )
        .await;

    // primary response
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        serde_json::json! {{"data":{"t":{"id":1234,"__typename":"T","x":"X"}}}}
    );

    let response = ReceiverStream::new(receiver).next().await.unwrap();

    // deferred response
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        // the primary response appears there because the deferred response gets data from it
        // unneeded parts are removed in response formatting
        serde_json::json! {{"data":{"t":{"y":"Y","__typename":"T","id":1234,"x":"X"}},"path":["t"]}}
    );
}

#[tokio::test]
async fn defer_if_condition() {
    let query = r#"
        query Me($shouldDefer: Boolean) {
            me {
              id
              ... @defer(if: $shouldDefer) {
                name
                username
              }
            }
          }"#;

    let schema = include_str!("testdata/defer_clause.graphql");
    // we need to use the planner here instead of Schema::parse_test because that one uses the router bridge's api_schema function
    // does not keep the defer directive definition
    let planner = BridgeQueryPlanner::new(schema.to_string(), Arc::new(Configuration::default()))
        .await
        .unwrap();
    let schema = planner.schema();

    let root: PlanNode =
        serde_json::from_str(include_str!("testdata/defer_clause_plan.json")).unwrap();

    let query_plan = QueryPlan {
        root,
        usage_reporting: UsageReporting {
            stats_report_key: "this is a test report key".to_string(),
            referenced_fields_by_type: Default::default(),
        }
        .into(),
        query: Arc::new(
            Query::parse(
                query,
                Some("Me"),
                &schema,
                &Configuration::fake_builder().build().unwrap(),
            )
            .unwrap(),
        ),
        formatted_query_plan: None,
    };

    let mocked_accounts = MockSubgraph::builder()
        // defer if true
        .with_json(
            serde_json::json! {{"query":"query Me__accounts__0{me{__typename id}}", "operationName":"Me__accounts__0"}},
            serde_json::json! {{"data": {"me": {"__typename": "User", "id": "1"}}}},
        )
        .with_json(
            serde_json::json! {{"query":"query Me__accounts__1($representations:[_Any!]!){_entities(representations:$representations){...on User{name username}}}", "operationName":"Me__accounts__1", "variables":{"representations":[{"__typename":"User","id":"1"}]}}},
            serde_json::json! {{"data": {"_entities": [{"name": "Ada Lovelace", "username": "@ada"}]}}},
        )
        // defer if false
        .with_json(serde_json::json! {{"query": "query Me__accounts__2{me{id name username}}", "operationName":"Me__accounts__2"}},
        serde_json::json! {{"data": {"me": {"id": "1", "name": "Ada Lovelace", "username": "@ada"}}}},
    )
        .build();

    let (sender, receiver) = tokio::sync::mpsc::channel(10);
    let mut receiver_stream = ReceiverStream::new(receiver);

    let service_factory = Arc::new(SubgraphServiceFactory {
        services: Arc::new(HashMap::from([(
            "accounts".into(),
            Arc::new(mocked_accounts) as Arc<dyn MakeSubgraphService>,
        )])),
        plugins: Default::default(),
    });
    let defer_primary_response = query_plan
        .execute(
            &Context::new(),
            &service_factory,
            &Arc::new(
                http::Request::builder()
                    .body(
                        request::Request::fake_builder()
                            .variables(json!({ "shouldDefer": true }).as_object().unwrap().clone())
                            .build(),
                    )
                    .unwrap(),
            ),
            &schema,
            sender,
            None,
            &None,
            None,
        )
        .await;

    // shouldDefer: true
    insta::assert_json_snapshot!(defer_primary_response);
    let deferred_response = receiver_stream.next().await.unwrap();
    insta::assert_json_snapshot!(deferred_response);
    assert!(receiver_stream.next().await.is_none());

    // shouldDefer: not provided, should default to true
    let (default_sender, default_receiver) = tokio::sync::mpsc::channel(10);
    let mut default_receiver_stream = ReceiverStream::new(default_receiver);
    let default_primary_response = query_plan
        .execute(
            &Context::new(),
            &service_factory,
            &Default::default(),
            &schema,
            default_sender,
            None,
            &None,
            None,
        )
        .await;

    assert_eq!(defer_primary_response, default_primary_response);
    assert_eq!(
        deferred_response,
        default_receiver_stream.next().await.unwrap()
    );
    assert!(default_receiver_stream.next().await.is_none());

    // shouldDefer: false, only 1 response
    let (sender, no_defer_receiver) = tokio::sync::mpsc::channel(10);
    let mut no_defer_receiver_stream = ReceiverStream::new(no_defer_receiver);
    let defer_disabled = query_plan
        .execute(
            &Context::new(),
            &service_factory,
            &Arc::new(
                http::Request::builder()
                    .body(
                        request::Request::fake_builder()
                            .variables(json!({ "shouldDefer": false }).as_object().unwrap().clone())
                            .build(),
                    )
                    .unwrap(),
            ),
            &schema,
            sender,
            None,
            &None,
            None,
        )
        .await;
    insta::assert_json_snapshot!(defer_disabled);
    assert!(no_defer_receiver_stream.next().await.is_none());
}

#[tokio::test]
async fn dependent_mutations() {
    let schema = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1"),
        @core(feature: "https://specs.apollo.dev/join/v0.1")
      {
        query: Query
        mutation: Mutation
      }

      directive @core(feature: String!) repeatable on SCHEMA
      directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
      directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
      directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
      directive @join__graph(name: String!, url: String!) on ENUM_VALUE
      scalar join__FieldSet

      enum join__Graph {
        A @join__graph(name: "A" url: "http://localhost:4001")
        B @join__graph(name: "B" url: "http://localhost:4004")
      }

      type Mutation {
          mutationA: Mutation @join__field(graph: A)
          mutationB: Boolean @join__field(graph: B)
      }

      type Query {
          query: Boolean @join__field(graph: A)
      }"#;

    let query_plan: QueryPlan = QueryPlan {
        // generated from:
        // mutation {
        //   mutationA {
        //     mutationB
        //   }
        // }
        formatted_query_plan: Default::default(),
        root: serde_json::from_str(
            r#"{
                "kind": "Sequence",
                "nodes": [
                    {
                        "kind": "Fetch",
                        "serviceName": "A",
                        "variableUsages": [],
                        "operation": "mutation{mutationA{__typename}}",
                        "operationKind": "mutation"
                    },
                    {
                        "kind": "Flatten",
                        "path": [
                            "mutationA"
                        ],
                        "node": {
                            "kind": "Fetch",
                            "serviceName": "B",
                            "variableUsages": [],
                            "operation": "mutation{...on Mutation{mutationB}}",
                            "operationKind": "mutation"
                        }
                    }
                ]
            }"#,
        )
        .unwrap(),
        usage_reporting: UsageReporting {
            stats_report_key: "this is a test report key".to_string(),
            referenced_fields_by_type: Default::default(),
        }
        .into(),
        query: Arc::new(Query::empty()),
    };

    let mut mock_a_service = plugin::test::MockSubgraphService::new();
    mock_a_service.expect_clone().returning(|| {
        let mut mock_a_service = plugin::test::MockSubgraphService::new();
        mock_a_service
            .expect_call()
            .times(1)
            .returning(|_| Ok(SubgraphResponse::fake_builder().build()));

        mock_a_service
    });

    // the first fetch returned null, so there should never be a call to B
    let mut mock_b_service = plugin::test::MockSubgraphService::new();
    mock_b_service.expect_call().never();

    let sf = Arc::new(SubgraphServiceFactory {
        services: Arc::new(HashMap::from([
            (
                "A".into(),
                Arc::new(mock_a_service) as Arc<dyn MakeSubgraphService>,
            ),
            (
                "B".into(),
                Arc::new(mock_b_service) as Arc<dyn MakeSubgraphService>,
            ),
        ])),
        plugins: Default::default(),
    });

    let (sender, _) = tokio::sync::mpsc::channel(10);
    let _response = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &Arc::new(Schema::parse_test(schema, &Default::default()).unwrap()),
            sender,
            None,
            &None,
            None,
        )
        .await;
}

#[tokio::test]
async fn alias_renaming() {
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
    
    interface I
      @join__type(graph: S1)
      @join__type(graph: S2)
    {
      id: String!
    }
    
    scalar join__FieldSet
    
    enum join__Graph {
      S1 @join__graph(name: "S1", url: "http://localhost/s1")
      S2 @join__graph(name: "S2", url: "http://localhost/s2")
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
      @join__type(graph: S1)
      @join__type(graph: S2)
    {
      testQuery(id: String!): I @join__field(graph: S1)
    }
    
    type T1 implements I
      @join__implements(graph: S1, interface: "I")
      @join__implements(graph: S2, interface: "I")
      @join__type(graph: S1, key: "id", resolvable: false)
      @join__type(graph: S2, key: "id")
    {
      id: String!
      foo: Test @join__field(graph: S2)
    }
    
    type T2 implements I
      @join__implements(graph: S1, interface: "I")
      @join__implements(graph: S2, interface: "I")
      @join__type(graph: S1, key: "id", resolvable: false)
      @join__type(graph: S2, key: "id")
    {
      id: String!
      bar: Test @join__field(graph: S2)
    }
    
    type Test
      @join__type(graph: S2)
    {
      field: String!
    }"#;

    let query = "query test($tId: String!) {
            testQuery(id: $tId) {
            ... on T1 {
            foo {
                field
            }
            }
            ... on T2 {
            foo: bar {
                field
            }
            }
        }
        }";

    let subgraphs = MockedSubgraphs([
        ("S1", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":
            "query test__S1__0($tId:String!){testQuery(id:$tId){__typename ...on T1{__typename id}...on T2{__typename id}}}",
            "operationName": "test__S1__0", "variables":{"tId":"1"}}},
            serde_json::json!{{"data": {
                "testQuery": {
                    "__typename": "T1",
                    "id": "T1",
                }
            } }}
        ).with_json(
            serde_json::json!{{"query":
            "query test__S1__0($tId:String!){testQuery(id:$tId){__typename ...on T1{__typename id}...on T2{__typename id}}}",
            "operationName": "test__S1__0", "variables":{"tId":"2"}}},
            serde_json::json!{{"data": {
                "testQuery": {
                    "__typename": "T2",
                    "id": "T2",
                }
            } }}
        ).build()),
        ("S2", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":
            "query test__S2__1($representations:[_Any!]!){_entities(representations:$representations){...on T1{foo{field}}...on T2{foo__alias_0:bar{field}}}}",
            "operationName": "test__S2__1", "variables":{"representations":[{
                "__typename": "T1",
                "id": "T1",
            }]}}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "foo": {
                        "field": "aaa"
                    }
                }]
            } }}
        ).with_json(
            serde_json::json!{{"query":
            "query test__S2__1($representations:[_Any!]!){_entities(representations:$representations){...on T1{foo{field}}...on T2{foo__alias_0:bar{field}}}}",
            "operationName": "test__S2__1", "variables":{"representations":[{
                "__typename": "T2",
                "id": "T2",
            }]}}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "foo__alias_0": {
                        "field": "bbb"
                    }
                }]
            } }}
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
        .context(Context::new())
        .query(query)
        .variables(
            serde_json_bytes::json! {{ "tId": "1"}}
                .as_object()
                .unwrap()
                .clone(),
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());

    let request = supergraph::Request::fake_builder()
        .context(Context::new())
        .query(query)
        .variables(
            serde_json_bytes::json! {{ "tId": "2"}}
                .as_object()
                .unwrap()
                .clone(),
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());
}

#[tokio::test]
async fn missing_fields_in_requires() {
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
  
  type Details
    @join__type(graph: SUB1)
    @join__type(graph: SUB2)
  {
    enabled: Boolean
  }
  
  scalar join__FieldSet
  
  enum join__Graph {
    SUB1 @join__graph(name: "sub1", url: "http://localhost:4002/test")
    SUB2 @join__graph(name: "sub2", url: "http://localhost:4002/test2")
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
    @join__type(graph: SUB1)
    @join__type(graph: SUB2)
  {
    stuff: Stuff @join__field(graph: SUB1)
  }
  
  type Stuff
    @join__type(graph: SUB1, key: "id")
    @join__type(graph: SUB2, key: "id", extension: true)
  {
    id: ID
    details: [Details] @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
    aDetailsIsEnabled: Boolean @join__field(graph: SUB2, requires: "details { enabled }")
  }"#;

    let query = "query {
        stuff {
          id
          aDetailsIsEnabled
        }
      }";

    let subgraphs = MockedSubgraphs([
        ("sub1", MockSubgraph::builder().with_json(
            serde_json::json!{{"query": "{stuff{__typename id details{enabled}}}",}},
            serde_json::json!{{"data": {
                "stuff": {
                  "__typename": "Stuff",
                  "id": "1",
                  "details": [{
                    "enabled": true
                  },
                  null,
                  {
                    "enabled": false
                  }]
                }
            } }}
        ).build()),
        ("sub2", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Stuff{aDetailsIsEnabled}}}",
                "variables":{"representations": [
                    {
                        "__typename": "Stuff",
                        "id": "1",
                        "details": [
                            {
                                "enabled": true
                            },
                            null,
                            {
                                "enabled": false
                            }
                        ]
                    }
                ]}}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "aDetailsIsEnabled": true
                }]
            } }}
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
        .context(Context::new())
        .query(query)
        .variables(
            serde_json_bytes::json! {{ "tId": "1"}}
                .as_object()
                .unwrap()
                .clone(),
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());
}

#[tokio::test]
async fn missing_typename_and_fragments_in_requires() {
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
    SUB1 @join__graph(name: "sub1", url: "http://localhost:4002/test")
    SUB2 @join__graph(name: "sub2", url: "http://localhost:4002/test2")
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
    @join__type(graph: SUB1)
    @join__type(graph: SUB2)
  {
    stuff: Stuff @join__field(graph: SUB1)
  }
  
  type Stuff
    @join__type(graph: SUB1, key: "id")
    @join__type(graph: SUB2, key: "id", extension: true)
  {
    id: ID
    thing: Thing
    isEnabled: Boolean @join__field(graph: SUB2, requires: "thing { ... on Thing { text } }")
  }
  
  type Thing
  @join__type(graph: SUB1, key: "id")
  @join__type(graph: SUB2, key: "id") {
    id: ID
    text: String @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
  }
  "#;

    let query = "query {
        stuff {
          id
          isEnabled
        }
      }";

    let subgraphs = MockedSubgraphs([
        ("sub1", MockSubgraph::builder().with_json(
            serde_json::json!{{"query": "{stuff{__typename id thing{__typename id text}}}",}},
            serde_json::json!{{"data": {
                "stuff": {
                  "__typename": "Stuff",
                  "id": "1",
                  "thing": {
                    "__typename": "Thing",
                    "id": "2",
                    "text": "aaa"
                  }
                }
            } }}
        ).build()),
        ("sub2", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Stuff{isEnabled}}}",
                "variables":{"representations": [
                    {
                        "__typename": "Stuff",
                        "id": "1",
                        "thing": {
                        "text": "aaa"
                        }
                    }
                ]}}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "isEnabled": true
                }]
            } }}
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
        .context(Context::new())
        .query(query)
        .variables(
            serde_json_bytes::json! {{ "tId": "1"}}
                .as_object()
                .unwrap()
                .clone(),
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());
}

#[tokio::test]
async fn missing_typename_and_fragments_in_requires2() {
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
    SUB1 @join__graph(name: "sub1", url: "http://localhost:4002/test")
    SUB2 @join__graph(name: "sub2", url: "http://localhost:4002/test2")
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
    @join__type(graph: SUB1)
    @join__type(graph: SUB2)
  {
    stuff: Stuff @join__field(graph: SUB1)
  }
  
  type Stuff
    @join__type(graph: SUB1, key: "id")
    @join__type(graph: SUB2, key: "id", extension: true)
  {
    id: ID
    thing: PossibleThing @join__field(graph: SUB1) @join__field(graph: SUB2, external: true) 
    isEnabled: Boolean @join__field(graph: SUB2, requires: "thing { ... on Thing1 { __typename text1 } ... on Thing2 { __typename text2 } }")
  }
  
  union PossibleThing @join__type(graph: SUB1) @join__type(graph: SUB2)
  @join__unionMember(graph: SUB1, member: "Thing1") @join__unionMember(graph: SUB1, member: "Thing2")
  @join__unionMember(graph: SUB2, member: "Thing1") @join__unionMember(graph: SUB2, member: "Thing2")
    = Thing1 | Thing2

  type Thing1
  @join__type(graph: SUB1, key: "id")
  @join__type(graph: SUB2, key: "id") {
    id: ID
    text1: String @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
  }

  type Thing2
  @join__type(graph: SUB1, key: "id")
  @join__type(graph: SUB2, key: "id") {
    id: ID
    text2: String @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
  }
  "#;

    let query = "query {
        stuff {
          id
          isEnabled
        }
      }";

    let subgraphs = MockedSubgraphs([
        ("sub1", MockSubgraph::builder().with_json(
            serde_json::json!{{"query": "{stuff{__typename id thing{__typename ...on Thing1{__typename text1}...on Thing2{__typename text2}}}}",}},
            serde_json::json!{{"data": {
                "stuff": {
                  "__typename": "Stuff",
                  "id": "1",
                  "thing": {
                    "__typename": "Thing1",
                    "text1": "aaa"
                  }
                }
            } }}
        ).build()),
        ("sub2", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Stuff{isEnabled}}}",
                "variables":{"representations": [
                    {
                        "__typename": "Stuff",
                        "id": "1",
                        "thing": {
                        "__typename": "Thing1",
                        "text1": "aaa"
                        }
                    }
                ]}}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "isEnabled": true
                }]
            } }}
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
        .context(Context::new())
        .query(query)
        .variables(
            serde_json_bytes::json! {{ "tId": "1"}}
                .as_object()
                .unwrap()
                .clone(),
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());
}

#[tokio::test]
async fn null_in_requires() {
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
    SUB1 @join__graph(name: "sub1", url: "http://localhost:4002/test")
    SUB2 @join__graph(name: "sub2", url: "http://localhost:4002/test2")
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
    @join__type(graph: SUB1)
    @join__type(graph: SUB2)
  {
    stuff: Stuff @join__field(graph: SUB1)
  }
  
  type Stuff
    @join__type(graph: SUB1, key: "id")
    @join__type(graph: SUB2, key: "id", extension: true)
  {
    id: ID
    thing: Thing
    isEnabled: Boolean @join__field(graph: SUB2, requires: "thing { a text }")
  }
  
  type Thing
  @join__type(graph: SUB1, key: "id")
  @join__type(graph: SUB2, key: "id") {
    id: ID
    a: String @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
    text: String @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
  }
  "#;

    let query = "query {
        stuff {
          id
          isEnabled
        }
      }";

    let subgraphs = MockedSubgraphs([
        ("sub1", MockSubgraph::builder().with_json(
            serde_json::json!{{"query": "{stuff{__typename id thing{__typename id a text}}}",}},
            serde_json::json!{{"data": {
                "stuff": {
                  "__typename": "Stuff",
                  "id": "1",
                  "thing": {
                    "__typename": "Thing",
                    "id": "2",
                    "a": "A",
                    "text": null
                  }
                }
            } }}
        ).build()),
        ("sub2", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Stuff{isEnabled}}}",
                "variables":{"representations": [
                    {
                        "__typename": "Stuff",
                        "id": "1",
                        "thing": {
                            "a": "A",
                            "text": null
                        }
                    }
                ]}}},
            serde_json::json!{{"data": {
                "_entities": [{
                    "isEnabled": true
                }]
            } }}
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
        .context(Context::new())
        .query(query)
        .variables(
            serde_json_bytes::json! {{ "tId": "1"}}
                .as_object()
                .unwrap()
                .clone(),
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());
}

const TYPENAME_PROPAGATION_SCHEMA: &str = r#"schema
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

type Author implements Node
@join__implements(graph: NODE_RELAY_SUBGRAPH, interface: "Node")
@join__type(graph: AUTHOR_SUBGRAPH, key: "authorId")
@join__type(graph: BOOK_SUBGRAPH, key: "authorId")
@join__type(graph: NODE_RELAY_SUBGRAPH, key: "authorId")
{
authorId: String!
fullName: String! @join__field(graph: AUTHOR_SUBGRAPH) @join__field(graph: BOOK_SUBGRAPH, external: true) @join__field(graph: NODE_RELAY_SUBGRAPH, external: true)
id: ID! @join__field(graph: NODE_RELAY_SUBGRAPH)
}

type Book implements Node
@join__implements(graph: NODE_RELAY_SUBGRAPH, interface: "Node")
@join__type(graph: BOOK_SUBGRAPH, key: "bookId author { fullName }")
@join__type(graph: NODE_RELAY_SUBGRAPH, key: "bookId author { fullName }")
{
bookId: String!
author: Author!
id: ID! @join__field(graph: NODE_RELAY_SUBGRAPH)
}

scalar join__FieldSet

enum join__Graph {
AUTHOR_SUBGRAPH @join__graph(name: "author_subgraph", url: "https://films.example.com")
BOOK_SUBGRAPH @join__graph(name: "book_subgraph", url: "https://films.example.com")
NODE_RELAY_SUBGRAPH @join__graph(name: "node_relay_subgraph", url: "https://films.example.com")
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

interface Node
@join__type(graph: NODE_RELAY_SUBGRAPH)
{
id: ID!
}

type Query
@join__type(graph: AUTHOR_SUBGRAPH)
@join__type(graph: BOOK_SUBGRAPH)
@join__type(graph: NODE_RELAY_SUBGRAPH)
{
b: Boolean @join__field(graph: AUTHOR_SUBGRAPH)
book: Book @join__field(graph: BOOK_SUBGRAPH)
node(id: ID): Node @join__field(graph: NODE_RELAY_SUBGRAPH)
}"#;

#[tokio::test]
async fn typename_propagation() {
    let subgraphs = MockedSubgraphs(
        [
            ("author_subgraph", MockSubgraph::builder().with_json(
                serde_json::json! {{
                    "query": "query QueryBook__author_subgraph__1($representations:[_Any!]!){_entities(representations:$representations){...on Author{fullName}}}",
                    "operationName": "QueryBook__author_subgraph__1",
                    "variables": {
                        "representations": [{
                            "__typename": "Author",
                            "authorId": "Author1"
                        }]
                    }
                }},
                serde_json::json! {{"data": {
                    "_entities": [{
                        "fullName": "Ada"
                    }]
                } }},
            ).build()),
            ("book_subgraph", MockSubgraph::builder().build()),
            ("node_relay_subgraph", MockSubgraph::builder().with_json(
                serde_json::json! {{
                    "query": "query Query__node_relay_subgraph__0{node{__typename ...on Book{id author{__typename}}}}",
                    "operationName": "Query__node_relay_subgraph__0"
                }},
                serde_json::json! {{"data": {
                    "node": {
                      "__typename": "Book",
                      "id": "1",
                      "author": {
                        "__typename": "Author"
                      }
                    }
                } }},
            ).build()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(TYPENAME_PROPAGATION_SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(Context::new())
        .query(
            "query Query {
            node {
              __typename
              ... on Book {
                id
                author {
                  __typename
                }
              }
            }
          }",
        )
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());
}

#[tokio::test]
async fn typename_propagation2() {
    let subgraphs = MockedSubgraphs(
        [
            ("author_subgraph", MockSubgraph::builder().build()),
            ("book_subgraph", MockSubgraph::builder().with_json(
                serde_json::json! {{
                    "query": "query QueryBook__book_subgraph__0{book{__typename bookId author{__typename authorId}}}",
                    "operationName": "QueryBook__book_subgraph__0"
                }},
                serde_json::json! {{"data": {
                    "book": {
                      "__typename": "Book",
                      "bookId": "book1",
                      "author": {
                        "__typename": null,
                        "authorId": "Author1"
                      }
                    }
                } }},
            ).build()),
            ("node_relay_subgraph", MockSubgraph::builder().with_json(
                serde_json::json! {{
                    "query": "query QueryBook__node_relay_subgraph__2($representations:[_Any!]!){_entities(representations:$representations){...on Book{__typename id}}}",
                    "operationName": "QueryBook__node_relay_subgraph__2",
                    "variables": {
                        "representations": [{
                            "__typename": "Book",
                            "bookId": "book1",
                            "author": null
                        }]
                    }
                }},
                serde_json::json! {{"data": {
                    "_entities": [{
                        "__typename": "Book",
                        "id": "1"
                    }]
                } }},
            ).with_json(
                serde_json::json! {{
                    "query": "query QueryBook__node_relay_subgraph__2($representations:[_Any!]!){_entities(representations:$representations){...on Book{__typename id}}}",
                    "operationName": "QueryBook__node_relay_subgraph__2",
                    "variables": {
                        "representations": [{
                            "__typename": "Book",
                            "bookId": "book1",
                            "author": {
                                "fullName": null
                            }
                        }]
                    }
                }},
                serde_json::json! {{"data": {
                    "_entities": [{
                        "__typename": "Book",
                        "id": "1"
                    }]
                } }},
            ).build()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(TYPENAME_PROPAGATION_SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let query = "query QueryBook {
        book {
          __typename
          ... on Book {
            id
            author {
              __typename
            }
          }
        }
      }";

    let request = supergraph::Request::fake_builder()
        .context(Context::new())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());
}

#[tokio::test]
async fn typename_propagation3() {
    let subgraphs = MockedSubgraphs(
        [
            ("author_subgraph", MockSubgraph::builder().with_json(
                serde_json::json! {{
                    "query": "query QueryBook2__author_subgraph__1($representations:[_Any!]!){_entities(representations:$representations){...on Author{fullName}}}",
                    "operationName": "QueryBook2__author_subgraph__1",
                    "variables": {
                        "representations": [{
                            "__typename": "Author",
                            "authorId": "Author1"
                        }]
                    }
                }},
                serde_json::json! {{"data": {
                    "_entities": [{
                        "fullName": "Ada"
                    }]
                } }},
            ).build()),
            ("book_subgraph", MockSubgraph::builder().with_json(
                serde_json::json! {{
                    "query": "query QueryBook2__book_subgraph__0{book{__typename bookId author{__typename authorId}}}",
                    "operationName": "QueryBook2__book_subgraph__0"
                }},
                serde_json::json! {{"data": {
                    "book": {
                      "__typename": "Book",
                      "bookId": "book1",
                      "author": {
                        "__typename": "Author",
                        "authorId": "Author1"
                      }
                    }
                } }},
            ).build()),
            ("node_relay_subgraph", MockSubgraph::builder().with_json(
                serde_json::json! {{
                    "query": "query QueryBook2__node_relay_subgraph__2($representations:[_Any!]!){_entities(representations:$representations){...on Book{__typename id author{id}}}}",
                    "operationName": "QueryBook2__node_relay_subgraph__2",
                    "variables": {
                        "representations": [{
                            "__typename": "Book",
                            "bookId": "book1",
                            "author": {
                                "fullName": "Ada"
                            }
                        }]
                    }
                }},
                serde_json::json! {{"data": {
                    "_entities": [{
                        "__typename": "Book",
                        "id": "1",
                        "author": {
                            "id": "2"
                        }
                    }]
                } }},
            ).build()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
        .unwrap()
        .schema(TYPENAME_PROPAGATION_SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let query = "query QueryBook2 {
        book {
          __typename
          ... on Book {
            id
            author {
              id
            }
          }
        }
      }";

    let request = supergraph::Request::fake_builder()
        .context(Context::new())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(serde_json::to_value(&response).unwrap());
}
