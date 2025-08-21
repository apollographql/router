use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;

/// Cache policy for connector responses
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ConnectorCachePolicy {
    /// Maximum age for the cache entry
    pub max_age: Option<Duration>,
    /// Whether the response can be cached in public caches
    pub public: bool,
    /// Cache tags for cache invalidation
    pub cache_tags: Vec<String>,
    /// Whether the response should bypass the cache entirely
    pub no_cache: bool,
}

impl ConnectorCachePolicy {
    /// Create a new cache policy with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum age for the cache entry
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    /// Set whether the response can be cached in public caches
    pub fn with_public(mut self, public: bool) -> Self {
        self.public = public;
        self
    }

    /// Add cache tags for cache invalidation
    pub fn with_cache_tags(mut self, cache_tags: Vec<String>) -> Self {
        self.cache_tags = cache_tags;
        self
    }

    /// Add a single cache tag
    pub fn with_cache_tag(mut self, cache_tag: String) -> Self {
        self.cache_tags.push(cache_tag);
        self
    }

    /// Set whether the response should bypass the cache entirely
    pub fn with_no_cache(mut self, no_cache: bool) -> Self {
        self.no_cache = no_cache;
        self
    }

    /// Check if the response is cacheable
    pub fn is_cacheable(&self) -> bool {
        !self.no_cache && self.max_age.is_some()
    }
}
