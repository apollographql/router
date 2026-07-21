// TODO: move under invalidation.rs
// TODO: reconcile with Redis

use crate::{Context, plugins::response_cache::plugin::CdnInvalidationConfig};
use http::{HeaderName, HeaderValue};
use itertools::Itertools;
use std::collections::HashSet;
use thiserror::Error;

use crate::plugins::response_cache::INTERNAL_CACHE_TAG_PREFIX;
use crate::plugins::response_cache::metrics::CdnTagHeaderOutcome;
use crate::plugins::response_cache::metrics::record_cdn_tag_header_error;
use crate::plugins::response_cache::metrics::record_cdn_tag_header_outcome;
use crate::plugins::response_cache::metrics::record_cdn_tag_header_untruncated_size;

/// Per-request aggregator of cache tags surfaced by response_cache.
///
/// Populated as the router walks the resolution tree: each subgraph response — cache hit,
/// cache miss, or partial entity hit — contributes its tag set. The supergraph_service
/// `map_response` consumes the union to emit the configured cache-tag header when
/// `cdn_invalidation.enabled` is true.
///
/// Stored as a typed `Context` extension; the type itself is the key. Tags pushed through
/// `add_tags` (and `add_type`/`add_subgraph`'s extension-sourced callers) are expected to
/// already be filtered through `user_facing_only` upstream, at the point they're read for the
/// header, so internal `__apollo_internal::`-prefixed tags do not leak.
#[derive(Debug, Default, Clone)]
pub(crate) struct InvalidationLabels {
    /// Finest-grained tier: exact tag values from schema `@cacheTag` directives (interpolated
    /// per-request) and the `apolloCacheTags`/`apolloEntityCacheTags` subgraph-response
    /// extensions. Rendered as-is into the header; see `user_facing_only`.
    pub(crate) tags: HashSet<String>,
    /// Medium tier: `(subgraph, type name)` pairs touched by this request. Rendered as the
    /// coarser `type-{subgraph}-{type}` fallback label via `format_type_labels`.
    pub(crate) types: HashSet<(String, String)>,
    /// Coarsest tier: subgraph names touched by this request. Rendered as the
    /// `subgraph-{name}` fallback label via `format_subgraph_labels`.
    pub(crate) subgraphs: HashSet<String>,
    /// The `Context` this handle is tied to, so mutators can write back through to the same
    /// context-stored entry rather than a disconnected local copy. `None` only when an
    /// `InvalidationLabels` is constructed directly (e.g. `Default::default()`) instead of via
    /// `get_or_create` — mutators on such a handle return `InvalidationLabelsError::MissingContext`.
    pub(crate) context: Option<Context>,
}

