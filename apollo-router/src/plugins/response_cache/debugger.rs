use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

use crate::Context;
use crate::graphql;
use crate::json_ext::Object;
use crate::plugins::response_cache::cache_control::CacheControl;
use crate::plugins::response_cache::invalidation_endpoint::InvalidationIndexes;
use crate::plugins::response_cache::invalidation_labels::CdnHeaderBuildResult;
use crate::plugins::response_cache::plugin::CONTEXT_DEBUG_CACHE_KEYS;
use crate::plugins::response_cache::plugin::CONTEXT_DEBUG_CDN_INVALIDATION;

pub(super) type CacheKeysContext = Vec<CacheKeyContext>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CacheKeyContext {
    pub(super) key: String,
    pub(super) invalidation_keys: Vec<String>,
    /// Whether this entry has any *fine-grained* tags (schema `@cacheTag`/extension values), as
    /// opposed to only the always-present `subgraph`/`type` fallback labels also rendered into
    /// `invalidation_keys`. Not serialized — it exists purely so `compute_warnings` can tell "no
    /// fine-grained tags configured" apart from "no invalidation keys at all": `invalidation_keys`
    /// always includes the fallback labels, so its emptiness alone can't signal that.
    #[serde(skip)]
    pub(super) has_tags: bool,
    /// Whether `response_cache.cdn_invalidation.enabled` is set. Not serialized — like
    /// `has_tags`, it exists purely so `compute_warnings` can decide whether missing
    /// fine-grained tags is worth warning about: a subgraph with the Redis `cache_tag` index
    /// off but CDN invalidation on still needs `@cacheTag` for its `Cache-Tag` header to be
    /// anything more than coarse subgraph/type fallback labels.
    #[serde(skip)]
    pub(super) cdn_invalidation_enabled: bool,
    /// Invalidation indexes resolved for this entry's subgraph at write time. Surfacing this in
    /// the debugger lets operators see at a glance which indexes are active, which is essential
    /// when interpreting absent invalidation keys (e.g., the entry was written under
    /// `indexes.cache_tag: false`, so user-tag keys are intentionally absent rather than missing).
    pub(super) indexes: InvalidationIndexes,
    pub(super) kind: CacheEntryKind,
    pub(super) subgraph_name: String,
    pub(super) subgraph_request: graphql::Request,
    pub(super) source: CacheKeySource,
    pub(super) cache_control: CacheControl,
    pub(super) should_store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) hashed_private_id: Option<String>,
    pub(super) data: serde_json_bytes::Value,
    pub(super) warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Warning {
    pub(super) code: String,
    pub(super) links: Vec<Link>,
    pub(super) message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Link {
    pub(super) url: String,
    pub(super) title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq, Hash))]
#[serde(rename_all = "camelCase", untagged)]
pub(crate) enum CacheEntryKind {
    Entity {
        typename: String,
        #[serde(rename = "entityKey")]
        entity_key: Object,
    },
    RootFields {
        #[serde(rename = "rootFields")]
        root_fields: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq, Hash, PartialOrd, Ord))]
#[serde(rename_all = "camelCase")]
pub(crate) enum CacheKeySource {
    /// Data fetched from subgraph
    Subgraph,
    /// Data fetched from cache
    Cache,
    /// Data fetched from connector
    Connector,
}

/// Debug info for the CDN `Cache-Tag` header, once per response — the piece the per-entry
/// `CacheKeyContext`s can't show, since header truncation depends on the combined label set
/// across every entry the response touched, not any single one of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CdnInvalidationDebug {
    pub(super) header_name: String,
    pub(super) max_bytes: usize,
    /// One of `empty`, `complete_without_truncation`, `complete_with_truncation`,
    /// `dropped_due_to_overflow` — see `CdnTagHeaderOutcome`.
    pub(super) outcome: String,
    /// The header value that was built for this response, if any. Present even when `emitted`
    /// is `false` due to an invalid header name/value, so the debugger can still show what was
    /// attempted.
    pub(super) header_value: Option<String>,
    /// Size the header would have been without `max_bytes` truncation. `None` only when
    /// `outcome` is `empty`.
    pub(super) untruncated_size_bytes: Option<u64>,
    /// Whether the header actually made it onto the response. `false` can mean truncation
    /// suppressed it entirely (`dropped_due_to_overflow`), there was nothing to report
    /// (`empty`), or `header_name`/the built value isn't a legal HTTP header name/value.
    pub(super) emitted: bool,
}

impl CdnInvalidationDebug {
    pub(super) fn new(
        header_name: String,
        max_bytes: usize,
        result: &CdnHeaderBuildResult,
    ) -> Self {
        CdnInvalidationDebug {
            header_name,
            max_bytes,
            outcome: result.outcome.as_str().to_string(),
            header_value: result.header.clone(),
            untruncated_size_bytes: result.untruncated_size_bytes,
            emitted: result.emitted,
        }
    }
}

