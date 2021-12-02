use crate::prelude::graphql::*;
use crate::CacheCallback;
use std::sync::Arc;

/// A cache for parsed GraphQL queries.
#[derive(Debug)]
pub struct QueryCache {
    cm: CachingMap<QueryPlannerError, String, Option<Arc<Query>>>,
    schema: Arc<Schema>,
}

#[async_trait::async_trait]
impl CacheCallback<QueryPlannerError, String, Option<Arc<Query>>> for QueryCache {
    async fn delegated_get(&self, key: String) -> Result<Option<Arc<Query>>, QueryPlannerError> {
        let query_parsing_future = {
            let schema = Arc::clone(&self.schema);
            tokio::task::spawn_blocking(move || Query::parse(key, &schema))
        };
        let parsed_query = match query_parsing_future.await {
            Ok(res) => res.map(Arc::new),
            // Silently ignore cancelled tasks (never happen for blocking tasks).
            Err(err) if err.is_cancelled() => None,
            Err(err) => {
                failfast_debug!("Parsing query task failed: {}", err);
                None
            }
        };
        Ok(parsed_query)
    }
}

impl QueryCache {
    /// Instantiate a new cache for parsed GraphQL queries.
    pub fn new(cache_limit: usize, schema: Arc<Schema>) -> Self {
        let cm = CachingMap::new(cache_limit);
        Self { cm, schema }
    }

    /// Attempt to parse a string to a [`Query`] using cache if possible.
    pub async fn get_query(&self, query: impl AsRef<str>) -> Option<Arc<Query>> {
        let key = query.as_ref().to_string();
        /*
        let q = |key: String| async move {
            let query_parsing_future = {
                let schema = Arc::clone(&self.schema);
                tokio::task::spawn_blocking(move || Query::parse(key, &schema))
            };
            let parsed_query = match query_parsing_future.await {
                Ok(res) => res.map(Arc::new),
                // Silently ignore cancelled tasks (never happen for blocking tasks).
                Err(err) if err.is_cancelled() => None,
                Err(err) => {
                    failfast_debug!("Parsing query task failed: {}", err);
                    None
                }
            };
            Ok(parsed_query)
        };
        */

        match self.cm.get(self, key).await {
            Ok(v) => v,
            Err(err) => {
                failfast_debug!("Parsing query task failed: {}", err);
                None
            }
        }
    }
}
