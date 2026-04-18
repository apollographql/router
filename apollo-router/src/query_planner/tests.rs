use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use apollo_compiler::name;
use apollo_federation::query_plan::requires_selection;
use apollo_federation::query_plan::serializable_document::SerializableDocument;
use futures::StreamExt;
use http::Method;
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
use crate::Configuration;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;
use crate::apollo_studio_interop::UsageReporting;
use crate::configuration::HoistOrphanErrors;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::plugin;
use crate::plugin::test::MockSubgraph;
use crate::query_planner;
use crate::query_planner::fetch::FetchNode;
use crate::services::SubgraphResponse;
use crate::services::SubgraphServiceFactory;
use crate::services::connector_service::ConnectorServiceFactory;
use crate::services::fetch_service::FetchServiceFactory;
use crate::services::subgraph_service::MakeSubgraphService;
use crate::services::supergraph;
use crate::spec::Query;
use crate::spec::Schema;

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

fn subgraph_service_factory(
    graphs: Vec<(String, Arc<dyn MakeSubgraphService>)>,
) -> SubgraphServiceFactory {
    SubgraphServiceFactory::new(
        graphs,
        Default::default(),
        // Required for subscriptions: we are not testing that here
        Default::default(),
        None,
    )
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
async fn mock_subgraph_service_with_panics_should_be_reported_as_service_closed() {
    let query_plan: QueryPlan = QueryPlan {
        root: serde_json::from_str(test_query_plan!()).unwrap(),
        formatted_query_plan: Default::default(),
        query: Arc::new(Query::empty_for_tests()),
        query_metrics: Default::default(),
        usage_reporting: UsageReporting::Error("this is a test report key".to_string()).into(),
        estimated_size: Default::default(),
    };

    let mut mock_products_service = plugin::test::MockSubgraphService::new();
    // This clone happens in the `MakeSubgraphService` impl for MockSubgraphService.
    mock_products_service.expect_clone().return_once(|| {
        let mut mock_products_service = plugin::test::MockSubgraphService::new();
        mock_products_service.expect_call().times(1).withf(|_| {
            panic!("this panic should be propagated to the test harness");
        });
        mock_products_service
    });

    let (sender, _) = tokio::sync::mpsc::channel(10);

    let schema = Arc::new(Schema::parse(test_schema!(), &Default::default()).unwrap());
    let ssf = subgraph_service_factory(vec![(
        "product".into(),
        Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
    )]);
    let sf = Arc::new(FetchServiceFactory::new(
        schema.clone(),
        Default::default(),
        Arc::new(ssf),
        None,
        Arc::new(ConnectorServiceFactory::empty(schema.clone())),
        Arc::new(SubgraphConfiguration::<HoistOrphanErrors>::default()),
    ));

    let result = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &schema,
            &Default::default(),
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
    assert_eq!(reason, "buffer's worker closed unexpectedly".to_string());
}

#[tokio::test]
async fn fetch_includes_operation_name() {
    let query_plan: QueryPlan = QueryPlan {
        root: serde_json::from_str(test_query_plan!()).unwrap(),
        formatted_query_plan: Default::default(),
        usage_reporting: UsageReporting::Error("this is a test report key".to_string()).into(),
        query: Arc::new(Query::empty_for_tests()),
        query_metrics: Default::default(),
        estimated_size: Default::default(),
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
            .expect_clone()
            .returning(plugin::test::MockSubgraphService::new);
        mock_products_service
    });

    let (sender, _) = tokio::sync::mpsc::channel(10);

    let schema = Arc::new(Schema::parse(test_schema!(), &Default::default()).unwrap());
    let ssf = subgraph_service_factory(vec![(
        "product".into(),
        Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
    )]);
    let sf = Arc::new(FetchServiceFactory::new(
        schema.clone(),
        Default::default(),
        Arc::new(ssf),
        None,
        Arc::new(ConnectorServiceFactory::empty(schema.clone())),
        Arc::new(SubgraphConfiguration::<HoistOrphanErrors>::default()),
    ));

    let _response = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &schema,
            &Default::default(),
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
        usage_reporting: UsageReporting::Error("this is a test report key".to_string()).into(),
        query: Arc::new(Query::empty_for_tests()),
        query_metrics: Default::default(),
        estimated_size: Default::default(),
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
            .expect_clone()
            .returning(plugin::test::MockSubgraphService::new);
        mock_products_service
    });

    let (sender, _) = tokio::sync::mpsc::channel(10);

    let schema = Arc::new(Schema::parse(test_schema!(), &Default::default()).unwrap());
    let ssf = subgraph_service_factory(vec![(
        "product".into(),
        Arc::new(mock_products_service) as Arc<dyn MakeSubgraphService>,
    )]);
    let sf = Arc::new(FetchServiceFactory::new(
        schema.clone(),
        Default::default(),
        Arc::new(ssf),
        None,
        Arc::new(ConnectorServiceFactory::empty(schema.clone())),
        Arc::new(SubgraphConfiguration::<HoistOrphanErrors>::default()),
    ));

    let _response = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &schema,
            &Default::default(),
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
                        operation: SerializableDocument::from_string("{ t { id __typename x } }"),
                        operation_name: Some("t".into()),
                        operation_kind: OperationKind::Query,
                        id: Some("fetch1".into()),
                        input_rewrites: None,
                        output_rewrites: None,
                        context_rewrites: None,
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
                            requires: vec![requires_selection::Selection::InlineFragment(
                                requires_selection::InlineFragment {
                                    type_condition: Some(name!("T")),
                                    selections: vec![
                                        requires_selection::Selection::Field(
                                            requires_selection::Field {
                                                alias: None,
                                                name: name!("id"),
                                                selections: Vec::new(),
                                            },
                                        ),
                                        requires_selection::Selection::Field(
                                            requires_selection::Field {
                                                alias: None,
                                                name: name!("__typename"),
                                                selections: Vec::new(),
                                            },
                                        ),
                                    ],
                                },
                            )],
                            variable_usages: vec![],
                            operation: SerializableDocument::from_string(
                                "query($representations:[_Any!]!){_entities(representations:$representations){...on T{y}}}"
                            ),
                            operation_name: None,
                            operation_kind: OperationKind::Query,
                            id: Some("fetch2".into()),
                            input_rewrites: None,
                            output_rewrites: None,
                            context_rewrites: None,
                            schema_aware_hash: Default::default(),
                            authorization: Default::default(),
                        })),
                    }))),
                }],
            }.into(),
            usage_reporting: UsageReporting::Error("this is a test report key".to_string()).into(),
            query: Arc::new(Query::empty_for_tests()),
            query_metrics: Default::default(),
            estimated_size: Default::default(),
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
            .expect_clone()
            .returning(plugin::test::MockSubgraphService::new);
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
            .expect_clone()
            .returning(plugin::test::MockSubgraphService::new);
        mock_y_service
    });

    let (sender, receiver) = tokio::sync::mpsc::channel(10);

    let schema = include_str!("testdata/defer_schema.graphql");
    let schema = Arc::new(Schema::parse(schema, &Default::default()).unwrap());
    let ssf = subgraph_service_factory(vec![
        (
            "X".into(),
            Arc::new(mock_x_service) as Arc<dyn MakeSubgraphService>,
        ),
        (
            "Y".into(),
            Arc::new(mock_y_service) as Arc<dyn MakeSubgraphService>,
        ),
    ]);
    let sf = Arc::new(FetchServiceFactory::new(
        schema.clone(),
        Default::default(),
        Arc::new(ssf),
        None,
        Arc::new(ConnectorServiceFactory::empty(schema.clone())),
        Arc::new(SubgraphConfiguration::<HoistOrphanErrors>::default()),
    ));

    let response = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &schema,
            &Default::default(),
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

    let schema = Arc::new(
        Schema::parse(
            include_str!("testdata/defer_clause.graphql"),
            &Configuration::default(),
        )
        .unwrap(),
    );

    let root: Arc<PlanNode> =
        serde_json::from_str(include_str!("testdata/defer_clause_plan.json")).unwrap();

    let query_plan = QueryPlan {
        root,
        usage_reporting: UsageReporting::Error("this is a test report key".to_string()).into(),
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
        query_metrics: Default::default(),
        estimated_size: Default::default(),
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

    let ssf = subgraph_service_factory(vec![(
        "accounts".into(),
        Arc::new(mocked_accounts) as Arc<dyn MakeSubgraphService>,
    )]);
    let service_factory = Arc::new(FetchServiceFactory::new(
        schema.clone(),
        Default::default(),
        Arc::new(ssf),
        None,
        Arc::new(ConnectorServiceFactory::empty(schema.clone())),
        Arc::new(SubgraphConfiguration::<HoistOrphanErrors>::default()),
    ));

    let defer_primary_response = query_plan
        .execute(
            &Context::new(),
            &service_factory,
            &Arc::new(
                http::Request::builder()
                    .body(
                        graphql::Request::fake_builder()
                            .variables(json!({ "shouldDefer": true }).as_object().unwrap().clone())
                            .build(),
                    )
                    .unwrap(),
            ),
            &schema,
            &Default::default(),
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
            &Default::default(),
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
                        graphql::Request::fake_builder()
                            .variables(json!({ "shouldDefer": false }).as_object().unwrap().clone())
                            .build(),
                    )
                    .unwrap(),
            ),
            &schema,
            &Default::default(),
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
    let schema = include_str!("../testdata/a_b_supergraph.graphql");

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
        usage_reporting: UsageReporting::Error("this is a test report key".to_string()).into(),
        query: Arc::new(Query::empty_for_tests()),
        query_metrics: Default::default(),
        estimated_size: Default::default(),
    };

    let mut mock_a_service = plugin::test::MockSubgraphService::new();
    mock_a_service.expect_clone().returning(|| {
        let mut mock_a_service = plugin::test::MockSubgraphService::new();
        mock_a_service
            .expect_call()
            .times(1)
            .returning(|_| Ok(SubgraphResponse::fake_builder().build()));
        mock_a_service
            .expect_clone()
            .returning(plugin::test::MockSubgraphService::new);

        mock_a_service
    });

    // the first fetch returned null, so there should never be a call to B
    let mut mock_b_service = plugin::test::MockSubgraphService::new();
    mock_b_service
        .expect_clone()
        .returning(plugin::test::MockSubgraphService::new);
    mock_b_service.expect_call().never();

    let schema = Arc::new(Schema::parse(schema, &Default::default()).unwrap());
    let ssf = subgraph_service_factory(vec![
        (
            "A".into(),
            Arc::new(mock_a_service) as Arc<dyn MakeSubgraphService>,
        ),
        (
            "B".into(),
            Arc::new(mock_b_service) as Arc<dyn MakeSubgraphService>,
        ),
    ]);
    let sf = Arc::new(FetchServiceFactory::new(
        schema.clone(),
        Default::default(),
        Arc::new(ssf),
        None,
        Arc::new(ConnectorServiceFactory::empty(schema.clone())),
        Arc::new(SubgraphConfiguration::<HoistOrphanErrors>::default()),
    ));

    let (sender, _) = tokio::sync::mpsc::channel(10);
    let _response = query_plan
        .execute(
            &Context::new(),
            &sf,
            &Default::default(),
            &schema,
            &Default::default(),
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
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
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
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
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
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
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
// Tests that when a subgraph returns __typename: null for a nested object,
// the entity representation cannot be built (the non-nullable `fullName`
// in the composite key `bookId author { fullName }` is unresolvable),
// so the downstream entity fetch is properly skipped with errors.
async fn typename_propagation2() {
    let subgraphs = MockedSubgraphs(
        [
            ("author_subgraph", MockSubgraph::builder().build()),
            ("book_subgraph", MockSubgraph::builder().with_json(
                serde_json::json! {{
                    "query": "query QueryBook__book_subgraph__0{book{__typename bookId author{__typename authorId}}}",
                    "operationName": "QueryBook__book_subgraph__0"
                }},
                // author.__typename is null — the Author entity cannot be resolved,
                // so fullName (needed for the Book key) is unresolvable.
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
            // node_relay_subgraph is never called because the Book entity
            // representation cannot be built.
            ("node_relay_subgraph", MockSubgraph::builder().build()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
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
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
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

#[test]
fn broken_plan_does_not_panic() {
    let operation = "{ invalid }";
    let subgraph_schema = "type Query { field: Int }";
    let mut plan = QueryPlan {
        root: PlanNode::Fetch(FetchNode {
            service_name: "X".into(),
            requires: vec![],
            variable_usages: vec![],
            operation: SerializableDocument::from_string(operation),
            operation_name: Some("t".into()),
            operation_kind: OperationKind::Query,
            id: Some("fetch1".into()),
            input_rewrites: None,
            output_rewrites: None,
            context_rewrites: None,
            schema_aware_hash: Default::default(),
            authorization: Default::default(),
        })
        .into(),
        formatted_query_plan: Default::default(),
        usage_reporting: UsageReporting::Error("this is a test report key".to_string()).into(),
        query: Arc::new(Query::empty_for_tests()),
        query_metrics: Default::default(),
        estimated_size: Default::default(),
    };
    let subgraph_schema = apollo_compiler::Schema::parse_and_validate(subgraph_schema, "").unwrap();
    let mut subgraph_schemas = HashMap::default();
    subgraph_schemas.insert(
        "X".to_owned(),
        query_planner::fetch::SubgraphSchema::new(subgraph_schema),
    );
    // Run the plan initialization code to make sure it doesn't panic.
    let result =
        Arc::make_mut(&mut plan.root).init_parsed_operations_and_hash_subqueries(&subgraph_schemas);
    assert_eq!(
        result.unwrap_err().to_string(),
        r#"[1:3] Cannot query field "invalid" on type "Query"."#
    );
}

// When a non-nullable field in a @requires selection is missing from the upstream subgraph
// response (e.g. stripped by a coprocessor), the entire downstream entity fetch shouldn't be
// silently skipped with no error. We ensure that errors are propagated to the client response
// explaining why the entity fields were nullified.
#[tokio::test]
async fn missing_nonnull_field_in_requires_returns_error() {
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
    SECURITY
    EXECUTION
  }

  type Query
    @join__type(graph: SUB1)
    @join__type(graph: SUB2)
  {
    entity: Entity @join__field(graph: SUB1)
  }

  type Entity
    @join__type(graph: SUB1, key: "id")
    @join__type(graph: SUB2, key: "id", extension: true)
  {
    id: ID!
    name: String @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
    code: String! @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
    computed: String @join__field(graph: SUB2, requires: "code")
    nickname: String @join__field(graph: SUB2, requires: "name")
  }"#;

    let query = "query {
        entity {
          id
          computed
          nickname
        }
      }";

    let subgraphs = MockedSubgraphs(
        [
            (
                "sub1",
                MockSubgraph::builder()
                    .with_json(
                        // Router queries sub1 for entity fields including code and name (needed for @requires)
                        serde_json::json! {{"query": "{entity{__typename id name code}}"}},
                        // Sub1 returns WITHOUT code (simulating coprocessor stripping the field from the request,
                        // so the subgraph never received it and doesn't return it)
                        serde_json::json! {{"data": {
                            "entity": {
                              "__typename": "Entity",
                              "id": "1",
                              "name": "Alice"
                            }
                        } }},
                    )
                    .build(),
            ),
            ("sub2", MockSubgraph::builder().build()),
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
        .context(Context::new())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    let value = serde_json::to_value(&response).unwrap();

    let data = &value["data"]["entity"];
    let errors = value["errors"]
        .as_array()
        .expect("errors should be present");

    // Both fields are null because the entity was nullified (non-nullable @requires field missing).
    // Per GraphQL spec, null bubbles up from the missing non-nullable field to the entity.
    assert_eq!(data["computed"], serde_json::Value::Null);
    assert_eq!(data["nickname"], serde_json::Value::Null);

    // The response now includes errors for each unfetched field.
    // Previously (bug), errors was empty — the fetch was silently skipped.
    assert_eq!(errors.len(), 2, "should have one error per unfetched field");

    let error_paths: Vec<&serde_json::Value> = errors.iter().map(|e| &e["path"]).collect();
    assert!(
        error_paths.contains(&&serde_json::json!(["entity", "computed"])),
        "should have error for entity.computed, got: {:?}",
        error_paths
    );
    assert!(
        error_paths.contains(&&serde_json::json!(["entity", "nickname"])),
        "should have error for entity.nickname, got: {:?}",
        error_paths
    );

    // Error messages are abstract — they don't expose internal @requires details.
    for err in errors {
        assert_eq!(
            err["extensions"]["code"].as_str(),
            Some("UNSATISFIED_FETCH_CONDITION"),
        );
    }
}

// Regression test: when a list of entities is fetched and only some entities have missing
// non-nullable @requires fields, the router should still fetch the successful entities
// and generate errors only for the failed ones.
#[tokio::test]
async fn batched_entity_partial_requires_failure() {
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
    SECURITY
    EXECUTION
  }

  type Query
    @join__type(graph: SUB1)
    @join__type(graph: SUB2)
  {
    entities: [Entity] @join__field(graph: SUB1)
  }

  type Entity
    @join__type(graph: SUB1, key: "id")
    @join__type(graph: SUB2, key: "id", extension: true)
  {
    id: ID!
    code: String! @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
    computed: String @join__field(graph: SUB2, requires: "code")
  }"#;

    let query = "query {
        entities {
          id
          computed
        }
      }";

    let subgraphs = MockedSubgraphs(
        [
            (
                "sub1",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{"query": "{entities{__typename id code}}"}},
                        // Entity 0 and 2 have code, entity 1 and 3 do NOT (missing required field)
                        serde_json::json! {{"data": {
                            "entities": [
                              {"__typename": "Entity", "id": "1", "code": "ABC"},
                              {"__typename": "Entity", "id": "2"},
                              {"__typename": "Entity", "id": "3", "code": "XYZ"},
                              {"__typename": "Entity", "id": "4"}
                            ]
                        } }},
                    )
                    .build(),
            ),
            (
                "sub2",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{
                            "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Entity{computed}}}",
                            "variables": {
                                "representations": [
                                    {"__typename": "Entity", "id": "1", "code": "ABC"},
                                    {"__typename": "Entity", "id": "3", "code": "XYZ"}
                                ]
                            }
                        }},
                        serde_json::json! {{"data": {
                            "_entities": [
                                {"computed": "computed-1"},
                                {"computed": "computed-3"}
                            ]
                        } }},
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
        .context(Context::new())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    let value = serde_json::to_value(&response).unwrap();

    let entities = value["data"]["entities"]
        .as_array()
        .expect("entities should be an array");

    // Entities 0 and 2 succeeded — they have computed values
    assert_eq!(entities[0]["computed"], serde_json::json!("computed-1"));
    assert_eq!(entities[2]["computed"], serde_json::json!("computed-3"));

    // Entities 1 and 3 failed — computed is null because code was missing
    assert_eq!(entities[1]["computed"], serde_json::Value::Null);
    assert_eq!(entities[3]["computed"], serde_json::Value::Null);

    let errors = value["errors"]
        .as_array()
        .expect("errors should be present");

    // Should have errors for the 2 failed entities
    assert_eq!(errors.len(), 2, "should have one error per failed entity");

    let error_paths: Vec<&serde_json::Value> = errors.iter().map(|e| &e["path"]).collect();
    assert!(
        error_paths.contains(&&serde_json::json!(["entities", 1, "computed"])),
        "should have error for entities[1].computed, got: {:?}",
        error_paths
    );
    assert!(
        error_paths.contains(&&serde_json::json!(["entities", 3, "computed"])),
        "should have error for entities[3].computed, got: {:?}",
        error_paths
    );

    for err in errors {
        assert_eq!(
            err["extensions"]["code"].as_str(),
            Some("UNSATISFIED_FETCH_CONDITION"),
        );
    }
}

// Regression test: when two @requires fetches execute in parallel at the same
// entity path, skipping one must not produce errors for the sibling fetch's
// fields. Before the fix, `errors_for_skipped_fetch` reported every input-query
// field absent from `parent_value` — which swept in fields the other parallel
// fetch was about to provide.
#[tokio::test]
async fn parallel_sibling_skipped_fetch_does_not_over_report() {
    run_parallel_sibling_skipped_fetch(false).await;
}

// Same scenario, but with `generate_query_fragments: true`. The skipped fetch's
// operation may contain named fragment spreads; verifying the fix still intersects
// correctly against the fetch's response shape when fragments are present.
#[tokio::test]
async fn parallel_sibling_skipped_fetch_does_not_over_report_generate_fragments() {
    run_parallel_sibling_skipped_fetch(true).await;
}

async fn run_parallel_sibling_skipped_fetch(generate_query_fragments: bool) {
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
    SUB1 @join__graph(name: "sub1", url: "http://localhost:4002/sub1")
    SUB2 @join__graph(name: "sub2", url: "http://localhost:4002/sub2")
    SUB3 @join__graph(name: "sub3", url: "http://localhost:4002/sub3")
  }

  scalar link__Import

  enum link__Purpose {
    SECURITY
    EXECUTION
  }

  type Query
    @join__type(graph: SUB1)
    @join__type(graph: SUB2)
    @join__type(graph: SUB3)
  {
    entity: Entity @join__field(graph: SUB1)
  }

  type Entity
    @join__type(graph: SUB1, key: "id")
    @join__type(graph: SUB2, key: "id", extension: true)
    @join__type(graph: SUB3, key: "id", extension: true)
  {
    id: ID!
    code1: String! @join__field(graph: SUB1) @join__field(graph: SUB2, external: true)
    code2: String! @join__field(graph: SUB1) @join__field(graph: SUB3, external: true)
    a: String @join__field(graph: SUB1)
    b: String @join__field(graph: SUB2, requires: "code1")
    c: String @join__field(graph: SUB3, requires: "code2")
  }"#;

    let query = "query {
        entity {
          a
          b
          c
        }
      }";

    let subgraphs = MockedSubgraphs(
        [
            (
                "sub1",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{"query": "{entity{__typename id a code1 code2}}"}},
                        // code1 is missing — sub2 (which requires code1) will be skipped.
                        // code2 is present — sub3 (which requires code2) will succeed.
                        serde_json::json! {{"data": {
                            "entity": {
                                "__typename": "Entity",
                                "id": "1",
                                "a": "a-value",
                                "code2": "c2"
                            }
                        } }},
                    )
                    .build(),
            ),
            ("sub2", MockSubgraph::builder().build()),
            (
                "sub3",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{
                            "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Entity{c}}}",
                            "variables": {
                                "representations": [
                                    {"__typename": "Entity", "id": "1", "code2": "c2"}
                                ]
                            }
                        }},
                        serde_json::json! {{"data": {
                            "_entities": [
                                {"c": "c-value"}
                            ]
                        } }},
                    )
                    .build(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
            "supergraph": {
                "generate_query_fragments": generate_query_fragments,
            }
        }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(Context::new())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    let value = serde_json::to_value(&response).unwrap();

    let entity = &value["data"]["entity"];
    // sub1 and sub3 succeeded — `a` and `c` should be populated, `b` is null.
    assert_eq!(entity["a"], serde_json::json!("a-value"));
    assert_eq!(entity["c"], serde_json::json!("c-value"));
    assert_eq!(entity["b"], serde_json::Value::Null);

    let errors = value["errors"]
        .as_array()
        .expect("errors should be present");

    // Only `entity.b` should be reported — NOT `entity.c`, even though `c`
    // was absent from parent_value at the time sub2 was skipped.
    assert_eq!(
        errors.len(),
        1,
        "should only report the skipped sibling's field, got: {:?}",
        errors
    );
    assert_eq!(errors[0]["path"], serde_json::json!(["entity", "b"]));
    assert_eq!(
        errors[0]["extensions"]["code"].as_str(),
        Some("UNSATISFIED_FETCH_CONDITION"),
    );
}

// Regression test: when two subgraph fetches share a composite field via
// @shareable, and one is skipped by a missing @requires input, the sibling
// fetch still provides its subfields. Error reporting should cover the fields
// that only the skipped fetch would have supplied, not the shared parent.
//
// Scenario:
//   query { me { profile { firstName address } } }
//   subA: me -> User { id, fullName }   (fullName missing from response)
//   subB: User.profile @requires(fullName) -> { firstName }   (skipped)
//   subC: User.profile -> { address }                         (succeeds)
#[tokio::test]
async fn skipped_fetch_with_overlapping_shareable_composite_field() {
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
    SUBA @join__graph(name: "suba", url: "http://localhost:4001/suba")
    SUBB @join__graph(name: "subb", url: "http://localhost:4002/subb")
    SUBC @join__graph(name: "subc", url: "http://localhost:4003/subc")
  }

  scalar link__Import

  enum link__Purpose {
    SECURITY
    EXECUTION
  }

  type Query
    @join__type(graph: SUBA)
    @join__type(graph: SUBB)
    @join__type(graph: SUBC)
  {
    me: User @join__field(graph: SUBA)
  }

  type User
    @join__type(graph: SUBA, key: "id")
    @join__type(graph: SUBB, key: "id", extension: true)
    @join__type(graph: SUBC, key: "id", extension: true)
  {
    id: ID!
    fullName: String! @join__field(graph: SUBA) @join__field(graph: SUBB, external: true)
    profile: Profile
      @join__field(graph: SUBB, requires: "fullName")
      @join__field(graph: SUBC)
  }

  type Profile
    @join__type(graph: SUBB)
    @join__type(graph: SUBC)
  {
    firstName: String @join__field(graph: SUBB)
    address: String @join__field(graph: SUBC)
  }"#;

    let query = "query {
        me {
          profile {
            firstName
            address
          }
        }
      }";

    let subgraphs = MockedSubgraphs(
        [
            (
                "suba",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{"query": "{me{__typename id fullName}}"}},
                        // fullName missing — subB (which requires fullName) will be skipped.
                        serde_json::json! {{"data": {
                            "me": {
                                "__typename": "User",
                                "id": "1"
                            }
                        } }},
                    )
                    .build(),
            ),
            // subB is skipped — it should never be called.
            ("subb", MockSubgraph::builder().build()),
            (
                "subc",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{
                            "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{profile{address}}}}",
                            "variables": {
                                "representations": [
                                    {"__typename": "User", "id": "1"}
                                ]
                            }
                        }},
                        serde_json::json! {{"data": {
                            "_entities": [
                                {"profile": {"address": "123 Main St"}}
                            ]
                        } }},
                    )
                    .build(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
        }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(Context::new())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    let value = serde_json::to_value(&response).unwrap();

    // subC succeeded — address should be present; firstName was not fetched.
    let profile = &value["data"]["me"]["profile"];
    assert_eq!(profile["address"], serde_json::json!("123 Main St"));
    assert_eq!(profile["firstName"], serde_json::Value::Null);

    let errors = value["errors"]
        .as_array()
        .expect("errors should be present");

    // Only firstName (uniquely provided by the skipped subB) should be reported.
    // `profile` itself should NOT be flagged — subC successfully provided it.
    assert_eq!(
        errors.len(),
        1,
        "should only report the field uniquely provided by the skipped fetch, got: {:?}",
        errors
    );
    assert_eq!(
        errors[0]["path"],
        serde_json::json!(["me", "profile", "firstName"])
    );
    assert_eq!(
        errors[0]["extensions"]["code"].as_str(),
        Some("UNSATISFIED_FETCH_CONDITION"),
    );
}

