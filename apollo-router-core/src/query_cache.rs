use crate::prelude::graphql::*;
use crate::CacheResolver;
use std::sync::Arc;

/// A cache for parsed GraphQL queries.
#[derive(Debug)]
pub struct QueryCache {
    cm: CachingMap<String, Option<Arc<Query>>>,
}

/// A resolver for cache misses
struct QueryCacheResolver;

#[async_trait::async_trait]
impl CacheResolver<String, Option<Arc<Query>>> for QueryCacheResolver {
    async fn retrieve(&self, key: String) -> Result<Option<Arc<Query>>, CacheResolverError> {
        let query_parsing_future = tokio::task::spawn_blocking(|| Query::parse(key));
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
    pub fn new(cache_limit: usize) -> Self {
        let resolver = QueryCacheResolver;
        let cm = CachingMap::new(Box::new(resolver), cache_limit);
        Self { cm }
    }

    /// Attempt to parse a string to a [`Query`] using cache if possible.
    pub async fn get_query(&self, query: impl AsRef<str>) -> Option<Arc<Query>> {
        let key = query.as_ref().to_string();

        match self.cm.get(key).await {
            Ok(v) => v,
            Err(err) => {
                failfast_debug!("Parsing query task failed: {}", err);
                None
            }
        }
    }
}
