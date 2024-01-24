use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use bloomfilter::Bloom;
use http::header;
use parking_lot::Mutex;
use serde_json_bytes::Value;
use tower::BoxError;
use tower_service::Service;

use super::entity::hash_query;
use super::entity::hash_vary_headers;
use super::entity::Ttl;
use super::entity::REPRESENTATIONS;
use crate::services::subgraph;
use crate::spec::TYPENAME;

pub(crate) struct CacheMetricsService(Option<InnerCacheMetricsService>);

impl CacheMetricsService {
    pub(crate) fn create(
        name: String,
        service: subgraph::BoxService,
        ttl: Option<&Ttl>,
        separate_per_type: bool,
    ) -> subgraph::BoxService {
        tower::util::BoxService::new(CacheMetricsService(Some(InnerCacheMetricsService {
            service,
            name: Arc::new(name),
            counter: Some(Arc::new(Mutex::new(CacheCounter::new(
                ttl.map(|t| t.0).unwrap_or_else(|| Duration::from_secs(60)),
                separate_per_type,
            )))),
        })))
    }
}

pub(crate) struct InnerCacheMetricsService {
    service: subgraph::BoxService,
    name: Arc<String>,
    counter: Option<Arc<Mutex<CacheCounter>>>,
}

impl Service<subgraph::Request> for CacheMetricsService {
    type Response = subgraph::Response;
    type Error = BoxError;
    type Future = <subgraph::BoxService as Service<subgraph::Request>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        match &mut self.0 {
            Some(s) => s.service.poll_ready(cx),
            None => panic!("service should have been called only once"),
        }
    }

    fn call(&mut self, request: subgraph::Request) -> Self::Future {
        match self.0.take() {
            None => panic!("service should have been called only once"),
            Some(s) => Box::pin(s.call_inner(request)),
        }
    }
}

impl InnerCacheMetricsService {
    async fn call_inner(
        mut self,
        mut request: subgraph::Request,
    ) -> Result<subgraph::Response, BoxError> {
        let cache_attributes = Self::get_cache_attributes(&mut request);
        println!(
            "inner metrics cache attributes in root req for {}: {:?}",
            self.name, cache_attributes
        );

        let response = self.service.call(request).await?;

        if let Some(cache_attributes) = cache_attributes {
            if let Some(counter) = &self.counter {
                println!("inner metrics cache {}: will update metrics", self.name,);
                Self::update_cache_metrics(&self.name, counter, &response, cache_attributes)
            }
        }

        Ok(response)
    }

    fn get_cache_attributes(sub_request: &mut subgraph::Request) -> Option<CacheAttributes> {
        let body = sub_request.subgraph_request.body_mut();
        let hashed_query = hash_query(&sub_request.query_hash, body);
        let representations = body
            .variables
            .get(REPRESENTATIONS)
            .and_then(|value| value.as_array())?;

        let keys = extract_cache_attributes(representations).ok()?;

        Some(CacheAttributes {
            headers: sub_request.subgraph_request.headers().clone(),
            hashed_query: Arc::new(hashed_query),
            representations: keys,
        })
    }

    fn update_cache_metrics(
        subgraph_name: &Arc<String>,
        counter: &Mutex<CacheCounter>,
        sub_response: &subgraph::Response,
        cache_attributes: CacheAttributes,
    ) {
        let mut vary_headers = sub_response
            .response
            .headers()
            .get_all(header::VARY)
            .into_iter()
            .filter_map(|val| {
                val.to_str().ok().map(|v| {
                    v.to_string()
                        .split(", ")
                        .map(|s| s.to_string())
                        .collect::<Vec<String>>()
                })
            })
            .flatten()
            .collect::<Vec<String>>();
        vary_headers.sort();
        let vary_headers = vary_headers.join(", ");

        let hashed_headers = if vary_headers.is_empty() {
            Arc::default()
        } else {
            Arc::new(hash_vary_headers(&cache_attributes.headers))
        };
        println!("will update cache counter");

        CacheCounter::record(
            counter,
            cache_attributes.hashed_query.clone(),
            subgraph_name,
            hashed_headers,
            cache_attributes.representations,
        );
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CacheAttributes {
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
    per_type: bool,
}

impl CacheCounter {
    pub(crate) fn new(ttl: Duration, per_type: bool) -> Self {
        Self {
            primary: Self::make_filter(),
            secondary: Self::make_filter(),
            created_at: Instant::now(),
            ttl,
            per_type,
        }
    }

    fn make_filter() -> Bloom<CacheKey> {
        // the filter is around 4kB in size (can be calculated with `Bloom::compute_bitmap_size`)
        Bloom::new_for_fp_rate(10000, 0.2)
    }

    pub(crate) fn record(
        counter: &Mutex<CacheCounter>,
        query: Arc<String>,
        subgraph_name: &Arc<String>,
        hashed_headers: Arc<String>,
        representations: Vec<(Arc<String>, Value)>,
    ) {
        let separate_metrics_per_type;
        {
            let mut c = counter.lock();
            if c.created_at.elapsed() >= c.ttl {
                c.clear();
            }
            separate_metrics_per_type = c.per_type;
        }

        // typename -> (nb of cache hits, nb of entities)
        let mut seen: HashMap<Arc<String>, (usize, usize)> = HashMap::new();
        let mut key = CacheKey {
            representation: Value::Null,
            typename: Arc::new(String::new()),
            query,
            subgraph_name: subgraph_name.clone(),
            hashed_headers,
        };
        for (typename, representation) in representations {
            let cache_hit;
            key.typename = typename.clone();
            key.representation = representation;

            {
                let mut c = counter.lock();
                cache_hit = c.check(&key);
            }

            let seen_entry = seen.entry(typename.clone()).or_default();
            if cache_hit {
                seen_entry.0 += 1;
            }
            seen_entry.1 += 1;
        }

        for (typename, (cache_hit, total_entities)) in seen.into_iter() {
            if separate_metrics_per_type {
                ::tracing::info!(
                    histogram.apollo.router.operations.entity.cache_hit = (cache_hit as f64 / total_entities as f64) * 100f64,
                    entity_type = %typename,
                    subgraph = %subgraph_name,
                );
            } else {
                ::tracing::info!(
                    histogram.apollo.router.operations.entity.cache_hit = (cache_hit as f64 / total_entities as f64) * 100f64,
                    subgraph = %subgraph_name,
                );
            }
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
