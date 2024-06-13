use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::subgraph;
use base64::prelude::BASE64_STANDARD;
use base64::Engine as _;
use prost::Message;
use prost_types::Timestamp;
use proto::reports::trace::node::Id::Index;
use proto::reports::trace::node::Id::ResponseName;
use proto::reports::trace::Node;
use proto::reports::Trace;
use serde_json::json;
use tower::ServiceExt;

#[allow(unreachable_pub)]
pub(crate) mod proto {
    pub(crate) mod reports {
        #![allow(clippy::derive_partial_eq_without_eq)]
        tonic::include_proto!("reports");
    }
}

pub(crate) fn encode_ftv1(trace: Trace) -> String {
    BASE64_STANDARD.encode(trace.encode_to_vec())
}

pub(crate) fn subgraph_mocks(subgraph: &str) -> subgraph::BoxService {
    let builder = MockSubgraph::builder();
    // base64 FTV1 blobs were manually captured from un-mocked responses
    if subgraph == "products" {
      let trace = Trace {
          start_time: Some(Timestamp { seconds: 1677594281, nanos: 831000000 }),
          end_time: Some(Timestamp { seconds: 1677594281, nanos: 832000000 }),
          duration_ns: 726851,
          root: Some(
              Node {
                  original_field_name: "".into(),
                  r#type: "".into(),
                  parent_type: "".into(),
                  cache_policy: None,
                  start_time: 0,
                  end_time: 0,
                  error: vec![],
                  child: vec![
                      Node {
                          original_field_name: "".into(),
                          r#type: "[Product]".into(),
                          parent_type: "Query".into(),
                          cache_policy: None,
                          start_time: 402005,
                          end_time: 507563,
                          // Synthetic errors for testing error stats
                          error: vec![Default::default(), Default::default()],
                          child: vec![
                              Node {
                                  original_field_name: "".into(),
                                  r#type: "".into(),
                                  parent_type: "".into(),
                                  cache_policy: None,
                                  start_time: 0,
                                  end_time: 0,
                                  error: vec![],
                                  child: vec![
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "String!".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 580346,
                                          end_time: 593649,
                                          error: vec![],
                                          child: vec![],
                                          id: Some(ResponseName("upc".into())),
                                      },
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "String".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 602613,
                                          end_time: 609973,
                                          error: vec![],
                                          child: vec![],
                                          id: Some(ResponseName("name".into())),
                                      },
                                  ],
                                  id: Some(Index(0)),
                              },
                              Node {
                                  original_field_name: "".into(),
                                  r#type: "".into(),
                                  parent_type: "".into(),
                                  cache_policy: None,
                                  start_time: 0,
                                  end_time: 0,
                                  error: vec![],
                                  child: vec![
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "String!".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 626113,
                                          end_time: 630409,
                                          error: vec![],
                                          child: vec![],
                                          id: Some(ResponseName("upc".into())),
                                      },
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "String".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 637000,
                                          end_time: 639867,
                                          error: vec![],
                                          child: vec![],
                                          id: Some(ResponseName("name".into())),
                                      },
                                  ],
                                  id: Some(Index(1)),
                              },
                              Node {
                                  original_field_name: "".into(),
                                  r#type: "".into(),
                                  parent_type: "".into(),
                                  cache_policy: None,
                                  start_time: 0,
                                  end_time: 0,
                                  error: vec![],
                                  child: vec![
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "String!".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 651656,
                                          end_time: 654866,
                                          error: vec![],
                                          child: vec![],
                                          id: Some(ResponseName("upc".into())),
                                      },
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "String".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 658295,
                                          end_time: 661247,
                                          error: vec![],
                                          child: vec![],
                                          id: Some(ResponseName("name".into())),
                                      },
                                  ],
                                  id: Some(Index(2)),
                              },
                          ],
                          id: Some(ResponseName("topProducts".into())),
                      },
                  ],
                  id: None,
              },
          ),
          field_execution_weight: 1.0,
          ..Default::default()
      };
      builder.with_json(
          json!({"query": "{topProducts{__typename upc name}}"}),
          json!({
              "data": {"topProducts": [
                  {"__typename": "Product", "upc": "1", "name": "Table"},
                  {"__typename": "Product", "upc": "2", "name": "Couch"},
                  {"__typename": "Product", "upc": "3", "name": "Chair"}
              ]},
              "errors": [
                  {"message": "", "path": ["topProducts"]},
                  {"message": "", "path": ["topProducts"]},
              ],
              "extensions": {"ftv1": encode_ftv1(trace)}
          }),
      )
  } else if subgraph == "reviews" {
      let trace = Trace {
          start_time: Some(Timestamp { seconds: 1677594281, nanos: 915000000 }),
          end_time: Some(Timestamp { seconds: 1677594281, nanos: 917000000 }),
          duration_ns: 1772792,
          root: Some(
              Node {
                  original_field_name: "".into(),
                  r#type: "".into(),
                  parent_type: "".into(),
                  cache_policy: None,
                  start_time: 0,
                  end_time: 0,
                  error: vec![],
                  child: vec![
                      Node {
                          original_field_name: "".into(),
                          r#type: "[_Entity]!".into(),
                          parent_type: "Query".into(),
                          cache_policy: None,
                          start_time: 264001,
                          end_time: 358151,
                          error: vec![],
                          child: vec![
                              Node {
                                  original_field_name: "".into(),
                                  r#type: "".into(),
                                  parent_type: "".into(),
                                  cache_policy: None,
                                  start_time: 0,
                                  end_time: 0,
                                  error: vec![],
                                  child: vec![
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "[Review]".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 401851,
                                          end_time: 1540892,
                                          error: vec![],
                                          child: vec![
                                              Node {
                                                  original_field_name: "".into(),
                                                  r#type: "".into(),
                                                  parent_type: "".into(),
                                                  cache_policy: None,
                                                  start_time: 0,
                                                  end_time: 0,
                                                  error: vec![],
                                                  child: vec![
                                                      Node {
                                                          original_field_name: "".into(),
                                                          r#type: "User".into(),
                                                          parent_type: "Review".into(),
                                                          cache_policy: None,
                                                          start_time: 1558122,
                                                          end_time: 1688492,
                                                          error: vec![],
                                                          child: vec![
                                                              Node {
                                                                  original_field_name: "".into(),
                                                                  r#type: "ID!".into(),
                                                                  parent_type: "User".into(),
                                                                  cache_policy: None,
                                                                  start_time: 1699382,
                                                                  end_time: 1703952,
                                                                  error: vec![],
                                                                  child: vec![],
                                                                  id: Some(ResponseName("id".into())),
                                                              },
                                                          ],
                                                          id: Some(ResponseName("author".into())),
                                                      },
                                                  ],
                                                  id: Some(Index(0)),
                                              },
                                              Node {
                                                  original_field_name: "".into(),
                                                  r#type: "".into(),
                                                  parent_type: "".into(),
                                                  cache_policy: None,
                                                  start_time: 0,
                                                  end_time: 0,
                                                  error: vec![],
                                                  child: vec![
                                                      Node {
                                                          original_field_name: "".into(),
                                                          r#type: "User".into(),
                                                          parent_type: "Review".into(),
                                                          cache_policy: None,
                                                          start_time: 1596072,
                                                          end_time: 1706952,
                                                          error: vec![],
                                                          child: vec![
                                                              Node {
                                                                  original_field_name: "".into(),
                                                                  r#type: "ID!".into(),
                                                                  parent_type: "User".into(),
                                                                  cache_policy: None,
                                                                  start_time: 1710962,
                                                                  end_time: 1713162,
                                                                  error: vec![],
                                                                  child: vec![],
                                                                  id: Some(ResponseName("id".into())),
                                                              },
                                                          ],
                                                          id: Some(ResponseName("author".into())),
                                                      },
                                                  ],
                                                  id: Some(Index(1)),
                                              },
                                          ],
                                          id: Some(ResponseName("reviews".into())),
                                      },
                                  ],
                                  id: Some(Index(0)),
                              },
                              Node {
                                  original_field_name: "".into(),
                                  r#type: "".into(),
                                  parent_type: "".into(),
                                  cache_policy: None,
                                  start_time: 0,
                                  end_time: 0,
                                  error: vec![],
                                  child: vec![
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "[Review]".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 478041,
                                          end_time: 1620202,
                                          error: vec![],
                                          child: vec![
                                              Node {
                                                  original_field_name: "".into(),
                                                  r#type: "".into(),
                                                  parent_type: "".into(),
                                                  cache_policy: None,
                                                  start_time: 0,
                                                  end_time: 0,
                                                  error: vec![],
                                                  child: vec![
                                                      Node {
                                                          original_field_name: "".into(),
                                                          r#type: "User".into(),
                                                          parent_type: "Review".into(),
                                                          cache_policy: None,
                                                          start_time: 1626482,
                                                          end_time: 1714552,
                                                          error: vec![],
                                                          child: vec![
                                                              Node {
                                                                  original_field_name: "".into(),
                                                                  r#type: "ID!".into(),
                                                                  parent_type: "User".into(),
                                                                  cache_policy: None,
                                                                  start_time: 1718812,
                                                                  end_time: 1720712,
                                                                  error: vec![],
                                                                  child: vec![],
                                                                  id: Some(ResponseName("id".into())),
                                                              },
                                                          ],
                                                          id: Some(ResponseName("author".into())),
                                                      },
                                                  ],
                                                  id: Some(Index(0)),
                                              },
                                          ],
                                          id: Some(ResponseName("reviews".into())),
                                      },
                                  ],
                                  id: Some(Index(1)),
                              },
                              Node {
                                  original_field_name: "".into(),
                                  r#type: "".into(),
                                  parent_type: "".into(),
                                  cache_policy: None,
                                  start_time: 0,
                                  end_time: 0,
                                  error: vec![],
                                  child: vec![
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "[Review]".into(),
                                          parent_type: "Product".into(),
                                          cache_policy: None,
                                          start_time: 1457461,
                                          end_time: 1649742,
                                          error: vec![],
                                          child: vec![
                                              Node {
                                                  original_field_name: "".into(),
                                                  r#type: "".into(),
                                                  parent_type: "".into(),
                                                  cache_policy: None,
                                                  start_time: 0,
                                                  end_time: 0,
                                                  error: vec![],
                                                  child: vec![
                                                      Node {
                                                          original_field_name: "".into(),
                                                          r#type: "User".into(),
                                                          parent_type: "Review".into(),
                                                          cache_policy: None,
                                                          start_time: 1655462,
                                                          end_time: 1722082,
                                                          error: vec![],
                                                          child: vec![
                                                              Node {
                                                                  original_field_name: "".into(),
                                                                  r#type: "ID!".into(),
                                                                  parent_type: "User".into(),
                                                                  cache_policy: None,
                                                                  start_time: 1726282,
                                                                  end_time: 1728152,
                                                                  error: vec![],
                                                                  child: vec![],
                                                                  id: Some(ResponseName("id".into())),
                                                              },
                                                          ],
                                                          id: Some(ResponseName("author".into())),
                                                      },
                                                  ],
                                                  id: Some(Index(0)),
                                              },
                                          ],
                                          id: Some(ResponseName("reviews".into())),
                                      },
                                  ],
                                  id: Some(Index(2)),
                              },
                          ],
                          id: Some(ResponseName("_entities".into())),
                      },
                  ],
                  id: None,
              },
          ),
          field_execution_weight: 1.0,
          ..Default::default()
      };
      builder.with_json(
          json!({
              "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{author{__typename id}}}}}",
              "variables": {"representations": [
                  {"__typename": "Product", "upc": "1"},
                  {"__typename": "Product", "upc": "2"},
                  {"__typename": "Product", "upc": "3"},
              ]}
          }),
          json!({
              "data": {"_entities": [
                  {"reviews": [
                      {"author": {"__typename": "User", "id": "1"}},
                      {"author": {"__typename": "User", "id": "2"}},
                  ]},
                  {"reviews": [
                      {"author": {"__typename": "User", "id": "1"}},
                  ]},
                  {"reviews": [
                      {"author": {"__typename": "User", "id": "2"}},
                  ]}
              ]},
              "extensions": {"ftv1": encode_ftv1(trace)}
          })
      )
  } else if subgraph == "accounts" {
      let trace = Trace {
          start_time: Some(Timestamp { seconds: 1677594281, nanos: 961000000 }),
          end_time: Some(Timestamp { seconds: 1677594281, nanos: 961000000 }),
          duration_ns: 922066,
          root: Some(
              Node {
                  original_field_name: "".into(),
                  r#type: "".into(),
                  parent_type: "".into(),
                  cache_policy: None,
                  start_time: 0,
                  end_time: 0,
                  error: vec![],
                  child: vec![
                      Node {
                          original_field_name: "".into(),
                          r#type: "[_Entity]!".into(),
                          parent_type: "Query".into(),
                          cache_policy: None,
                          start_time: 517152,
                          end_time: 689749,
                          error: vec![],
                          child: vec![
                              Node {
                                  original_field_name: "".into(),
                                  r#type: "".into(),
                                  parent_type: "".into(),
                                  cache_policy: None,
                                  start_time: 0,
                                  end_time: 0,
                                  error: vec![],
                                  child: vec![
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "String".into(),
                                          parent_type: "User".into(),
                                          cache_policy: None,
                                          start_time: 1000000,
                                          end_time: 1002000,
                                          error: vec![],
                                          child: vec![],
                                          id: Some(ResponseName("name".into())),
                                      },
                                  ],
                                  id: Some(Index(0)),
                              },
                              Node {
                                  original_field_name: "".into(),
                                  r#type: "".into(),
                                  parent_type: "".into(),
                                  cache_policy: None,
                                  start_time: 0,
                                  end_time: 0,
                                  error: vec![],
                                  child: vec![
                                      Node {
                                          original_field_name: "".into(),
                                          r#type: "String".into(),
                                          parent_type: "User".into(),
                                          cache_policy: None,
                                          start_time: 811212,
                                          end_time: 821266,
                                          // Synthetic error for testing error stats
                                          error: vec![Default::default()],
                                          child: vec![],
                                          id: Some(ResponseName("name".into())),
                                      },
                                  ],
                                  id: Some(Index(1)),
                              },
                          ],
                          id: Some(ResponseName("_entities".into())),
                      },
                  ],
                  id: None,
              },
          ),
          field_execution_weight: 1.0,
          ..Default::default()
      };
      builder.with_json(
          json!({
              "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
              "variables": {"representations": [
                  {"__typename": "User", "id": "1"},
                  {"__typename": "User", "id": "2"},
              ]}
          }),
          json!({
              "data": {"_entities": [
                  {"name": "Ada Lovelace"},
                  {"name": "Alan Turing"},
              ]},
              "errors": [
                  {"message": "", "path": ["_entities", 1, "name"]},
              ],
              "extensions": {"ftv1": encode_ftv1(trace)}
          })
      )
  } else {
      builder
  }
  .build()
  .boxed()
}