pub(super) fn add_cdn_invalidation_debug_to_context(
    context: &Context,
    debug: CdnInvalidationDebug,
) -> Result<(), BoxError> {
    context.insert(CONTEXT_DEBUG_CDN_INVALIDATION, debug)?;
    Ok(())
}

impl CacheKeyContext {
    fn compute_warnings(mut self) -> Self {
        let cache_control_mdn_docs: Link = Link {
            url: String::from(
                "https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Cache-Control",
            ),
            title: "Cache-Control header documentation".to_string(),
        };
        // Not cached because either no cache-control header set or no-store
        if self.cache_control.no_store() {
            self.warnings.push(Warning {
                code: "CACHE_CONTROL_NO_STORE".to_string(),
                links: vec![cache_control_mdn_docs.clone()],
                message: "Either the request or the subgraph response contained a Cache-Control header with no-store, so the data was not cached".to_string(),
            });
        }
        // Not cached because private in cache-control header and no private_id found in the context
        if self.cache_control.private() && self.hashed_private_id.is_none() {
            self.warnings.push(Warning {
                code: "CACHE_CONTROL_PRIVATE_WITHOUT_PRIVATE_ID".to_string(),
                links: vec![Link { url: String::from("https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/customization#private-data-caching"), title: "Configure private data caching in the Router".to_string() }, cache_control_mdn_docs.clone()],
                message: "The subgraph returned a 'Cache-Control' header containing private but you didn't provide a context entry to get the private data (token, username, ...) related to the current user.".to_string(),
            });
        }

        // TTL
        match self.cache_control.max_age() {
            Some(maxage) => {
                // Small maxage less than a minute
                if maxage < 60 {
                    self.warnings.push(Warning {
                        code: "CACHE_CONTROL_SMALL_MAX_AGE".to_string(),
                        links: vec![Link { url: String::from("https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/observability"), title: "Monitor with telemetry".to_string() }, cache_control_mdn_docs.clone()],
                        message: "The subgraph returned a 'Cache-Control' header with a small max-age (less than a minute) which could end up with less cache hits.".to_string(),
                    });
                }
                // Age header value bigger than max-age in cache-control header
                if let Some(age) = self.cache_control.age()
                    && maxage < age
                {
                    self.warnings.push(Warning {
                        code: "CACHE_CONTROL_MAX_AGE_SMALLER_AGE".to_string(),
                        links: vec![Link { url: String::from("https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Caching#fresh_and_stale_based_on_age"), title: "Fresh and stale data based on age".to_string() }, cache_control_mdn_docs.clone()],
                        message: "The subgraph returned a 'Cache-Control' header with a max-age smaller than the value of 'Age' header. This means the data has already expired, so the Router will not cache it.".to_string(),
                    });
                }
            }
            None => {
                // Only warn about missing max-age if no-store isn't set; if no-store is set,
                // the CACHE_CONTROL_NO_STORE warning above already covers the non-caching case.
                if !self.cache_control.no_store() {
                    // Default ttl
                    self.warnings.push(Warning {
                        code: "CACHE_CONTROL_WITHOUT_MAX_AGE".to_string(),
                        links: vec![Link { url: String::from("https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/invalidation#configure-default-ttl"), title: "Configure default TTL in the Router".to_string() }, cache_control_mdn_docs.clone()],
                        message: "The subgraph returned a 'Cache-Control' header without any max-age set, so the Router will use the default (configured in the Router configuration file).".to_string(),
                    });
                }
            }
        }
        if let CacheEntryKind::RootFields { root_fields } = &self.kind {
            // No cache tags on root fields. Only fire this when something would actually
            // consume per-tag values for this subgraph — the Redis cache_tag index or CDN
            // invalidation; otherwise the operator has intentionally opted out of both and
            // missing @cacheTag directives are the expected, configured state.
            if !self.has_tags && (self.indexes.cache_tag || self.cdn_invalidation_enabled) {
                self.warnings.push(Warning {
                    code: "NO_CACHE_TAG_ON_ROOT_FIELD".to_string(),
                    links: vec![Link { url: String::from("https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/invalidation#invalidation-methods"), title: "Add '@cacheTag' in your schema".to_string() }],
                    message: "No cache tags are specified on your root fields query. If you want to use active invalidation, you'll need to add cache tags on your root field.".to_string(),
                });
            }

            let root_fields_len = root_fields.len();
            // Several root fields cached in the same entry
            if root_fields_len > 1 {
                self.warnings.push(Warning {
                    code: "SEVERAL_ROOT_FIELDS".to_string(),
                    links: vec![Link { url: String::from("https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/faq#how-does-caching-work-for-operations-with-multiple-root-fields"), title: "Caching for operations with multiple root fields".to_string() }],
                    message: "The query contains several root field queries. These will be cached in the same cache entry per subgraph and will be invalidated together, regardless of whether you set separate cache tags on each root field.".to_string(),
                });
            }
        }

        self
    }

