// TODO: may need to be able to serialize and deserialize, probably with
// #[serde(skip_serializing_if = "Option::is_none", default)]
// #[serde(skip_serializing_if = "is_false", default)]

/// Cache control header either:
/// * Sent from client to router to control response cache
/// * Sent from router to subgraph to control external cache (ie CDN)
pub(crate) struct CacheControl {
    /// Indicates that the client allows a stored response that is generated on the origin server within N seconds.
    max_age: Option<u64>,

    /// Indicates that the client allows a stored response that is stale within N seconds.
    max_stale: Option<u64>,

    /// Indicates that the client allows a stored response that is fresh for at least N seconds
    min_fresh: Option<u64>,

    /// Asks cache to validate the response with the origin server before reuse.
    no_cache: bool,

    /// Asks cache to refrain from storing the request and corresponding response — even if the origin server's response could be stored.
    no_store: bool,

    /// Indicates that any intermediary (regardless of whether it implements a cache) shouldn't transform the response contents.
    /// TODO: not sure if used by the router
    no_transform: bool,

    /// Indicates that an already-cached response should be returned. If a cache has a stored response, even a stale one, it will be returned. If no cached response is available, a 504 Gateway Timeout response will be returned.
    only_if_cached: bool,

    /// Indicates that the browser is interested in receiving stale content on error from any intermediate server for a particular origin.
    /// TODO: apparently this is not supported by any browser.
    stale_if_error: bool,
}

impl Default for CacheControl {
    fn default() -> Self {
        Self {
            max_age: None,
            max_stale: None,
            min_fresh: None,
            no_cache: false,
            no_store: false,
            no_transform: false,
            only_if_cached: false,
            stale_if_error: false,
        }
    }
}