// When the skipped fetch would have populated a list-typed field with per-item
// sub-selections, the error must stop at the list field itself. The
// response-key tree has no array indices, so we can't enumerate per-item
// leaves like `[..., addresses, 0, street]`.
//
// Scenario:
//   query { me { addresses { street city } } }
//   subA: me -> User { id, fullName }   (fullName missing from response)
//   subB: User.addresses @requires(fullName) -> [Address { street, city }] (skipped)
#[tokio::test]
async fn skipped_fetch_with_list_field_reports_at_list_level() {
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
    SUBA @join__graph(name: "suba", url: "http://localhost:4001/suba")
    SUBB @join__graph(name: "subb", url: "http://localhost:4002/subb")
  }

  scalar link__Import

  enum link__Purpose {
    SECURITY
    EXECUTION
  }

  type Query
    @join__type(graph: SUBA)
    @join__type(graph: SUBB)
  {
    me: User @join__field(graph: SUBA)
  }

  type User
    @join__type(graph: SUBA, key: "id")
    @join__type(graph: SUBB, key: "id", extension: true)
  {
    id: ID!
    fullName: String! @join__field(graph: SUBA) @join__field(graph: SUBB, external: true)
    addresses: [Address]
      @join__field(graph: SUBB, requires: "fullName")
  }

  type Address
    @join__type(graph: SUBB)
  {
    street: String @join__field(graph: SUBB)
    city: String @join__field(graph: SUBB)
  }"#;

    let query = "query {
        me {
          addresses {
            street
            city
          }
        }
      }";

    let subgraphs = MockedSubgraphs(
        [
            (
                "suba",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{"query": "{me{__typename id fullName}}"}},
                        // fullName missing — subB (which requires fullName) will be skipped.
                        serde_json::json! {{"data": {
                            "me": {
                                "__typename": "User",
                                "id": "1"
                            }
                        } }},
                    )
                    .build(),
            ),
            // subB is skipped — it should never be called.
            ("subb", MockSubgraph::builder().build()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
        }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(Context::new())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    let value = serde_json::to_value(&response).unwrap();

    let errors = value["errors"]
        .as_array()
        .expect("errors should be present");

    // Exactly one error, at the list field itself — not descending with fake
    // indices or enumerating per-item leaves under `addresses`.
    assert_eq!(
        errors.len(),
        1,
        "should emit one error at the list field, got: {:?}",
        errors
    );
    assert_eq!(errors[0]["path"], serde_json::json!(["me", "addresses"]));
    assert_eq!(
        errors[0]["extensions"]["code"].as_str(),
        Some("UNSATISFIED_FETCH_CONDITION"),
    );
}

