use bloomfilter::Bloom;
use serde_json_bytes::Value;
use tower::BoxError;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::spec::TYPENAME;

#[derive(Debug, Clone)]
pub(crate) struct CacheAttributes {
    pub(crate) subgraph_name: Arc<String>,
    pub(crate) headers: http::HeaderMap,
    pub(crate) hashed_query: Arc<String>,
    // Typename + hashed_representation
    pub(crate) representations: Vec<(Arc<String>, Value)>,
}

#[derive(Debug, Hash, Clone)]
pub(crate) struct CacheKey {
    pub(crate) representation: Value,
    pub(crate) typename: Arc<String>,
    pub(crate) query: Arc<String>,
    pub(crate) subgraph_name: Arc<String>,
    pub(crate) hashed_headers: Arc<String>,
}

// Get typename and hashed representation for each representations in the subgraph query
pub(crate) fn extract_cache_attributes(
    representations: &[Value],
) -> Result<Vec<(Arc<String>, Value)>, BoxError> {
    let mut res = Vec::new();
    for representation in representations {
        let opt_type = representation
            .as_object()
            .and_then(|o| o.get(TYPENAME))
            .ok_or("missing __typename in representation")?;
        let typename = opt_type.as_str().unwrap_or("");

        res.push((Arc::new(typename.to_string()), representation.clone()));
    }
    Ok(res)
}

pub(crate) struct CacheCounter {
    primary: Bloom<CacheKey>,
    secondary: Bloom<CacheKey>,
    created_at: Instant,
    ttl: Duration,
}

impl CacheCounter {
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            primary: Self::make_filter(),
            secondary: Self::make_filter(),
            created_at: Instant::now(),
            ttl,
        }
    }

    fn make_filter() -> Bloom<CacheKey> {
        // the filter is around 4kB in size (can be calculated with `Bloom::compute_bitmap_size`)
        Bloom::new_for_fp_rate(10000, 0.2)
    }

    pub(crate) fn record(
        &mut self,
        query: Arc<String>,
        subgraph_name: Arc<String>,
        hashed_headers: Arc<String>,
        representations: Vec<(Arc<String>, Value)>,
    ) {
        if self.created_at.elapsed() >= self.ttl {
            self.clear();
        }

        // typename -> (nb of cache hits, nb of entities)
        let mut seen: HashMap<Arc<String>, (usize, usize)> = HashMap::new();
        for (typename, representation) in representations {
            let cache_hit = self.check(&CacheKey {
                representation,
                typename: typename.clone(),
                query: query.clone(),
                subgraph_name: subgraph_name.clone(),
                hashed_headers: hashed_headers.clone(),
            });

            let seen_entry = seen.entry(typename.clone()).or_default();
            if cache_hit {
                seen_entry.0 += 1;
            }
            seen_entry.1 += 1;
        }

        for (typename, (cache_hit, total_entities)) in seen.into_iter() {
            ::tracing::info!(
                histogram.apollo.router.operations.entity.cache_hit = (cache_hit as f64 / total_entities as f64) * 100f64,
                entity_type = %typename,
                subgraph = %subgraph_name,
            );
        }
    }

    fn check(&mut self, key: &CacheKey) -> bool {
        self.primary.check_and_set(key) || self.secondary.check(key)
    }

    fn clear(&mut self) {
        let secondary = std::mem::replace(&mut self.primary, Self::make_filter());
        self.secondary = secondary;

        self.created_at = Instant::now();
    }
}
