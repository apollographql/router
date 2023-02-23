#[cfg(test)]
use std::collections::HashMap;
use std::num::NonZeroUsize;

use router_bridge::introspect;
use router_bridge::introspect::IntrospectionError;
use router_bridge::planner::IncrementalDeliverySupport;
use router_bridge::planner::QueryPlannerConfig;

use crate::cache::storage::CacheStorage;
use crate::graphql::Response;
use crate::Configuration;

const DEFAULT_INTROSPECTION_CACHE_CAPACITY: NonZeroUsize =
    unsafe { NonZeroUsize::new_unchecked(5) };

/// A cache containing our well known introspection queries.
pub(crate) struct Introspection {
    cache: CacheStorage<String, Response>,
    defer_support: bool,
}

impl Introspection {
    pub(crate) async fn with_capacity(
        configuration: &Configuration,
        capacity: NonZeroUsize,
    ) -> Self {
        Self {
            cache: CacheStorage::new(capacity, None, "introspection").await,
            defer_support: configuration.supergraph.defer_support,
        }
    }

    pub(crate) async fn new(configuration: &Configuration) -> Self {
        Self::with_capacity(configuration, DEFAULT_INTROSPECTION_CACHE_CAPACITY).await
    }

    #[cfg(test)]
    pub(crate) async fn from_cache(
        configuration: &Configuration,
        cache: HashMap<String, Response>,
    ) -> Self {
        let this = Self::with_capacity(configuration, cache.len().try_into().unwrap()).await;

        for (query, response) in cache.into_iter() {
            this.cache.insert(query, response).await;
        }
        this
    }

    /// Execute an introspection and cache the response.
    pub(crate) async fn execute(
        &self,
        schema_sdl: &str,
        query: String,
    ) -> Result<Response, IntrospectionError> {
        if let Some(response) = self.cache.get(&query).await {
            return Ok(response);
        }

        // Do the introspection query and cache it
        let mut response = introspect::batch_introspect(
            schema_sdl,
            vec![query.to_owned()],
            QueryPlannerConfig {
                incremental_delivery: Some(IncrementalDeliverySupport {
                    enable_defer: Some(self.defer_support),
                }),
            },
        )
        .map_err(|err| IntrospectionError {
            message: format!("Deno runtime error: {err:?}").into(),
        })??;
        let introspection_result = response
            .pop()
            .ok_or_else(|| IntrospectionError {
                message: String::from("cannot find the introspection response").into(),
            })?
            .into_result()
            .map_err(|err| IntrospectionError {
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
    use super::*;

    #[tokio::test]
    async fn test_plan_cache() {
        let query_to_test = "this is a test query";
        let schema = " ";
        let expected_data = Response::builder().data(42).build();

        let cache = [(query_to_test.to_string(), expected_data.clone())]
            .iter()
            .cloned()
            .collect();
        let introspection = Introspection::from_cache(&Configuration::default(), cache).await;

        assert_eq!(
            expected_data,
            introspection
                .execute(schema, query_to_test.to_string())
                .await
                .unwrap()
        );
    }
}
