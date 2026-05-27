use crate::plugins::response_cache::cache_control::now_epoch_seconds;

/// Cache control header either:
/// * Returned from subgraph to router to control response cache
/// * Returned from router to client to control external cache (ie CDN)
pub(crate) struct CacheControl {
    created: u64,

    // Not actually part of the header, used to offset the max_age
    age: Option<u64>,

    /// Indicates that the response remains fresh until N seconds after the response is generated
    max_age: Option<u64>,

    /// Indicates how long the response remains fresh in a shared cache. Overrides the value specified by max-age.
    s_max_age: Option<u64>,

    /// Indicates that the response can be stored in caches, but the response must be validated with the origin server before each reuse, even when the cache is disconnected from the origin server.
    no_cache: bool,

    /// Indicates that caches of any kind should not store this response.
    no_store: bool,

    /// Indicates that any intermediary (regardless of whether it implements a cache) shouldn't transform the response contents.
    /// TODO: not sure if used by the router
    no_transform: bool,

    /// Indicates that the response can be stored in caches and can be reused while fresh. If the response becomes stale, it must be validated with the origin server before reuse.
    must_revalidate: bool,

    /// The equivalent of `must-revalidate`, but specifically for shared caches.
    proxy_revalidate: bool,

    /// Indicates that a cache should store the response only if it understands the requirements for caching based on status code.
    /// TODO: not sure what this means for our use case
    must_understand: bool,

    /// Indicates that the response can be stored only in a private cache (e.g., local caches in browsers).
    private: bool,

    /// Indicates that the response can be stored in a shared cache (i.e., despite the presence of the `Authorization` header).
    public: bool,

    /// Indicates that the response will not be updated while it's fresh.
    /// TODO: not sure if used by the router
    immutable: bool,

    /// Indicates that the cache could reuse a stale response while it revalidates it to a cache.
    /// TODO: not sure if used by the router
    stale_while_revalidate: Option<u64>,

    /// Indicates that the cache can reuse a stale response when an upstream server generates an error, or when the error is generated locally. Here, an error is considered any response with a status code of 500, 502, 503, or 504.
    /// TODO: not sure if used by the router
    stale_if_error: Option<u64>,
}

impl Default for CacheControl {
    fn default() -> Self {
        Self {
            created: now_epoch_seconds(),
            age: None,
            max_age: None,
            s_max_age: None,
            no_cache: false,
            no_store: false,
            no_transform: false,
            must_revalidate: false,
            proxy_revalidate: false,
            must_understand: false,
            private: false,
            public: false,
            immutable: false,
            stale_while_revalidate: None,
            stale_if_error: None,
        }
    }
}