impl InvalidationLabels {
    /// Fetches this context's `InvalidationLabels` entry, creating a default one first if none
    /// exists yet. Stamps `context` onto the stored entry (mirroring what the mutators below
    /// also do) so later reads and mutations of this context's entry stay tied to it regardless
    /// of which method first vivified it.
    ///
    /// Returns an owned clone of the stored entry rather than a reference: `with_lock` only
    /// lends out a `&mut` scoped to its closure, so a clone is the only way to hand a snapshot
    /// back to the caller. Cloning is cheap — `Context` is reference-counted internally, and the
    /// tag sets are typically small per request.
    pub(crate) fn get_or_create(context: &Context) -> Self {
        let invalidation_labels = context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            invalidation_labels.context = Some(context.clone());
            invalidation_labels.clone()
        });
        invalidation_labels
    }

    /// Renders each touched subgraph as its coarsest-tier fallback label (`subgraph-{name}`).
    fn format_subgraph_labels(&self) -> Vec<String> {
        self.subgraphs
            .iter()
            .map(|subgraph| format!("subgraph-{subgraph}"))
            .collect()
    }
    /// Renders each touched `(subgraph, type)` pair as its medium-tier fallback label
    /// (`type-{subgraph}-{type}`).
    fn format_type_labels(&self) -> Vec<String> {
        self.types
            .iter()
            .map(|(subgraph, r#type)| format!("type-{subgraph}-{type}"))
            .collect()
    }

    // WARN: this removes the sorting behavior
    fn build_header(&self, config: &CdnInvalidationConfig) -> Option<String> {
        let mut included: Vec<&str> = Vec::new();
        let mut current_len = 0usize;
        let header_delimiter_size = config.header_delimiter.len();

        // WARN: ordering here matters; we want coarsest labels first to make sure that we're
        // always able to invalidate whatever's cached. So, we start with subgraph labels, then
        // move on to {subgraph}-{type} labels, and then finally the user-set (via schema or
        // extension) tags labels
        // NOTE: this order is enforced through tests
        let subgraphs = self.format_subgraph_labels();
        let types = self.format_type_labels();
        let tags = self.tags.iter().cloned().collect_vec();
        let subgraph_types_tags_labels = vec![subgraphs, types, tags].concat();

        // WARN: don't remove this check; you might think that we always get the subgraph and
        // types even if we don't have the `@cacheTag` directive in the schema or user-sent
        // extensions, but for any subgraph where response caching isn't enabled, no CacheService
        // gets added and CacheService is the thing getting the subgraphs and types; so, it's more
        // than possible that we won't have subgraphs or types and will need to return None
        if subgraph_types_tags_labels.is_empty() {
            record_cdn_tag_header_outcome(CdnTagHeaderOutcome::Empty);
            tracing::debug!(
                "response_cache has no invalidation labels to emit for this response; skipping Cache-Tag header"
            );
            return None;
        }

        // Recorded regardless of whether truncation happens below, so the distribution tracks
        // headroom before truncation kicks in, not just the truncation events themselves.
        record_cdn_tag_header_untruncated_size(
            subgraph_types_tags_labels
                .join(&config.header_delimiter)
                .len() as u64,
        );

        for label in &subgraph_types_tags_labels {
            let next_len = current_len + header_delimiter_size + label.len();

            if next_len >= config.max_bytes {
                tracing::warn!(
                    "CDN invalidation labels header at capacity. This means you have more labels than can fit into the header."
                );
                break;
            }

            current_len = next_len;
            included.push(label.as_str());
        }

        let header = included.join(&config.header_delimiter);

        // WARN: don't remove this saturating sub; we're dealing with usizes, and we don't want
        // underflowing or overflowing-- guarded by tests, so should be safe, but this is where you
        // should look if you're suddenly dealing with insane numbers for dropped labels
        let dropped = subgraph_types_tags_labels
            .len()
            .saturating_sub(included.len());

        if dropped > 0 {
            tracing::warn!(
                max_bytes = %config.max_bytes,
                actual_bytes = %header.len(),
                dropped_count = %dropped,
                "response_cache cache-tag header exceeds max_bytes; truncated per on_overflow=truncate"
            );
        }

        if dropped > 0 && config.experimental_drop_on_overflow {
            record_cdn_tag_header_outcome(CdnTagHeaderOutcome::DroppedDueToOverflow);
            None
        } else {
            record_cdn_tag_header_outcome(if dropped > 0 {
                CdnTagHeaderOutcome::CompleteWithTruncation
            } else {
                CdnTagHeaderOutcome::CompleteWithoutTruncation
            });
            Some(header)
        }
    }

    pub(crate) fn maybe_emit_header(
        &self,
        headers: &mut http::HeaderMap,
        config: &CdnInvalidationConfig,
    ) {
        let header = if let Some(labels) = self.build_header(config) {
            labels
        } else {
            // `build_header` already recorded why (empty vs. dropped-due-to-overflow) and logged
            // accordingly, since it's the only place that knows which of the two applies here.
            return;
        };

        let header_name = match HeaderName::from_bytes(config.header_name.as_bytes()) {
            Ok(name) => name,
            Err(err) => {
                tracing::warn!(
                    header = %config.header_name,
                    error = %err,
                    "response_cache cdn_invalidation.header is not a valid HTTP header name; skipping emission"
                );

                record_cdn_tag_header_error("invalid_header_name");
                return;
            }
        };

        match HeaderValue::from_str(&header) {
            Ok(invalidation_labels) => {
                headers.insert(header_name, invalidation_labels);
                // Deliberately doesn't log the header value itself: it's built per response and
                // can be up to `max_bytes` (16kb by default), so logging it at debug level on
                // every response would be noisy and costly at volume.
                tracing::debug!("response_cache emitted aggregated cache-tag header");
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "response_cache aggregated cache-tag header value is not a valid HTTP header value; skipping emission"
                );
                record_cdn_tag_header_error("invalid_header_value");
            }
        }
    }

    /// Filters out any tag that starts with `INTERNAL_CACHE_TAG_PREFIX`, which denotes internal
    /// keys versus external. The difference between the two is that internal tags are not
    /// exposed to users; external are, and they're used for invalidation.
    ///
    /// The router's own cache debugger is the only current caller — it surfaces exactly the
    /// `@cacheTag`/extension values a customer set, not the router-computed `types`/`subgraphs`
    /// fallback tiers, which aren't user-authored and so have nothing to filter.
    ///
    /// Takes `&self`, so `self.tags` can only be read through a shared reference — `.cloned()`
    /// on the iterator is what produces owned `String`s here; there's no `self` (by value) to
    /// consume via `.into_iter()` instead.
    pub(crate) fn user_facing_only(&self) -> Vec<String> {
        // TODO: use all labels
        self.tags
            .iter()
            .cloned()
            .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
            .collect()
    }

    /// Unions `other_invalidatoin_labels`'s three tiers into this context's stored entry —
    /// used on a cache hit to merge a previously-stored `CacheEntry`'s `InvalidationLabels`
    /// (`other_invalidatoin_labels`) into the current request's aggregator, so labels recorded
    /// when the entry was first written still make it into this response's header.
    pub(crate) fn merge(
        &self,
        other_invalidatoin_labels: InvalidationLabels,
    ) -> Result<Self, InvalidationLabelsError> {
        let context = self
            .context
            .clone()
            .ok_or(InvalidationLabelsError::MissingContext(
                "Missing Context".to_string(),
            ))?;
        context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // make sure we don't lose context; it connects request/responses together
            invalidation_labels.context = Some(context.clone());

            invalidation_labels
                .tags
                .extend(other_invalidatoin_labels.tags);
            invalidation_labels
                .types
                .extend(other_invalidatoin_labels.types);
            invalidation_labels
                .subgraphs
                .extend(other_invalidatoin_labels.subgraphs);

            // fresh view of InvalidationLabels after writing to context
            Ok(invalidation_labels.clone())
        })
    }

    /// Unions `tags` into this context's stored `tags` tier — the finest-grained tier; see
    /// `InvalidationLabels::tags`.
    pub(crate) fn add_tags(&mut self, tags: Vec<String>) -> Result<Self, InvalidationLabelsError> {
        let context = self
            .context
            .clone()
            .ok_or(InvalidationLabelsError::MissingContext(
                "Missing Context".to_string(),
            ))?;
        context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // make sure we don't lose context; it connects request/responses together
            invalidation_labels.context = Some(context.clone());
            invalidation_labels.tags.extend(tags);
            Ok(invalidation_labels.clone())
        })
    }

    /// Records that `(subgraph, type)` was touched by this request, adding it to this context's
    /// stored `types` tier — the medium-coarseness fallback tier; see
    /// `InvalidationLabels::types`. Called unconditionally on every root-field or entity
    /// resolution (not gated on any invalidation index), since the type-tier fallback label
    /// must always be available regardless of indexing configuration.
    pub(crate) fn add_type(
        &mut self,
        subgraph: &str,
        r#type: &str,
    ) -> Result<Self, InvalidationLabelsError> {
        let context = self
            .context
            .clone()
            .ok_or(InvalidationLabelsError::MissingContext(
                "Missing Context".to_string(),
            ))?;
        context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // make sure we don't lose context; it connects request/responses together
            invalidation_labels.context = Some(context.clone());
            invalidation_labels
                .types
                .insert((subgraph.to_string(), r#type.to_string()));
            Ok(invalidation_labels.clone())
        })
    }

    /// Records that `subgraph` was touched by this request, adding it to this context's stored
    /// `subgraphs` tier — the coarsest fallback tier; see `InvalidationLabels::subgraphs`.
    /// Called unconditionally, same as `add_type`.
    pub(crate) fn add_subgraph(&mut self, subgraph: &str) -> Result<Self, InvalidationLabelsError> {
        let context = self
            .context
            .clone()
            .ok_or(InvalidationLabelsError::MissingContext(
                "Missing Context".to_string(),
            ))?;
        context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // make sure we don't lose context; it connects request/responses together
            invalidation_labels.context = Some(context.clone());
            invalidation_labels.subgraphs.insert(subgraph.to_string());
            Ok(invalidation_labels.clone())
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum InvalidationLabelsError {
    #[error("failed to record invalidation labels on context: {0}")]
    MissingContext(String),
}

/// Logs a failed invalidation-label write without panicking. Every call site is expected to go
/// through `get_or_create` first, which always populates `context`, so `MissingContext` should be
/// unreachable in practice — but invalidation-label bookkeeping is best-effort and must never
/// crash a request in flight, so failures are logged and swallowed rather than unwrapped.
pub(crate) fn log_invalidation_label_error(err: InvalidationLabelsError) {
    tracing::error!(
        error = %err,
        "response_cache failed to record an invalidation label on context; CDN/Redis invalidation may be incomplete for this response"
    );
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::metrics::FutureMetricsExt;

    fn cdn_config(overrides: impl FnOnce(&mut CdnInvalidationConfig)) -> CdnInvalidationConfig {
        let mut config = CdnInvalidationConfig::default();
        overrides(&mut config);
        config
    }

    // Constructs an InvalidationLabels handle tied to `context`, without seeding the
    // context's extensions map — used to exercise the "Missing InvalidationLabels on
    // Context" error path, distinct from the "Missing Context" path.
    fn unseeded_handle(context: &Context) -> InvalidationLabels {
        InvalidationLabels {
            context: Some(context.clone()),
            ..Default::default()
        }
    }

    #[test]
    fn get_or_create_returns_default_when_absent() {
        let context = Context::new();
        let labels = InvalidationLabels::get_or_create(&context);
        assert!(labels.tags.is_empty());
        assert!(labels.types.is_empty());
        assert!(labels.subgraphs.is_empty());
    }

    // `add_tags`/`add_type`/`add_subgraph` are meant to write through to the context-stored
    // value the same way, so their "does it persist" tests are the same shape three times
    // over: seed a context, mutate through one handle, and check a SEPARATE `get_or_create`
    // call sees the change (proving the write reached the context, not just a local copy).
    #[rstest]
    #[case::add_tags(
        (|labels: &mut InvalidationLabels| labels.add_tags(vec!["a".to_string(), "b".to_string()]).map(|_| ())) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(refetched.tags, HashSet::from(["a".to_string(), "b".to_string()]));
        }) as fn(&InvalidationLabels),
    )]
    #[case::add_type(
        (|labels: &mut InvalidationLabels| labels.add_type("accounts", "User").map(|_| ())) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(
                refetched.types,
                HashSet::from([("accounts".to_string(), "User".to_string())]),
                "add_type should write through to the context-stored value, the same way add_tags does"
            );
        }) as fn(&InvalidationLabels),
    )]
    #[case::add_subgraph(
        (|labels: &mut InvalidationLabels| labels.add_subgraph("accounts").map(|_| ())) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(
                refetched.subgraphs,
                HashSet::from(["accounts".to_string()]),
                "add_subgraph should write through to the context-stored value, the same way add_tags does"
            );
        }) as fn(&InvalidationLabels),
    )]
    fn mutator_persists_across_separate_get_or_create_calls(
        #[case] mutate: fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        #[case] check: fn(&InvalidationLabels),
    ) {
        let context = Context::new();
        let mut labels = InvalidationLabels::get_or_create(&context);

        mutate(&mut labels).expect("mutator should succeed against a seeded context");

        let refetched = InvalidationLabels::get_or_create(&context);
        check(&refetched);
    }

    #[test]
    fn add_tags_deduplicates_via_hashset_union() {
        let context = Context::new();
        let mut labels = InvalidationLabels::get_or_create(&context);

        labels.add_tags(vec!["a".to_string()]).unwrap();
        labels
            .add_tags(vec!["a".to_string(), "b".to_string()])
            .unwrap();

        let refetched = InvalidationLabels::get_or_create(&context);
        assert_eq!(
            refetched.tags,
            HashSet::from(["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn merge_unions_all_three_fields_into_context() {
        let context = Context::new();
        let labels = InvalidationLabels::get_or_create(&context);

        let other = InvalidationLabels {
            tags: HashSet::from(["homepage".to_string()]),
            types: HashSet::from([("accounts".to_string(), "User".to_string())]),
            subgraphs: HashSet::from(["accounts".to_string()]),
            context: None,
        };

        labels
            .merge(other)
            .expect("merge should succeed against a seeded context");

        let refetched = InvalidationLabels::get_or_create(&context);
        assert_eq!(refetched.tags, HashSet::from(["homepage".to_string()]));
        assert_eq!(
            refetched.types,
            HashSet::from([("accounts".to_string(), "User".to_string())])
        );
        assert_eq!(refetched.subgraphs, HashSet::from(["accounts".to_string()]));
    }

    #[test]
    fn mutators_without_context_return_missing_context_error() {
        let mut labels = InvalidationLabels::default();
        assert!(labels.context.is_none());

        assert!(matches!(
            labels.add_tags(vec!["a".to_string()]),
            Err(InvalidationLabelsError::MissingContext(_))
        ));
        assert!(matches!(
            labels.add_type("accounts", "User"),
            Err(InvalidationLabelsError::MissingContext(_))
        ));
        assert!(matches!(
            labels.add_subgraph("accounts"),
            Err(InvalidationLabelsError::MissingContext(_))
        ));
        assert!(matches!(
            labels.merge(InvalidationLabels::default()),
            Err(InvalidationLabelsError::MissingContext(_))
        ));
    }

    // `unseeded_handle` gives a handle whose context is seeded but whose context's extensions
    // map has no `InvalidationLabels` entry yet (i.e. `get_or_create` was never called first).
    // Mutators should auto-vivify that missing entry via `get_or_default_mut` rather than
    // erroring, mirroring `mutator_persists_across_separate_get_or_create_calls` above: mutate
    // through one handle, then check a SEPARATE `get_or_create` call sees the change.
    #[rstest]
    #[case::add_tags(
        (|labels: &mut InvalidationLabels| labels.add_tags(vec!["a".to_string(), "b".to_string()]).map(|_| ())) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(refetched.tags, HashSet::from(["a".to_string(), "b".to_string()]));
        }) as fn(&InvalidationLabels),
    )]
    #[case::add_type(
        (|labels: &mut InvalidationLabels| labels.add_type("accounts", "User").map(|_| ())) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(
                refetched.types,
                HashSet::from([("accounts".to_string(), "User".to_string())]),
            );
        }) as fn(&InvalidationLabels),
    )]
    #[case::add_subgraph(
        (|labels: &mut InvalidationLabels| labels.add_subgraph("accounts").map(|_| ())) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(
                refetched.subgraphs,
                HashSet::from(["accounts".to_string()]),
            );
        }) as fn(&InvalidationLabels),
    )]
    #[case::merge(
        (|labels: &mut InvalidationLabels| {
            let other = InvalidationLabels {
                tags: HashSet::from(["homepage".to_string()]),
                types: HashSet::from([("accounts".to_string(), "User".to_string())]),
                subgraphs: HashSet::from(["accounts".to_string()]),
                context: None,
            };
            labels.merge(other).map(|_| ())
        }) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(refetched.tags, HashSet::from(["homepage".to_string()]));
            assert_eq!(
                refetched.types,
                HashSet::from([("accounts".to_string(), "User".to_string())])
            );
            assert_eq!(refetched.subgraphs, HashSet::from(["accounts".to_string()]));
        }) as fn(&InvalidationLabels),
    )]
    fn mutators_auto_create_missing_context_entry(
        #[case] mutate: fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        #[case] check: fn(&InvalidationLabels),
    ) {
        let context = Context::new();
        let mut labels = unseeded_handle(&context);

        mutate(&mut labels).expect(
            "mutator should auto-create the missing InvalidationLabels entry instead of erroring",
        );

        let refetched = InvalidationLabels::get_or_create(&context);
        check(&refetched);
    }

    // A mutator, not `get_or_create`, is what first vivifies the entry here (via
    // `unseeded_handle`, same as above). If the mutator didn't also stamp `context` onto the
    // stored entry the way `get_or_create` does, a second independently-constructed handle
    // (again bypassing `get_or_create`) would hit `MissingContext` on the stored entry even
    // though the first call already proved the context was valid.
    #[test]
    fn mutator_stamps_context_so_a_later_mutator_call_does_not_need_get_or_create_first() {
        let context = Context::new();

        unseeded_handle(&context)
            .add_tags(vec!["a".to_string()])
            .expect("first mutator call should auto-create the entry");

        unseeded_handle(&context).add_subgraph("accounts").expect(
            "second mutator call should see the context stamped by the first, not MissingContext",
        );

        let refetched = InvalidationLabels::get_or_create(&context);
        assert_eq!(refetched.tags, HashSet::from(["a".to_string()]));
        assert_eq!(refetched.subgraphs, HashSet::from(["accounts".to_string()]));
    }

    #[rstest]
    #[case::filters_internal_prefixed_tags(
        HashSet::from([
            "homepage".to_string(),
            format!("{INTERNAL_CACHE_TAG_PREFIX}version:1:subgraph:accounts"),
        ]),
        vec!["homepage".to_string()],
    )]
    #[case::empty_when_no_user_facing_tags(
        HashSet::from([format!("{INTERNAL_CACHE_TAG_PREFIX}version:1:subgraph:accounts")]),
        vec![],
    )]
    fn user_facing_only_filters_by_internal_prefix(
        #[case] tags: HashSet<String>,
        #[case] expected: Vec<String>,
    ) {
        let labels = InvalidationLabels {
            tags,
            ..Default::default()
        };

        assert_eq!(labels.user_facing_only(), expected);
    }

    #[test]
    fn maybe_emit_header_skips_when_no_tags() {
        let labels = InvalidationLabels::default();
        let mut headers = http::HeaderMap::new();

        labels.maybe_emit_header(&mut headers, &cdn_config(|_| {}));

        assert!(headers.is_empty());
    }

    #[test]
    fn maybe_emit_header_inserts_single_tag_under_configured_header_name() {
        let labels = InvalidationLabels {
            tags: HashSet::from(["homepage".to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        labels.maybe_emit_header(
            &mut headers,
            &cdn_config(|c| c.header_name = "X-Cache-Tag".to_string()),
        );

        assert_eq!(headers.get("X-Cache-Tag").unwrap(), "homepage");
    }

    #[test]
    fn maybe_emit_header_joins_multiple_tags_with_configured_delimiter() {
        // Equal-length tags: the number that fit under max_bytes is independent of the
        // HashSet's arbitrary iteration order, so this stays deterministic.
        let labels = InvalidationLabels {
            tags: HashSet::from(["aaa".to_string(), "bbb".to_string(), "ccc".to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        labels.maybe_emit_header(
            &mut headers,
            &cdn_config(|c| c.header_delimiter = "|".to_string()),
        );

        let value = headers.get("Cache-Tag").unwrap().to_str().unwrap();
        let mut got: Vec<&str> = value.split('|').collect();
        got.sort_unstable();
        assert_eq!(got, vec!["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn maybe_emit_header_truncates_when_over_max_bytes() {
        // Four equal-length tags at 3 bytes + 1-byte delimiter each: the running total
        // after 1 tag is 4, after 2 is 8, after 3 is 12. With max_bytes=10, the loop
        // includes tags while next_len < 10 and breaks once it isn't, so exactly 2 fit
        // regardless of the HashSet's arbitrary iteration order (each tag costs the
        // same either way), while which two are chosen is not deterministic.
        let all: HashSet<String> = HashSet::from([
            "aaa".to_string(),
            "bbb".to_string(),
            "ccc".to_string(),
            "ddd".to_string(),
        ]);
        let labels = InvalidationLabels {
            tags: all.clone(),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        labels.maybe_emit_header(&mut headers, &cdn_config(|c| c.max_bytes = 10));

        let value = headers.get("Cache-Tag").unwrap().to_str().unwrap();
        let got: Vec<&str> = value.split(',').collect();
        assert_eq!(
            got.len(),
            2,
            "expected truncation to exactly 2 tags, got {value:?}"
        );
        for tag in &got {
            assert!(
                all.contains(*tag),
                "unexpected tag {tag} in truncated header"
            );
        }
    }

    #[test]
    fn maybe_emit_header_skips_on_invalid_header_name() {
        let labels = InvalidationLabels {
            tags: HashSet::from(["homepage".to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        // Space is not a legal header-name byte.
        labels.maybe_emit_header(
            &mut headers,
            &cdn_config(|c| c.header_name = "invalid header".to_string()),
        );

        assert!(headers.is_empty());
    }

    #[test]
    fn maybe_emit_header_skips_on_invalid_header_value() {
        // A CR/LF pair is not legal inside an HTTP header value, so the joined header string
        // fails `HeaderValue::from_str` even though every individual tag is a well-formed
        // `String` — this is the `invalid_header_value` sibling of
        // `maybe_emit_header_skips_on_invalid_header_name` above, which covers the header *name*
        // side of the same fallibility.
        let labels = InvalidationLabels {
            tags: HashSet::from(["bad\r\ntag".to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        labels.maybe_emit_header(&mut headers, &cdn_config(|_| {}));

        assert!(headers.is_empty());
    }

    #[rstest]
    #[case::empty(HashSet::new(), HashSet::new())]
    #[case::one_subgraph(
        HashSet::from(["products".to_string()]),
        HashSet::from(["subgraph-products".to_string()]),
    )]
    #[case::multiple_subgraphs(
        HashSet::from(["products".to_string(), "reviews".to_string()]),
        HashSet::from(["subgraph-products".to_string(), "subgraph-reviews".to_string()]),
    )]
    fn format_subgraph_labels_prefixes_each_subgraph(
        #[case] subgraphs: HashSet<String>,
        #[case] expected: HashSet<String>,
    ) {
        let labels = InvalidationLabels {
            subgraphs,
            ..Default::default()
        };

        let got: HashSet<String> = labels.format_subgraph_labels().into_iter().collect();
        assert_eq!(got, expected);
    }

    #[rstest]
    #[case::empty(HashSet::new(), HashSet::new())]
    #[case::one_type(
        HashSet::from([("products".to_string(), "Query".to_string())]),
        HashSet::from(["type-products-Query".to_string()]),
    )]
    #[case::multiple_types_same_subgraph(
        HashSet::from([
            ("products".to_string(), "Query".to_string()),
            ("products".to_string(), "Product".to_string()),
        ]),
        HashSet::from([
            "type-products-Query".to_string(),
            "type-products-Product".to_string(),
        ]),
    )]
    #[case::same_type_name_different_subgraphs(
        HashSet::from([
            ("reviews".to_string(), "Product".to_string()),
            ("pricing".to_string(), "Product".to_string()),
        ]),
        HashSet::from([
            "type-reviews-Product".to_string(),
            "type-pricing-Product".to_string(),
        ]),
    )]
    fn format_type_labels_renders_subgraph_and_type_distinctly(
        #[case] types: HashSet<(String, String)>,
        #[case] expected: HashSet<String>,
    ) {
        let labels = InvalidationLabels {
            types,
            ..Default::default()
        };

        let got: HashSet<String> = labels.format_type_labels().into_iter().collect();
        assert_eq!(got, expected);
    }

    #[rstest]
    #[case::all_empty(HashSet::new(), HashSet::new(), HashSet::new(), None)]
    #[case::only_subgraphs(
        HashSet::new(),
        HashSet::new(),
        HashSet::from(["products".to_string()]),
        Some("subgraph-products"),
    )]
    #[case::only_types(
        HashSet::new(),
        HashSet::from([("products".to_string(), "Query".to_string())]),
        HashSet::new(),
        Some("type-products-Query"),
    )]
    #[case::only_tags(
        HashSet::from(["homepage".to_string()]),
        HashSet::new(),
        HashSet::new(),
        Some("homepage"),
    )]
    fn maybe_emit_header_present_iff_any_tier_nonempty(
        #[case] tags: HashSet<String>,
        #[case] types: HashSet<(String, String)>,
        #[case] subgraphs: HashSet<String>,
        #[case] expected_content: Option<&str>,
    ) {
        let labels = InvalidationLabels {
            tags,
            types,
            subgraphs,
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        labels.maybe_emit_header(&mut headers, &cdn_config(|_| {}));

        match expected_content {
            None => assert!(
                headers.is_empty(),
                "expected no header when every tier is empty"
            ),
            Some(expected) => {
                assert_eq!(headers.get("Cache-Tag").unwrap(), expected);
            }
        }
    }

    #[test]
    fn maybe_emit_header_orders_subgraphs_then_types_then_tags() {
        let labels = InvalidationLabels {
            tags: HashSet::from(["homepage".to_string(), "checkout".to_string()]),
            types: HashSet::from([
                ("products".to_string(), "Query".to_string()),
                ("products".to_string(), "Product".to_string()),
            ]),
            subgraphs: HashSet::from(["products".to_string(), "reviews".to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        // Generous budget: nothing truncates, so this only tests ordering, not byte math
        // (that's covered separately by `maybe_emit_header_truncation_protects_coarse_tiers_first`).
        labels.maybe_emit_header(&mut headers, &cdn_config(|c| c.max_bytes = 1000));

        let value = headers.get("Cache-Tag").unwrap().to_str().unwrap();
        let segments: Vec<&str> = value.split(',').collect();

        // Neither fixture tag starts with "subgraph-"/"type-", so tier classification below is
        // unambiguous; keep that true if these fixture values ever change.
        let classify = |s: &str| {
            if s.starts_with("subgraph-") {
                0
            } else if s.starts_with("type-") {
                1
            } else {
                2
            }
        };

        let classes: Vec<i32> = segments.iter().map(|s| classify(s)).collect();
        let mut sorted = classes.clone();
        sorted.sort();
        assert_eq!(
            classes, sorted,
            "expected subgraph labels before type labels before tags, got {value:?}"
        );
        assert!(
            classes.contains(&0) && classes.contains(&1) && classes.contains(&2),
            "fixture should exercise all three tiers, got {value:?}"
        );
    }

    #[rstest]
    // All labels within a tier are deliberately the same rendered length (12 bytes for both
    // subgraph and type labels, 3 bytes for tags) so the number that fits at a given max_bytes
    // is exactly predictable regardless of the HashSet's arbitrary iteration order. Cumulative
    // next_len after each of the 8 items, in tier order, is: 13, 26, 39, 52, 56, 60, 64, 68.
    #[case::only_coarse_tiers_fit(54, 2, 2, 0)]
    #[case::coarse_tiers_plus_some_tags_fit(62, 2, 2, 2)]
    #[case::everything_fits(100, 2, 2, 4)]
    #[case::not_even_the_first_coarsest_label_fits(10, 0, 0, 0)]
    fn maybe_emit_header_truncation_protects_coarse_tiers_first(
        #[case] max_bytes: usize,
        #[case] expected_subgraphs: usize,
        #[case] expected_types: usize,
        #[case] expected_tags: usize,
    ) {
        let labels = InvalidationLabels {
            subgraphs: HashSet::from(["aaa".to_string(), "bbb".to_string()]),
            types: HashSet::from([
                ("ccc".to_string(), "ddd".to_string()),
                ("eee".to_string(), "fff".to_string()),
            ]),
            tags: HashSet::from([
                "ggg".to_string(),
                "hhh".to_string(),
                "iii".to_string(),
                "jjj".to_string(),
            ]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        labels.maybe_emit_header(&mut headers, &cdn_config(|c| c.max_bytes = max_bytes));

        let total_expected = expected_subgraphs + expected_types + expected_tags;
        if total_expected == 0 {
            // Budget too small even for the single coarsest label: the pre-truncation label set
            // wasn't empty, so this is an empty-*value* header, not a suppressed one — the
            // `None` path (see `maybe_emit_header_present_iff_any_tier_nonempty`) only fires
            // when there's nothing to consider at all, which isn't the case here.
            assert_eq!(headers.get("Cache-Tag").unwrap(), "");
            return;
        }

        let value = headers.get("Cache-Tag").unwrap().to_str().unwrap();
        let segments: Vec<&str> = value.split(',').collect();
        assert_eq!(
            segments.len(),
            total_expected,
            "unexpected segment count in {value:?}"
        );

        let subgraph_count = segments
            .iter()
            .filter(|s| s.starts_with("subgraph-"))
            .count();
        let type_count = segments.iter().filter(|s| s.starts_with("type-")).count();
        let tag_count = segments.len() - subgraph_count - type_count;

        assert_eq!(
            subgraph_count, expected_subgraphs,
            "subgraph count mismatch in {value:?}"
        );
        assert_eq!(
            type_count, expected_types,
            "type count mismatch in {value:?}"
        );
        assert_eq!(tag_count, expected_tags, "tag count mismatch in {value:?}");
    }

    #[rstest]
    // Same fixture and byte math as `maybe_emit_header_truncation_protects_coarse_tiers_first`
    // above (already-verified thresholds). With `experimental_drop_on_overflow` set, ANY
    // truncation — even just dropping fine-grained tags while every coarse subgraph/type label
    // still fit — should suppress the header entirely rather than emit the partial content. With
    // the flag left off, truncation behaves as before: a partial, non-suppressed header — the
    // last case cross-checks that against `only_coarse_tiers_fit` in the table above, which hits
    // the same max_bytes/fixture combination with the flag at its default.
    #[case::coarse_tiers_fit_but_tags_dropped_with_flag_on(54, true, None)]
    #[case::coarse_tiers_plus_some_tags_fit_but_some_dropped_with_flag_on(62, true, None)]
    #[case::everything_fits_nothing_dropped_with_flag_on(100, true, Some(8))]
    #[case::not_even_the_first_coarsest_label_fits_with_flag_on(10, true, None)]
    #[case::truncation_with_flag_off_stays_partial_not_suppressed(54, false, Some(4))]
    fn maybe_emit_header_experimental_drop_on_overflow_suppresses_truncated_header(
        #[case] max_bytes: usize,
        #[case] drop_on_overflow: bool,
        #[case] expected_segment_count: Option<usize>,
    ) {
        let labels = InvalidationLabels {
            subgraphs: HashSet::from(["aaa".to_string(), "bbb".to_string()]),
            types: HashSet::from([
                ("ccc".to_string(), "ddd".to_string()),
                ("eee".to_string(), "fff".to_string()),
            ]),
            tags: HashSet::from([
                "ggg".to_string(),
                "hhh".to_string(),
                "iii".to_string(),
                "jjj".to_string(),
            ]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        labels.maybe_emit_header(
            &mut headers,
            &cdn_config(|c| {
                c.max_bytes = max_bytes;
                c.experimental_drop_on_overflow = drop_on_overflow;
            }),
        );

        match expected_segment_count {
            None => assert!(
                headers.is_empty(),
                "max_bytes={max_bytes}, drop_on_overflow={drop_on_overflow}: expected header to be suppressed entirely, got {:?}",
                headers.get("Cache-Tag")
            ),
            Some(expected) => {
                let value = headers
                    .get("Cache-Tag")
                    .unwrap_or_else(|| {
                        panic!(
                            "max_bytes={max_bytes}, drop_on_overflow={drop_on_overflow}: expected header to still be present"
                        )
                    })
                    .to_str()
                    .unwrap();
                let segments: Vec<&str> = value.split(',').collect();
                assert_eq!(
                    segments.len(),
                    expected,
                    "unexpected segment count in {value:?}"
                );
            }
        }
    }

    // Every `maybe_emit_header`/`build_header` test above asserts on the resulting HTTP header;
    // none of them checks the telemetry `build_header` also records on every branch. These pin
    // down that the `cdn_tag_header.outcome` counter fires with the right label for each
    // possible outcome — the observable an SRE would actually alert on, distinct from (and
    // untested by) the header content itself.
    #[rstest]
    #[case::empty(InvalidationLabels::default(), 16384, false, "empty")]
    #[case::complete_without_truncation(
        InvalidationLabels { tags: HashSet::from(["homepage".to_string()]), ..Default::default() },
        16384,
        false,
        "complete_without_truncation",
    )]
    #[case::complete_with_truncation(
        InvalidationLabels {
            subgraphs: HashSet::from(["aaa".to_string(), "bbb".to_string()]),
            types: HashSet::from([("ccc".to_string(), "ddd".to_string()), ("eee".to_string(), "fff".to_string())]),
            tags: HashSet::from(["ggg".to_string(), "hhh".to_string(), "iii".to_string(), "jjj".to_string()]),
            ..Default::default()
        },
        54,
        false,
        "complete_with_truncation",
    )]
    #[case::dropped_due_to_overflow(
        InvalidationLabels {
            subgraphs: HashSet::from(["aaa".to_string(), "bbb".to_string()]),
            types: HashSet::from([("ccc".to_string(), "ddd".to_string()), ("eee".to_string(), "fff".to_string())]),
            tags: HashSet::from(["ggg".to_string(), "hhh".to_string(), "iii".to_string(), "jjj".to_string()]),
            ..Default::default()
        },
        54,
        true,
        "dropped_due_to_overflow",
    )]
    #[tokio::test]
    async fn maybe_emit_header_records_outcome_metric_per_branch(
        #[case] labels: InvalidationLabels,
        #[case] max_bytes: usize,
        #[case] drop_on_overflow: bool,
        #[case] expected_outcome: &'static str,
    ) {
        async move {
            let mut headers = http::HeaderMap::new();
            labels.maybe_emit_header(
                &mut headers,
                &cdn_config(|c| {
                    c.max_bytes = max_bytes;
                    c.experimental_drop_on_overflow = drop_on_overflow;
                }),
            );

            assert_counter!(
                "apollo.router.operations.response_cache.cdn_tag_header.outcome",
                1u64,
                "outcome" = expected_outcome
            );
        }
        .with_metrics()
        .await;
    }

    // The `Empty` outcome returns before `record_cdn_tag_header_untruncated_size` is ever
    // called, so it's the one case that must NOT record this histogram at all — covered as the
    // `None` case below alongside the two truncating/non-truncating cases that must.
    #[rstest]
    #[case::empty(InvalidationLabels::default(), None)]
    #[case::single_tag_no_truncation(
        InvalidationLabels { tags: HashSet::from(["homepage".to_string()]), ..Default::default() },
        // "homepage" is 8 bytes; join() with a single element adds no delimiter.
        Some(8u64),
    )]
    #[tokio::test]
    async fn maybe_emit_header_records_untruncated_size_histogram_iff_any_labels(
        #[case] labels: InvalidationLabels,
        #[case] expected_bytes: Option<u64>,
    ) {
        async move {
            let mut headers = http::HeaderMap::new();
            labels.maybe_emit_header(&mut headers, &cdn_config(|_| {}));

            match expected_bytes {
                Some(bytes) => {
                    assert_histogram_sum!(
                        "apollo.router.operations.response_cache.cdn_tag_header.untruncated_size_bytes",
                        bytes
                    );
                }
                None => {
                    assert_histogram_not_exists!(
                        "apollo.router.operations.response_cache.cdn_tag_header.untruncated_size_bytes",
                        u64
                    );
                }
            }
        }
        .with_metrics()
        .await;
    }

    // Records the untruncated size even when the header ends up truncated — this is what lets
    // the histogram answer "how much headroom do we have before truncation kicks in," not just
    // "how big was the final header."
    #[tokio::test]
    async fn maybe_emit_header_records_untruncated_size_even_when_truncated() {
        async move {
            let labels = InvalidationLabels {
                tags: HashSet::from(["aaaaaaaaaa".to_string(), "bbbbbbbbbb".to_string()]),
                ..Default::default()
            };
            let mut headers = http::HeaderMap::new();

            // Both 10-byte tags plus a 1-byte delimiter = 21 bytes untruncated; max_bytes=15
            // forces the second to be dropped.
            labels.maybe_emit_header(&mut headers, &cdn_config(|c| c.max_bytes = 15));

            assert_histogram_sum!(
                "apollo.router.operations.response_cache.cdn_tag_header.untruncated_size_bytes",
                21u64
            );
        }
        .with_metrics()
        .await;
    }

    #[rstest]
    #[case::invalid_header_name("invalid header", "homepage", "invalid_header_name")]
    #[case::invalid_header_value("Cache-Tag", "bad\r\ntag", "invalid_header_value")]
    #[tokio::test]
    async fn maybe_emit_header_records_error_metric_on_failure(
        #[case] header_name: &'static str,
        #[case] tag: &'static str,
        #[case] expected_reason: &'static str,
    ) {
        async move {
            let labels = InvalidationLabels {
                tags: HashSet::from([tag.to_string()]),
                ..Default::default()
            };
            let mut headers = http::HeaderMap::new();

            labels.maybe_emit_header(
                &mut headers,
                &cdn_config(|c| c.header_name = header_name.to_string()),
            );

            assert!(headers.is_empty());
            assert_counter!(
                "apollo.router.operations.response_cache.cdn_tag_header.error",
                1u64,
                "reason" = expected_reason
            );
        }
        .with_metrics()
        .await;
    }
}
