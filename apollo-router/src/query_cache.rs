//! A cache for queries.
use opentelemetry::trace::SpanKind;
use tracing::{info_span, Instrument};

use crate::cache::CachingMap;
use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::traits::CacheResolver;
use crate::*;
use std::sync::Arc;

/// A cache for parsed GraphQL queries.
#[derive(Debug)]
pub struct QueryCache {
    cm: CachingMap<String, Option<Arc<Query>>>,
}

/// A resolver for cache misses
struct QueryCacheResolver {
    schema: Arc<Schema>,
}

#[async_trait::async_trait]
impl CacheResolver<String, Option<Arc<Query>>> for QueryCacheResolver {
    async fn retrieve(&self, key: String) -> Result<Option<Arc<Query>>, CacheResolverError> {
        let schema = self.schema.clone();
        let query_parsing_future = tokio::task::spawn_blocking(move || Query::parse(key, &schema))
            .instrument(info_span!("parse_query", "otel.kind" = %SpanKind::Internal));
        let parsed_query = match query_parsing_future.await {
            Ok(res) => Some(Arc::new(res.map_err(QueryPlannerError::from)?)),
            // Silently ignore cancelled tasks (never happen for blocking tasks).
            Err(err) if err.is_cancelled() => None,
            Err(err) => {
                failfast_debug!("parsing query task failed: {}", err);
                return Err(QueryPlannerError::from(err).into());
            }
        };

        Ok(parsed_query)
    }
}

impl QueryCache {
    /// Instantiate a new cache for parsed GraphQL queries.
    pub fn new(cache_limit: usize, schema: Arc<Schema>) -> Self {
        let resolver = QueryCacheResolver { schema };
        let cm = CachingMap::new(Box::new(resolver), cache_limit);
        Self { cm }
    }

    /// Attempt to parse a string to a [`Query`] using cache if possible.
    pub async fn get(&self, query: impl AsRef<str>) -> Option<Arc<Query>> {
        let key = query.as_ref().to_string();

        match self.cm.get(key).await {
            Ok(v) => v,
            Err(err) => {
                failfast_debug!("parsing query task failed: {}", err);
                None
            }
        }
    }
}
