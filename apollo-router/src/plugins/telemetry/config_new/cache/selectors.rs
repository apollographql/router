use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

use crate::plugins::telemetry::config_new::Selector;
use crate::services::subgraph;
use crate::Context;

// #[derive(Deserialize, JsonSchema, Clone, PartialEq)]
// #[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
// enum CacheSelector {
//     Cache {
//         /// Select if you want to get cache hit or cache miss
//         cache: CacheKind,
//         /// Specify the entity type on which you want the cache data. (default: all)
//         entity_type: EntityType,
//     },
// }

// impl Selector for CacheSelector {
//     type Request = subgraph::Request;
//     type Response = subgraph::Response;
//     type EventResponse = ();

//     fn on_request(&self, _request: &Self::Request) -> Option<opentelemetry::Value> {
//         None
//     }

//     fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value> {
//         let cache_info: CacheSubgraph = response
//             .context
//             .get(response.subgraph_name.as_ref()?.as_str())
//             .ok()
//             .flatten()?;
//         match self {
//             CacheSelector::Cache { cache, entity_type } => match entity_type {
//                 EntityType::All => Some(
//                     (cache_info
//                         .0
//                         .iter()
//                         .fold(0usize, |acc, (_entity_type, cache_hit_miss)| match cache {
//                             CacheKind::Hit => acc + cache_hit_miss.hit,
//                             CacheKind::Miss => acc + cache_hit_miss.miss,
//                         }) as i64)
//                         .into(),
//                 ),
//                 EntityType::Named(entity_type_name) => {
//                     let res =
//                         cache_info
//                             .0
//                             .iter()
//                             .fold(0usize, |acc, (entity_type, cache_hit_miss)| {
//                                 if entity_type == entity_type_name {
//                                     match cache {
//                                         CacheKind::Hit => acc + cache_hit_miss.hit,
//                                         CacheKind::Miss => acc + cache_hit_miss.miss,
//                                     }
//                                 } else {
//                                     acc
//                                 }
//                             });

//                     (res != 0).then_some((res as i64).into())
//                 }
//             },
//         }
//     }

//     fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Option<opentelemetry::Value> {
//         None
//     }
// }
