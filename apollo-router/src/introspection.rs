#[cfg(test)]
use std::collections::HashMap;

use router_bridge::introspect;
use router_bridge::introspect::IntrospectionError;

use crate::cache::storage::CacheStorage;
use crate::graphql::Response;

const DEFAULT_INTROSPECTION_CACHE_CAPACITY: usize = 5;

/// A cache containing our well known introspection queries.
pub(crate) struct Introspection {
    cache: CacheStorage<String, Response>,
}

impl Introspection {
    pub(crate) async fn with_capacity(capacity: usize) -> Self {
        Self {
            cache: CacheStorage::new(capacity).await,
        }
    }

    pub(crate) async fn new() -> Self {
        Self::with_capacity(DEFAULT_INTROSPECTION_CACHE_CAPACITY).await
    }

    #[cfg(test)]
    pub(crate) async fn from_cache(cache: HashMap<String, Response>) -> Self {
        let this = Self::with_capacity(cache.len()).await;

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
        let mut response = introspect::batch_introspect(schema_sdl, vec![query.to_owned()])
            .map_err(|err| IntrospectionError {
                message: format!("Deno runtime error: {:?}", err).into(),
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
        let introspection = Introspection::from_cache(cache).await;

        assert_eq!(
            expected_data,
            introspection
                .execute(schema, query_to_test.to_string())
                .await
                .unwrap()
        );
    }
}