    fn compute_should_store(mut self) -> Self {
        self.should_store = self.cache_control.should_store();
        // If it's private data but we don't have a private id to add into the primary cache key we won't cache it
        if self.cache_control.private() && self.hashed_private_id.is_none() {
            self.should_store = false;
        }
        self
    }

    pub(super) fn update_metadata(self) -> Self {
        self.compute_warnings().compute_should_store()
    }
}

pub(super) fn add_cache_key_to_context(
    context: &Context,
    cache_key_context: CacheKeyContext,
) -> Result<(), BoxError> {
    context.upsert::<_, CacheKeysContext>(CONTEXT_DEBUG_CACHE_KEYS, |mut val| {
        val.push(cache_key_context);
        val
    })
}

pub(super) fn add_cache_keys_to_context<I: Iterator<Item = CacheKeyContext>>(
    context: &Context,
    cache_keys_context: I,
) -> Result<(), BoxError> {
    context.upsert::<_, CacheKeysContext>(CONTEXT_DEBUG_CACHE_KEYS, |mut val| {
        val.extend(cache_keys_context);
        val
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::rstest;

    use super::*;

    // A `CacheControl` with a generous max-age, so the only warning in play across these tests
    // is `NO_CACHE_TAG_ON_ROOT_FIELD` — no `CACHE_CONTROL_*` warnings firing alongside it.
    fn clean_cache_control() -> CacheControl {
        CacheControl::default().with_default_ttl(Some(Duration::from_secs(90)))
    }

    fn root_fields_context(
        has_tags: bool,
        cache_tag_index: bool,
        cdn_invalidation_enabled: bool,
    ) -> CacheKeyContext {
        CacheKeyContext {
            key: "test-key".to_string(),
            invalidation_keys: vec![],
            has_tags,
            cdn_invalidation_enabled,
            indexes: InvalidationIndexes {
                cache_tag: cache_tag_index,
                ..Default::default()
            },
            kind: CacheEntryKind::RootFields {
                root_fields: vec!["currentUser".to_string()],
            },
            subgraph_name: "user".to_string(),
            subgraph_request: graphql::Request::default(),
            source: CacheKeySource::Subgraph,
            cache_control: clean_cache_control(),
            should_store: false,
            hashed_private_id: None,
            data: serde_json_bytes::Value::default(),
            warnings: Vec::new(),
        }
    }

    fn has_warning(ctx: &CacheKeyContext, code: &str) -> bool {
        ctx.warnings.iter().any(|w| w.code == code)
    }

    #[rstest]
    // No fine-grained tags: the warning should fire whenever *either* consumer (the Redis
    // cache_tag index or CDN invalidation) would actually use per-tag values for this subgraph.
    #[case::neither_consumer_enabled(false, false, false, false)]
    #[case::only_cache_tag_index_enabled(false, true, false, true)]
    #[case::only_cdn_invalidation_enabled(false, false, true, true)]
    #[case::both_enabled(false, true, true, true)]
    // Has fine-grained tags: never warn, regardless of which consumers are enabled.
    #[case::has_tags_neither_enabled(true, false, false, false)]
    #[case::has_tags_cache_tag_index_enabled(true, true, false, false)]
    #[case::has_tags_cdn_invalidation_enabled(true, false, true, false)]
    #[case::has_tags_both_enabled(true, true, true, false)]
    fn no_cache_tag_on_root_field_fires_iff_either_consumer_wants_tags(
        #[case] has_tags: bool,
        #[case] cache_tag_index: bool,
        #[case] cdn_invalidation_enabled: bool,
        #[case] expect_warning: bool,
    ) {
        let ctx = root_fields_context(has_tags, cache_tag_index, cdn_invalidation_enabled)
            .compute_warnings();

        assert_eq!(
            has_warning(&ctx, "NO_CACHE_TAG_ON_ROOT_FIELD"),
            expect_warning,
            "has_tags={has_tags}, cache_tag_index={cache_tag_index}, cdn_invalidation_enabled={cdn_invalidation_enabled}: warnings were {:?}",
            ctx.warnings.iter().map(|w| &w.code).collect::<Vec<_>>()
        );
    }

    #[test]
    fn no_cache_tag_on_root_field_never_fires_for_entities() {
        let mut ctx = root_fields_context(false, true, true);
        ctx.kind = CacheEntryKind::Entity {
            typename: "User".to_string(),
            entity_key: Object::default(),
        };

        let ctx = ctx.compute_warnings();

        assert!(
            !has_warning(&ctx, "NO_CACHE_TAG_ON_ROOT_FIELD"),
            "entities should never get the root-field-only warning, got {:?}",
            ctx.warnings.iter().map(|w| &w.code).collect::<Vec<_>>()
        );
    }
}
