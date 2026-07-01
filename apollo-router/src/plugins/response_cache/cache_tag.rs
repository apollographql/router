//! Typed cache-tag entries used by `response_cache` to index cached documents in Redis.
//!
//! Each [`CacheTag`] variant corresponds to one of the three invalidation modes documented at
//! <https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/invalidation>.
//! The plugin layer builds a `Vec<CacheTag>` per cached document, filtered by the subgraph's
//! active [`InvalidationIndexes`](super::invalidation_endpoint::InvalidationIndexes), and the
//! storage layer renders each entry into a Redis ZSET key via [`CacheTag::to_redis_key`].
//!
//! This module replaces an older string-based representation in which a single
//! `Vec<String>` mixed user-facing and internal cache-tag values, requiring the debugger and
//! invalidation filter to inspect the `__apollo_internal::` string prefix to distinguish the
//! two. With the typed representation the distinction is structural and the storage layer
//! has no policy of its own; it is a pure rendering step.

use super::invalidation_endpoint::IndexMode;
use super::plugin::INTERNAL_CACHE_TAG_PREFIX;
use super::plugin::RESPONSE_CACHE_VERSION;

/// One logical cache-tag entry a cached document is indexed under.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CacheTag {
    /// Whole-subgraph index entry. Backs `By subgraph` invalidation requests.
    Subgraph,
    /// Per-type index entry, parameterized by GraphQL type name. Backs `By type` invalidation.
    Type(String),
    /// Per-tag index entry, parameterized by the tag value supplied via
    /// `apolloCacheTags`, `apolloEntityCacheTags`, or a resolved `@cacheTag` directive.
    /// Backs `By cache tag` invalidation.
    Tag(String),
}

impl CacheTag {
    /// Which invalidation index mode backs this cache-tag entry. Currently used by the unit
    /// tests; will be referenced by the upcoming invalidation-request matching path that
    /// maps a request kind to the index it depends on.
    #[allow(dead_code)]
    pub(crate) fn index_mode(&self) -> IndexMode {
        match self {
            CacheTag::Subgraph => IndexMode::Subgraph,
            CacheTag::Type(_) => IndexMode::Type,
            CacheTag::Tag(_) => IndexMode::CacheTag,
        }
    }

    /// `true` when this entry is internal to the router (not surfaced to operators in debug
    /// output or invalidation request results). [`CacheTag::Subgraph`] and [`CacheTag::Type`]
    /// are internal; [`CacheTag::Tag`] is user-facing.
    #[allow(dead_code)]
    pub(crate) fn is_internal(&self) -> bool {
        matches!(self, CacheTag::Subgraph | CacheTag::Type(_))
    }

    /// The user-facing string representation of this tag, used by the cache debugger to show
    /// which `@cacheTag` or extension-supplied tags a cached entry was indexed under. Returns
    /// `None` for internal entries.
    pub(crate) fn user_value(&self) -> Option<&str> {
        match self {
            CacheTag::Tag(s) => Some(s),
            _ => None,
        }
    }

    /// Render this entry as the Redis ZSET key it indexes into. The format preserves the
    /// pre-existing namespace so existing cache data continues to map correctly across the
    /// transition to the typed representation.
    pub(crate) fn to_redis_key(&self, subgraph_name: &str) -> String {
        match self {
            CacheTag::Subgraph => {
                format!("version:{RESPONSE_CACHE_VERSION}:cache-tag:subgraph-{subgraph_name}")
            }
            CacheTag::Type(type_name) => {
                // The internal prefix preserves the historical key shape so old cache entries
                // and new ones share the same ZSET namespace.
                format!(
                    "version:{RESPONSE_CACHE_VERSION}:cache-tag:subgraph-{subgraph_name}:key-{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph_name}:type:{type_name}"
                )
            }
            CacheTag::Tag(value) => {
                format!(
                    "version:{RESPONSE_CACHE_VERSION}:cache-tag:subgraph-{subgraph_name}:key-{value}"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_mode_dispatches_per_variant() {
        assert_eq!(CacheTag::Subgraph.index_mode(), IndexMode::Subgraph);
        assert_eq!(CacheTag::Type("User".into()).index_mode(), IndexMode::Type);
        assert_eq!(
            CacheTag::Tag("homepage".into()).index_mode(),
            IndexMode::CacheTag,
        );
    }

    #[test]
    fn is_internal_distinguishes_user_facing_tags() {
        assert!(CacheTag::Subgraph.is_internal());
        assert!(CacheTag::Type("User".into()).is_internal());
        assert!(!CacheTag::Tag("homepage".into()).is_internal());
    }

    #[test]
    fn user_value_returns_inner_string_only_for_tag() {
        assert_eq!(CacheTag::Subgraph.user_value(), None);
        assert_eq!(CacheTag::Type("User".into()).user_value(), None);
        assert_eq!(
            CacheTag::Tag("homepage".into()).user_value(),
            Some("homepage"),
        );
    }

    #[test]
    fn to_redis_key_subgraph_variant() {
        let key = CacheTag::Subgraph.to_redis_key("payments");
        assert!(
            key.contains(":cache-tag:subgraph-payments"),
            "expected subgraph-prefixed key, got {key}"
        );
        assert!(
            !key.contains(":key-"),
            "subgraph key should not have :key- segment, got {key}"
        );
    }

    #[test]
    fn to_redis_key_type_variant_uses_internal_prefix() {
        let key = CacheTag::Type("User".into()).to_redis_key("payments");
        assert!(
            key.contains(INTERNAL_CACHE_TAG_PREFIX),
            "type key should include the internal prefix for back-compat: {key}"
        );
        assert!(
            key.ends_with(":subgraph:payments:type:User"),
            "type key tail mismatch: {key}"
        );
    }

    #[test]
    fn to_redis_key_tag_variant_inlines_user_value() {
        let key = CacheTag::Tag("homepage".into()).to_redis_key("payments");
        assert!(
            key.ends_with(":subgraph-payments:key-homepage"),
            "tag key should end with :key-{{value}}: {key}"
        );
    }
}
