#[cfg(test)]
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use router_bridge::introspect::IntrospectionError;
use router_bridge::planner::Planner;

use crate::cache::storage::CacheStorage;
use crate::graphql::Response;
use crate::query_planner::QueryPlanResult;

const DEFAULT_INTROSPECTION_CACHE_CAPACITY: NonZeroUsize =
    unsafe { NonZeroUsize::new_unchecked(5) };

/// A cache containing our well known introspection queries.
pub(crate) struct Introspection {
    cache: CacheStorage<String, Response>,
    planner: Arc<Planner<QueryPlanResult>>,
}

impl Introspection {
    pub(crate) async fn with_capacity(
        planner: Arc<Planner<QueryPlanResult>>,
        capacity: NonZeroUsize,
    ) -> Self {
        Self {
            cache: CacheStorage::new(capacity, None, None, "introspection").await,
            planner,
        }
    }

    pub(crate) async fn new(planner: Arc<Planner<QueryPlanResult>>) -> Self {
        Self::with_capacity(planner, DEFAULT_INTROSPECTION_CACHE_CAPACITY).await
    }

    #[cfg(test)]
    pub(crate) async fn from_cache(
        planner: Arc<Planner<QueryPlanResult>>,
        cache: HashMap<String, Response>,
    ) -> Self {
        let this = Self::with_capacity(planner, cache.len().try_into().unwrap()).await;

        for (query, response) in cache.into_iter() {
            this.cache.insert(query, response).await;
        }
        this
    }

    /// Execute an introspection and cache the response.
    pub(crate) async fn execute(&self, query: String) -> Result<Response, IntrospectionError> {
        if let Some(response) = self.cache.get(&query).await {
            return Ok(response);
        }

        // Do the introspection query and cache it
        let response =
            self.planner
                .introspect(query.clone())
                .await
                .map_err(|_e| IntrospectionError {
                    message: String::from("cannot find the introspection response").into(),
                })?;

        let introspection_result = response.into_result().map_err(|err| IntrospectionError {
            message: format!(
                "introspection error : {}",
                err.into_iter()
                    .map(|err| err.to_string())
                    .collect::<Vec<String>>()
                    .join(", "),
            )
            .into(),
        })?;

        let response = Response::builder().data(introspection_result).build();

        self.cache.insert(query, response.clone()).await;

        Ok(response)
    }
}

#[cfg(test)]
mod introspection_tests {
    use std::sync::Arc;

    use router_bridge::planner::IncrementalDeliverySupport;
    use router_bridge::planner::QueryPlannerConfig;

    use super::*;

    #[tokio::test]
    async fn test_plan_cache() {
        let query_to_test = r#"{
            __schema {
              types {
                name
              }
            }
          }"#;
        let schema = include_str!("../tests/fixtures/supergraph.graphql");
        let expected_data = Response::builder().data(42).build();

        let planner = Arc::new(
            Planner::new(
                schema.to_string(),
                QueryPlannerConfig {
                    incremental_delivery: Some(IncrementalDeliverySupport {
                        enable_defer: Some(true),
                    }),
                    graphql_validation: true,
                    reuse_query_fragments: Some(false),
                },
            )
            .await
            .unwrap(),
        );

        let cache = [(query_to_test.to_string(), expected_data.clone())]
            .iter()
            .cloned()
            .collect();
        let introspection = Introspection::from_cache(planner, cache).await;

        assert_eq!(
            expected_data,
            introspection
                .execute(query_to_test.to_string())
                .await
                .unwrap()
        );
    }
}
