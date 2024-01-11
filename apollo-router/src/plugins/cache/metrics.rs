struct CacheCounter {
    primary: Bloom<CacheKey>,
    secondary: Bloom<CacheKey>,
    created_at: Instant,
    ttl: Duration,
}

impl CacheCounter {
    fn new(ttl: Duration) -> Self {
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

    fn record(
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
