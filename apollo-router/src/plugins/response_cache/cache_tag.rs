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
use super::plugin::RESPONSE_CACHE_VERSION;
use crate::plugins::response_cache::INTERNAL_CACHE_TAG_PREFIX;

/// Which caching scope a cache-tag entry belongs to. Subgraph and connector entries live in
/// **disjoint** index namespaces so that invalidation targeting one scope can never resolve (and
/// delete) entries belonging to the other, even when both scopes share a Redis instance +
/// namespace and a subgraph name happens to equal a connector source id (`subgraph.source`).
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CacheScope {
    #[default]
    Subgraph,
    Connector,
}

impl CacheScope {
    /// The scope word used in both the ZSET namespace (`cache-tag:{word}-{name}`) and the
    /// internal document-key segment (`version:X:{word}:{name}:type:T`). Keeping the same word in
    /// both places preserves the write-path/invalidate-path string-identity contract per scope.
    pub(crate) const fn word(&self) -> &'static str {
        match self {
            CacheScope::Subgraph => "subgraph",
            CacheScope::Connector => "connector",
        }
    }
}

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

    /// The user-facing string representation of this tag, used by the cache debugger to show
    /// which `@cacheTag` or extension-supplied tags a cached entry was indexed under. Returns
    /// `None` for internal entries.
    pub(crate) fn user_value(&self) -> Option<&str> {
        match self {
            CacheTag::Tag(s) => Some(s),
            _ => None,
        }
    }

    /// Render this entry as the Redis ZSET key it indexes into, scoped to `scope`. Subgraph and
    /// connector scopes render into disjoint namespaces (`subgraph-`/`connector-`) so the two can
    /// never collide on a shared Redis. For the subgraph scope the format is byte-identical to the
    /// pre-existing key shape, so existing cache data continues to map correctly.
    pub(crate) fn to_redis_key(&self, scope: CacheScope, name: &str) -> String {
        let scope = scope.word();
        match self {
            CacheTag::Subgraph => {
                format!("version:{RESPONSE_CACHE_VERSION}:cache-tag:{scope}-{name}")
            }
            CacheTag::Type(type_name) => {
                // The internal prefix preserves the historical key shape so old cache entries
                // and new ones share the same ZSET namespace.
                format!(
                    "version:{RESPONSE_CACHE_VERSION}:cache-tag:{scope}-{name}:key-{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:{scope}:{name}:type:{type_name}"
                )
            }
            CacheTag::Tag(value) => {
                format!("version:{RESPONSE_CACHE_VERSION}:cache-tag:{scope}-{name}:key-{value}")
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
    fn to_redis_key_subgraph_variant() {
        let key = CacheTag::Subgraph.to_redis_key(CacheScope::Subgraph, "payments");
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
        let key = CacheTag::Type("User".into()).to_redis_key(CacheScope::Subgraph, "payments");
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
        let key = CacheTag::Tag("homepage".into()).to_redis_key(CacheScope::Subgraph, "payments");
        assert!(
            key.ends_with(":subgraph-payments:key-homepage"),
            "tag key should end with :key-{{value}}: {key}"
        );
    }

    #[test]
    fn connector_scope_renders_disjoint_namespace() {
        // The whole-scope, type, and tag keys must all differ between scopes for the same name,
        // so a subgraph-kind invalidation can never resolve a connector source's index.
        let name = "graph.api";
        for tag in [
            CacheTag::Subgraph,
            CacheTag::Type("User".into()),
            CacheTag::Tag("homepage".into()),
        ] {
            let sub = tag.to_redis_key(CacheScope::Subgraph, name);
            let con = tag.to_redis_key(CacheScope::Connector, name);
            assert_ne!(sub, con, "scopes must render disjoint keys for {tag:?}");
            assert!(con.contains(":cache-tag:connector-graph.api"), "got {con}");
            assert!(!con.contains(":cache-tag:subgraph-"), "got {con}");
        }
        // The connector type key's internal doc segment must use the `connector:` word so it
        // matches the connector document-key prefix and the ConnectorType invalidation key.
        let con_type = CacheTag::Type("User".into()).to_redis_key(CacheScope::Connector, name);
        assert!(
            con_type.ends_with(":connector:graph.api:type:User"),
            "connector type key tail mismatch: {con_type}"
        );
    }
}