// When a shareable list-typed field is partially fetched — one subgraph
// populates the array while another's fetch is skipped for @requires — we
// still emit an error at the list field. The tree has no array indices, so we
// can't enumerate per-item leaves for the skipped subgraph's contributions,
// and the list-level emission must not be suppressed just because another
// fetch already wrote a value at that path.
//
// Scenario:
//   query { person { friends { name hobby job } } }
//   subA: person -> Person { id, friends: [Friend{name, hobby}] }   (fullName missing)
//   subB: Person.friends @requires(fullName) -> [Friend{job}]       (skipped)
#[tokio::test]
async fn skipped_fetch_with_shareable_list_field_reports_error_and_value() {
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
    SUBA @join__graph(name: "suba", url: "http://localhost:4001/suba")
    SUBB @join__graph(name: "subb", url: "http://localhost:4002/subb")
  }

  scalar link__Import

  enum link__Purpose {
    SECURITY
    EXECUTION
  }

  type Query
    @join__type(graph: SUBA)
    @join__type(graph: SUBB)
  {
    person: Person @join__field(graph: SUBA)
  }

  type Person
    @join__type(graph: SUBA, key: "id")
    @join__type(graph: SUBB, key: "id", extension: true)
  {
    id: ID!
    fullName: String! @join__field(graph: SUBA) @join__field(graph: SUBB, external: true)
    friends: [Friend]
      @join__field(graph: SUBA)
      @join__field(graph: SUBB, requires: "fullName")
  }

  type Friend
    @join__type(graph: SUBA)
    @join__type(graph: SUBB)
  {
    name: String @join__field(graph: SUBA) @join__field(graph: SUBB)
    hobby: String @join__field(graph: SUBA)
    job: String @join__field(graph: SUBB)
  }"#;

    let query = "query {
        person {
          friends {
            name
            hobby
            job
          }
        }
      }";

    let subgraphs = MockedSubgraphs(
        [
            (
                "suba",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json! {{"query": "{person{__typename id friends{name hobby} fullName}}"}},
                        // fullName missing — subB (which requires fullName) is skipped.
                        serde_json::json! {{"data": {
                            "person": {
                                "__typename": "Person",
                                "id": "1",
                                "friends": [
                                    { "name": "Alice", "hobby": "chess" }
                                ]
                            }
                        } }},
                    )
                    .build(),
            ),
            // subB is skipped — it should never be called.
            ("subb", MockSubgraph::builder().build()),
        ]
        .into_iter()
        .collect(),
    );

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": { "all": true },
        }))
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(Context::new())
        .query(query)
        .build()
        .unwrap();

    let mut stream = service.clone().oneshot(request).await.unwrap();
    let response = stream.next_response().await.unwrap();
    let value = serde_json::to_value(&response).unwrap();

    let errors = value["errors"]
        .as_array()
        .expect("errors should be present");

    // One UNSATISFIED_FETCH_CONDITION error at the list field itself — even
    // though subA populated `friends` with a value.
    let unsatisfied: Vec<_> = errors
        .iter()
        .filter(|e| e["extensions"]["code"].as_str() == Some("UNSATISFIED_FETCH_CONDITION"))
        .collect();
    assert_eq!(
        unsatisfied.len(),
        1,
        "should emit exactly one UNSATISFIED_FETCH_CONDITION at the list field, got: {:?}",
        errors
    );
    assert_eq!(
        unsatisfied[0]["path"],
        serde_json::json!(["person", "friends"])
    );

    // The response still carries the array populated by subA.
    let friends = &value["data"]["person"]["friends"];
    assert!(
        friends.is_array(),
        "friends should be populated by subA, got: {:?}",
        value["data"]
    );
    assert_eq!(friends[0]["name"].as_str(), Some("Alice"));
    assert_eq!(friends[0]["hobby"].as_str(), Some("chess"));
}
