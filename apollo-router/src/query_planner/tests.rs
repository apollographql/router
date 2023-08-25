use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures::StreamExt;
use http::Method;
use router_bridge::planner::UsageReporting;
use serde_json_bytes::json;

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
use crate::request;
use crate::services::subgraph_service::MakeSubgraphService;
use crate::services::SubgraphResponse;
use crate::services::SubgraphServiceFactory;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Configuration;
use crate::Context;

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
        },
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

    let (sender, _) = futures::channel::mpsc::channel(10);
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
        },
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

    let (sender, _) = futures::channel::mpsc::channel(10);

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
        },
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

    let (sender, _) = futures::channel::mpsc::channel(10);

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
                    path: None,
                    subselection: Some("{ t { x } }".to_string()),
                    node: Some(Box::new(PlanNode::Fetch(FetchNode {
                        service_name: "X".to_string(),
                        requires: vec![],
                        variable_usages: vec![],
                        operation: "{ t { id __typename x } }".to_string(),
                        operation_name: Some("t".to_string()),
                        operation_kind: OperationKind::Query,
                        id: Some("fetch1".to_string()),
                        input_rewrites: None,
                        output_rewrites: None,
                    }))),
                },
                deferred: vec![DeferredNode {
                    depends: vec![Depends {
                        id: "fetch1".to_string(),
                        defer_label: None,
                    }],
                    label: None,
                    query_path: Path(vec![PathElement::Key("t".to_string())]), 
                    subselection: Some("{ y }".to_string()),
                    node: Some(Arc::new(PlanNode::Flatten(FlattenNode {
                        path: Path(vec![PathElement::Key("t".to_string())]),
                        node: Box::new(PlanNode::Fetch(FetchNode {
                            service_name: "Y".to_string(),
                            requires: vec![query_planner::selection::Selection::InlineFragment(
                                query_planner::selection::InlineFragment {
                                    type_condition: Some("T".into()),
                                    selections: vec![
                                        query_planner::selection::Selection::Field(
                                            query_planner::selection::Field {
                                                alias: None,
                                                name: "id".into(),
                                                selections: None,
                                            },
                                        ),
                                        query_planner::selection::Selection::Field(
                                            query_planner::selection::Field {
                                                alias: None,
                                                name: "__typename".into(),
                                                selections: None,
                                            },
                                        ),
                                    ],
                                },
                            )],
                            variable_usages: vec![],
                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on T{y}}}".to_string(),
                            operation_name: None,
                            operation_kind: OperationKind::Query,
                            id: Some("fetch2".to_string()),
                            input_rewrites: None,
                            output_rewrites: None,
                        })),
                    }))),
                }],
            },
            usage_reporting: UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            },
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

    let (sender, mut receiver) = futures::channel::mpsc::channel(10);

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

    let response = receiver.next().await.unwrap();

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
    let schema = Arc::new(Schema::parse_test(schema, &Default::default()).unwrap());

    let root: PlanNode =
        serde_json::from_str(include_str!("testdata/defer_clause_plan.json")).unwrap();

    let query_plan = QueryPlan {
        root,
        usage_reporting: UsageReporting {
            stats_report_key: "this is a test report key".to_string(),
            referenced_fields_by_type: Default::default(),
        },
        query: Arc::new(
            Query::parse(
                query,
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

    let (sender, mut receiver) = futures::channel::mpsc::channel(10);

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
    let deferred_response = receiver.next().await.unwrap();
    insta::assert_json_snapshot!(deferred_response);
    assert!(receiver.next().await.is_none());

    // shouldDefer: not provided, should default to true
    let (default_sender, mut default_receiver) = futures::channel::mpsc::channel(10);
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
    assert_eq!(deferred_response, default_receiver.next().await.unwrap());
    assert!(default_receiver.next().await.is_none());

    // shouldDefer: false, only 1 response
    let (sender, mut no_defer_receiver) = futures::channel::mpsc::channel(10);
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
    assert!(no_defer_receiver.next().await.is_none());
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
        },
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

    let (sender, _) = futures::channel::mpsc::channel(10);
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
