// TODO: move under invalidation.rs
// TODO: reconcile with Redis

use std::collections::HashSet;

use http::HeaderName;
use http::HeaderValue;
use itertools::Itertools;
use thiserror::Error;

use crate::Context;
use crate::plugins::response_cache::INTERNAL_CACHE_TAG_PREFIX;
use crate::plugins::response_cache::metrics::CdnTagHeaderOutcome;
use crate::plugins::response_cache::metrics::record_cdn_tag_header_error;
use crate::plugins::response_cache::metrics::record_cdn_tag_header_outcome;
use crate::plugins::response_cache::metrics::record_cdn_tag_header_untruncated_size;
use crate::plugins::response_cache::plugin::CdnInvalidationConfig;

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

/// What happened when building (and attempting to emit) the CDN `Cache-Tag` header for one
/// response. Returned by `maybe_emit_header` so the cache debugger can show *why* a header was,
/// or wasn't, sent — information the debugger has no other way to see, since it otherwise only
/// knows about individual cache entries, not the whole-response header-building step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CdnHeaderBuildResult {
    /// The header value that was built, if any label existed to build one from. `None` for
    /// `Empty` and `DroppedDueToOverflow` outcomes.
    pub(super) header: Option<String>,
    pub(super) outcome: CdnTagHeaderOutcome,
    /// Size the header would have been without `max_bytes` truncation. `None` only for the
    /// `Empty` outcome, since there's nothing to measure.
    pub(super) untruncated_size_bytes: Option<u64>,
    /// Whether the header was actually inserted into the response. Can be `false` even when
    /// `header` is `Some`: `config.header_name`/the built value might not be a legal HTTP
    /// header name/value, which is a separate failure mode from `outcome`.
    pub(super) emitted: bool,
}

impl InvalidationLabels {
    /// Fetches this context's `InvalidationLabels` entry, creating a default one first if none
    /// exists yet, and stamps `context` onto the *returned* handle so later mutator calls on it
    /// don't need another `get_or_create` first.
    ///
    /// Deliberately never writes `context` onto the copy stored in the extensions map: `Context`
    /// holds a strong `Arc` to the very extensions map its entries live in, so an
    /// `InvalidationLabels` stored *inside* that map with its own `context` field pointing back
    /// to the same `Context` would be a self-referential `Arc` cycle — the map's strong count
    /// would never reach zero, leaking the whole per-request extensions map (every plugin's
    /// entries in it, not just this one) for the life of the process. Only ever stamping the
    /// value handed back to the caller (which drops like any other owned value) avoids that.
    ///
    /// Returns an owned clone of the stored entry rather than a reference: `with_lock` only
    /// lends out a `&mut` scoped to its closure, so a clone is the only way to hand a snapshot
    /// back to the caller. Cloning is cheap — `Context` is reference-counted internally, and the
    /// tag sets are typically small per request.
    pub(crate) fn get_or_create(context: &Context) -> Self {
        let mut invalidation_labels = context
            .extensions()
            .with_lock(|lock| lock.get_or_default_mut::<InvalidationLabels>().clone());
        invalidation_labels.context = Some(context.clone());
        invalidation_labels
    }

    /// Renders each touched subgraph as its coarsest-tier fallback label (`subgraph-{name}`),
    /// sorted so which labels get dropped under truncation is deterministic across replicas
    /// instead of depending on `HashSet`'s per-process iteration order.
    fn format_subgraph_labels(&self) -> Vec<String> {
        let mut labels: Vec<String> = self
            .subgraphs
            .iter()
            .map(|subgraph| format!("subgraph-{subgraph}"))
            .collect();
        labels.sort_unstable();
        labels
    }
    /// Renders each touched `(subgraph, type)` pair as its medium-tier fallback label
    /// (`type-{subgraph}-{type}`), sorted for the same reason as `format_subgraph_labels`.
    fn format_type_labels(&self) -> Vec<String> {
        let mut labels: Vec<String> = self
            .types
            .iter()
            .map(|(subgraph, r#type)| format!("type-{subgraph}-{type}"))
            .collect();
        labels.sort_unstable();
        labels
    }

    fn build_header(&self, config: &CdnInvalidationConfig) -> CdnHeaderBuildResult {
        let mut included: Vec<&str> = Vec::new();
        let mut current_len = 0usize;
        let header_delimiter_size = config.header_delimiter.len();

        // Ordering here matters; we want coarsest labels first to make sure that we're
        // always able to invalidate whatever's cached. So, we start with subgraph labels, then
        // move on to {subgraph}-{type} labels, and then finally the user-set (via schema or
        // extension) tags labels
        // NOTE: this order is enforced through tests
        let mut subgraphs = self.format_subgraph_labels();
        let mut types = self.format_type_labels();
        let mut tags = self.tags.iter().cloned().collect_vec();
        tags.sort_unstable();

        // A label containing the configured delimiter would be indistinguishable, once joined,
        // from two separate labels — ambiguous for any CDN splitting the header on that
        // delimiter. Drop such labels rather than emit a header that silently means something
        // different than what it looks like. `subgraph-`/`type-` labels are router-computed and
        // can't contain arbitrary bytes, so in practice this only ever affects user-authored
        // `@cacheTag`/extension tag values.
        let delimiter = config.header_delimiter.as_str();
        let mut dropped_for_delimiter_collision = 0usize;
        for labels in [&mut subgraphs, &mut types, &mut tags] {
            let before = labels.len();
            labels.retain(|label| delimiter.is_empty() || !label.contains(delimiter));
            dropped_for_delimiter_collision += before - labels.len();
        }
        if dropped_for_delimiter_collision > 0 {
            tracing::warn!(
                count = dropped_for_delimiter_collision,
                "response_cache: dropped cache-tag label(s) containing the configured \
                 header_delimiter; they can't be safely represented in the Cache-Tag header"
            );
            record_cdn_tag_header_error("label_contains_delimiter");
        }

        let subgraph_types_tags_labels = [subgraphs, types, tags].concat();

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
            return CdnHeaderBuildResult {
                header: None,
                outcome: CdnTagHeaderOutcome::Empty,
                untruncated_size_bytes: None,
                emitted: false,
            };
        }

        // Recorded regardless of whether truncation happens below, so the distribution tracks
        // headroom before truncation kicks in, not just the truncation events themselves.
        let untruncated_size_bytes = subgraph_types_tags_labels
            .join(&config.header_delimiter)
            .len() as u64;
        record_cdn_tag_header_untruncated_size(untruncated_size_bytes);

        for label in &subgraph_types_tags_labels {
            // A delimiter is only needed before this label if something's already been
            // included — `included.join(delimiter)` has one fewer delimiter than label, and
            // this accounting has to match that or it overcounts the real joined length by one
            // delimiter's worth per response, truncating labels that would actually have fit.
            let delimiter_cost = if included.is_empty() {
                0
            } else {
                header_delimiter_size
            };
            let next_len = current_len + delimiter_cost + label.len();

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
            CdnHeaderBuildResult {
                header: None,
                outcome: CdnTagHeaderOutcome::DroppedDueToOverflow,
                untruncated_size_bytes: Some(untruncated_size_bytes),
                emitted: false,
            }
        } else {
            let outcome = if dropped > 0 {
                CdnTagHeaderOutcome::CompleteWithTruncation
            } else {
                CdnTagHeaderOutcome::CompleteWithoutTruncation
            };
            record_cdn_tag_header_outcome(outcome);
            CdnHeaderBuildResult {
                header: Some(header),
                outcome,
                untruncated_size_bytes: Some(untruncated_size_bytes),
                emitted: false,
            }
        }
    }

    /// Builds the CDN `Cache-Tag` header and inserts it into `headers` if enabled/non-empty,
    /// returning what happened so callers (namely the cache debugger, in `debug` mode) can show
    /// why a header was, or wasn't, emitted for a given response — including cases the debugger
    /// can't otherwise see, like truncation or a config-level emission failure.
    pub(super) fn maybe_emit_header(
        &self,
        headers: &mut http::HeaderMap,
        config: &CdnInvalidationConfig,
    ) -> CdnHeaderBuildResult {
        let mut result = self.build_header(config);
        let header = match &result.header {
            // `build_header` already recorded why (empty vs. dropped-due-to-overflow) and logged
            // accordingly, since it's the only place that knows which of the two applies here.
            None => return result,
            Some(header) => header.clone(),
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
                result.emitted = false;
                return result;
            }
        };

        match HeaderValue::from_str(&header) {
            Ok(invalidation_labels) => {
                headers.insert(header_name, invalidation_labels);
                // Deliberately doesn't log the header value itself: it's built per response and
                // can be up to `max_bytes` (16kb by default), so logging it at debug level on
                // every response would be noisy and costly at volume.
                tracing::debug!("response_cache emitted aggregated cache-tag header");
                result.emitted = true;
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "response_cache aggregated cache-tag header value is not a valid HTTP header value; skipping emission"
                );
                record_cdn_tag_header_error("invalid_header_value");
                result.emitted = false;
            }
        }

        result
    }

    /// Returns every label a customer could use to purge or invalidate this entry — the same
    /// three tiers rendered into the CDN `Cache-Tag` header (`subgraph-{name}`,
    /// `type-{subgraph}-{type}`, and exact tag values), minus any tag that starts with
    /// `INTERNAL_CACHE_TAG_PREFIX`. Internal tags are router bookkeeping, never exposed to
    /// users; only `tags` can carry them, since `types`/`subgraphs` are router-computed rather
    /// than user-authored and so have nothing to filter.
    ///
    /// The router's own cache debugger is the only current caller: it surfaces these strings
    /// directly so a customer can see exactly what they'd need to paste into a CDN purge call
    /// or a Redis `/invalidation` request for a given entry, rather than having to reconstruct
    /// the label format themselves from the debugger's separate structured subgraph/type
    /// fields.
    pub(crate) fn user_facing_only(&self) -> Vec<String> {
        let mut labels = self.format_subgraph_labels();
        labels.extend(self.format_type_labels());
        let mut tags: Vec<String> = self
            .tags
            .iter()
            .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
            .cloned()
            .collect();
        tags.sort_unstable();
        labels.extend(tags);
        labels
    }

    /// Unions `other_invalidatoin_labels`'s three tiers into this context's stored entry —
    /// used on a cache hit to merge a previously-stored `CacheEntry`'s `InvalidationLabels`
    /// (`other_invalidatoin_labels`) into the current request's aggregator, so labels recorded
    /// when the entry was first written still make it into this response's header.
    pub(crate) fn merge(
        &self,
        other_invalidatoin_labels: InvalidationLabels,
    ) -> Result<(), InvalidationLabelsError> {
        let context = self
            .context
            .clone()
            .ok_or(InvalidationLabelsError::MissingContext(
                "Missing Context".to_string(),
            ))?;
        context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // Deliberately never set `.context` on the stored entry here — see
            // `get_or_create`'s doc comment for why that would leak the extensions map.
            invalidation_labels
                .tags
                .extend(other_invalidatoin_labels.tags);
            invalidation_labels
                .types
                .extend(other_invalidatoin_labels.types);
            invalidation_labels
                .subgraphs
                .extend(other_invalidatoin_labels.subgraphs);
        });
        Ok(())
    }

    /// Unions `tags` into this context's stored `tags` tier — the finest-grained tier; see
    /// `InvalidationLabels::tags`.
    pub(crate) fn add_tags(&mut self, tags: Vec<String>) -> Result<(), InvalidationLabelsError> {
        let context = self
            .context
            .clone()
            .ok_or(InvalidationLabelsError::MissingContext(
                "Missing Context".to_string(),
            ))?;
        context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // Deliberately never set `.context` on the stored entry here — see
            // `get_or_create`'s doc comment for why that would leak the extensions map.
            invalidation_labels.tags.extend(tags);
        });
        Ok(())
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
    ) -> Result<(), InvalidationLabelsError> {
        let context = self
            .context
            .clone()
            .ok_or(InvalidationLabelsError::MissingContext(
                "Missing Context".to_string(),
            ))?;
        context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // Deliberately never set `.context` on the stored entry here — see
            // `get_or_create`'s doc comment for why that would leak the extensions map.
            invalidation_labels
                .types
                .insert((subgraph.to_string(), r#type.to_string()));
        });
        Ok(())
    }

    /// Records that `subgraph` was touched by this request, adding it to this context's stored
    /// `subgraphs` tier — the coarsest fallback tier; see `InvalidationLabels::subgraphs`.
    /// Called unconditionally, same as `add_type`.
    pub(crate) fn add_subgraph(&mut self, subgraph: &str) -> Result<(), InvalidationLabelsError> {
        let context = self
            .context
            .clone()
            .ok_or(InvalidationLabelsError::MissingContext(
                "Missing Context".to_string(),
            ))?;
        context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // Deliberately never set `.context` on the stored entry here — see
            // `get_or_create`'s doc comment for why that would leak the extensions map.
            invalidation_labels.subgraphs.insert(subgraph.to_string());
        });
        Ok(())
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

    // Constructs an InvalidationLabels handle tied to `context`, without seeding the context's
    // extensions map — used to exercise mutators creating that missing entry themselves (see
    // the tests below), as opposed to `get_or_create`, which seeds the entry itself.
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

    /// Regression test: `get_or_create`/the mutators used to stamp `context: Some(context.clone())`
    /// onto the *stored* `InvalidationLabels` entry, not just the handle returned to the caller.
    /// Since `Context` holds a strong `Arc` to the very extensions map its entries live in, that
    /// created a self-referential `Arc` cycle — the map's strong count would never reach zero, so
    /// nothing stored in it (from this or any other plugin) was ever freed. This inserts a
    /// drop-flagging marker into the same extensions map `InvalidationLabels` lives in, exercises
    /// every mutator, then drops every external handle and asserts the whole map actually got
    /// freed — which only happens if `InvalidationLabels` isn't holding a `Context` back-reference
    /// inside the map itself.
    #[test]
    fn context_extensions_do_not_leak_via_self_referential_invalidation_labels() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::Ordering;

        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let context = Context::new();
        context.extensions().with_lock(|lock| {
            lock.insert(DropFlag(dropped.clone()));
        });

        let mut labels = InvalidationLabels::get_or_create(&context);
        labels.add_tags(vec!["a".to_string()]).unwrap();
        labels.add_subgraph("accounts").unwrap();
        labels.add_type("accounts", "User").unwrap();
        labels
            .merge(InvalidationLabels {
                tags: HashSet::from(["b".to_string()]),
                ..Default::default()
            })
            .unwrap();

        drop(labels);
        drop(context);

        assert!(
            dropped.load(Ordering::SeqCst),
            "extensions map (and everything stored in it) should be freed once every external \
             Context handle drops; if this fails, InvalidationLabels is holding a self-referential \
             Context that leaks the whole map"
        );
    }

    // `add_tags`/`add_type`/`add_subgraph` are meant to write through to the context-stored
    // value the same way, so their "does it persist" tests are the same shape three times
    // over: seed a context, mutate through one handle, and check a SEPARATE `get_or_create`
    // call sees the change (proving the write reached the context, not just a local copy).
    #[rstest]
    #[case::add_tags(
        (|labels: &mut InvalidationLabels| labels.add_tags(vec!["a".to_string(), "b".to_string()])) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(refetched.tags, HashSet::from(["a".to_string(), "b".to_string()]));
        }) as fn(&InvalidationLabels),
    )]
    #[case::add_type(
        (|labels: &mut InvalidationLabels| labels.add_type("accounts", "User")) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(
                refetched.types,
                HashSet::from([("accounts".to_string(), "User".to_string())]),
                "add_type should write through to the context-stored value, the same way add_tags does"
            );
        }) as fn(&InvalidationLabels),
    )]
    #[case::add_subgraph(
        (|labels: &mut InvalidationLabels| labels.add_subgraph("accounts")) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
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
    // Mutators should create that missing entry via `get_or_default_mut` rather than erroring,
    // mirroring `mutator_persists_across_separate_get_or_create_calls` above: mutate through one
    // handle, then check a SEPARATE `get_or_create` call sees the change.
    #[rstest]
    #[case::add_tags(
        (|labels: &mut InvalidationLabels| labels.add_tags(vec!["a".to_string(), "b".to_string()])) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(refetched.tags, HashSet::from(["a".to_string(), "b".to_string()]));
        }) as fn(&InvalidationLabels),
    )]
    #[case::add_type(
        (|labels: &mut InvalidationLabels| labels.add_type("accounts", "User")) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
        (|refetched: &InvalidationLabels| {
            assert_eq!(
                refetched.types,
                HashSet::from([("accounts".to_string(), "User".to_string())]),
            );
        }) as fn(&InvalidationLabels),
    )]
    #[case::add_subgraph(
        (|labels: &mut InvalidationLabels| labels.add_subgraph("accounts")) as fn(&mut InvalidationLabels) -> Result<(), InvalidationLabelsError>,
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
            labels.merge(other)
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

    // A mutator, not `get_or_create`, is what first creates the entry here (via
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

    #[rstest]
    #[case::subgraph_tier_only(
        HashSet::new(),
        HashSet::new(),
        HashSet::from(["accounts".to_string()]),
        HashSet::from(["subgraph-accounts".to_string()]),
    )]
    #[case::type_tier_only(
        HashSet::new(),
        HashSet::from([("accounts".to_string(), "User".to_string())]),
        HashSet::new(),
        HashSet::from(["type-accounts-User".to_string()]),
    )]
    #[case::all_three_tiers_together(
        HashSet::from(["homepage".to_string()]),
        HashSet::from([("accounts".to_string(), "User".to_string())]),
        HashSet::from(["accounts".to_string()]),
        HashSet::from([
            "homepage".to_string(),
            "type-accounts-User".to_string(),
            "subgraph-accounts".to_string(),
        ]),
    )]
    #[case::internal_tag_filtered_but_type_and_subgraph_kept(
        HashSet::from([
            format!("{INTERNAL_CACHE_TAG_PREFIX}version:1:subgraph:accounts"),
            "homepage".to_string(),
        ]),
        HashSet::from([("accounts".to_string(), "User".to_string())]),
        HashSet::from(["accounts".to_string()]),
        HashSet::from([
            "homepage".to_string(),
            "type-accounts-User".to_string(),
            "subgraph-accounts".to_string(),
        ]),
    )]
    fn user_facing_only_includes_type_and_subgraph_tiers(
        #[case] tags: HashSet<String>,
        #[case] types: HashSet<(String, String)>,
        #[case] subgraphs: HashSet<String>,
        #[case] expected: HashSet<String>,
    ) {
        let labels = InvalidationLabels {
            tags,
            types,
            subgraphs,
            ..Default::default()
        };

        let got: HashSet<String> = labels.user_facing_only().into_iter().collect();
        assert_eq!(got, expected);
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

    #[tokio::test]
    async fn maybe_emit_header_drops_tags_containing_the_delimiter() {
        // A tag value containing the configured delimiter would be indistinguishable, once
        // joined, from two separate tags — it must be dropped rather than corrupt the header.
        async move {
            let labels = InvalidationLabels {
                tags: HashSet::from(["region,west".to_string(), "safe-tag".to_string()]),
                ..Default::default()
            };
            let mut headers = http::HeaderMap::new();

            labels.maybe_emit_header(&mut headers, &cdn_config(|_| {}));

            let value = headers.get("Cache-Tag").unwrap().to_str().unwrap();
            assert_eq!(value, "safe-tag");
            assert_counter!(
                "apollo.router.operations.response_cache.cdn_tag_header.error",
                1u64,
                "reason" = "label_contains_delimiter"
            );
        }
        .with_metrics()
        .await;
    }

    #[test]
    fn maybe_emit_header_reports_empty_when_every_label_collides_with_the_delimiter() {
        let labels = InvalidationLabels {
            tags: HashSet::from(["a,b".to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        let result = labels.maybe_emit_header(&mut headers, &cdn_config(|_| {}));

        assert_eq!(result.outcome, CdnTagHeaderOutcome::Empty);
        assert!(headers.is_empty());
    }

    #[test]
    fn maybe_emit_header_truncates_when_over_max_bytes() {
        // Four equal-length 3-byte tags, joined with a 1-byte delimiter: the running total
        // (matching real join() length) is 3 after 1 tag, 7 after 2, 11 after 3. With
        // max_bytes=10, the loop includes tags while next_len < 10 and breaks once it isn't,
        // so exactly 2 fit regardless of the HashSet's arbitrary iteration order (each tag
        // costs the same either way), while which two are chosen is not deterministic.
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
    fn maybe_emit_header_orders_labels_within_a_tier_deterministically() {
        // Regression test: labels within a tier used to come from unsorted `HashSet` iteration,
        // so which labels a truncated header kept was non-deterministic across processes/
        // replicas for identical content. Building the same header repeatedly must always
        // produce the exact same string now that each tier is sorted before joining.
        let labels = InvalidationLabels {
            tags: HashSet::from([
                "zzz".to_string(),
                "aaa".to_string(),
                "mmm".to_string(),
                "ccc".to_string(),
            ]),
            types: HashSet::from([
                ("zsub".to_string(), "ZType".to_string()),
                ("asub".to_string(), "AType".to_string()),
            ]),
            subgraphs: HashSet::from(["zzz-subgraph".to_string(), "aaa-subgraph".to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();
        labels.maybe_emit_header(&mut headers, &cdn_config(|c| c.max_bytes = 1000));
        let first = headers
            .get("Cache-Tag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        for _ in 0..5 {
            let mut headers = http::HeaderMap::new();
            labels.maybe_emit_header(&mut headers, &cdn_config(|c| c.max_bytes = 1000));
            let value = headers.get("Cache-Tag").unwrap().to_str().unwrap();
            assert_eq!(value, first, "header ordering must be deterministic");
        }

        assert_eq!(
            first,
            "subgraph-aaa-subgraph,subgraph-zzz-subgraph,type-asub-AType,type-zsub-ZType,aaa,ccc,mmm,zzz"
        );
    }

    #[test]
    fn maybe_emit_header_fits_labels_up_to_the_real_joined_length() {
        // "aa,bb" is exactly 5 bytes; with max_bytes=6 both tags should fit. A regression test
        // for an earlier bug where the truncation loop counted a delimiter before every label
        // (including the first) instead of only between labels, overcounting the true joined
        // length by one delimiter and truncating labels that actually fit.
        let labels = InvalidationLabels {
            tags: HashSet::from(["aa".to_string(), "bb".to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        let result = labels.maybe_emit_header(&mut headers, &cdn_config(|c| c.max_bytes = 6));

        assert_eq!(
            result.outcome,
            CdnTagHeaderOutcome::CompleteWithoutTruncation
        );
        let value = headers.get("Cache-Tag").unwrap().to_str().unwrap();
        let mut got: Vec<&str> = value.split(',').collect();
        got.sort_unstable();
        assert_eq!(
            got,
            vec!["aa", "bb"],
            "expected both tags to fit in 6 bytes"
        );
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
    // next_len (matching real join() length) after each of the 8 items, in tier order, is:
    // 12, 25, 38, 51, 55, 59, 63, 67.
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

    // `maybe_emit_header`'s return value is what the cache debugger's CDN-invalidation debug
    // info is built from (see `CdnInvalidationDebug` in debugger.rs) — these pin down that the
    // returned `CdnHeaderBuildResult` itself (not just the HeaderMap side effect, covered above)
    // matches each outcome.
    #[rstest]
    #[case::empty(
        InvalidationLabels::default(),
        16384,
        false,
        CdnTagHeaderOutcome::Empty,
        None,
        false
    )]
    #[case::complete_without_truncation(
        InvalidationLabels { tags: HashSet::from(["homepage".to_string()]), ..Default::default() },
        16384,
        false,
        CdnTagHeaderOutcome::CompleteWithoutTruncation,
        Some("homepage"),
        true,
    )]
    #[case::complete_with_truncation(
        InvalidationLabels {
            tags: HashSet::from(["aaaaaaaaaa".to_string(), "bbbbbbbbbb".to_string()]),
            ..Default::default()
        },
        15,
        false,
        CdnTagHeaderOutcome::CompleteWithTruncation,
        None, // exactly which tag survives isn't deterministic; checked separately below
        true,
    )]
    #[case::dropped_due_to_overflow(
        InvalidationLabels {
            tags: HashSet::from(["aaaaaaaaaa".to_string(), "bbbbbbbbbb".to_string()]),
            ..Default::default()
        },
        15,
        true,
        CdnTagHeaderOutcome::DroppedDueToOverflow,
        None,
        false,
    )]
    fn maybe_emit_header_result_matches_outcome_for_each_branch(
        #[case] labels: InvalidationLabels,
        #[case] max_bytes: usize,
        #[case] drop_on_overflow: bool,
        #[case] expected_outcome: CdnTagHeaderOutcome,
        #[case] expected_header: Option<&str>,
        #[case] expected_emitted: bool,
    ) {
        let mut headers = http::HeaderMap::new();
        let result = labels.maybe_emit_header(
            &mut headers,
            &cdn_config(|c| {
                c.max_bytes = max_bytes;
                c.experimental_drop_on_overflow = drop_on_overflow;
            }),
        );

        assert_eq!(result.outcome, expected_outcome, "{result:?}");
        assert_eq!(result.emitted, expected_emitted, "{result:?}");
        if let Some(expected_header) = expected_header {
            assert_eq!(
                result.header.as_deref(),
                Some(expected_header),
                "{result:?}"
            );
        }
        // `emitted` should always agree with whether a header actually landed in the map.
        assert_eq!(
            result.emitted,
            headers.contains_key(&cdn_config(|_| {}).header_name),
            "{result:?}"
        );
    }

    #[test]
    fn maybe_emit_header_result_reports_untruncated_size_only_when_nonempty() {
        let mut headers = http::HeaderMap::new();

        let empty_result =
            InvalidationLabels::default().maybe_emit_header(&mut headers, &cdn_config(|_| {}));
        assert_eq!(empty_result.untruncated_size_bytes, None);

        let labels = InvalidationLabels {
            tags: HashSet::from(["homepage".to_string()]),
            ..Default::default()
        };
        let result = labels.maybe_emit_header(&mut headers, &cdn_config(|_| {}));
        assert_eq!(result.untruncated_size_bytes, Some(8)); // "homepage" is 8 bytes
    }

    #[rstest]
    #[case::invalid_header_name("invalid header", "homepage")]
    #[case::invalid_header_value("Cache-Tag", "bad\r\ntag")]
    fn maybe_emit_header_result_reports_not_emitted_on_header_failure(
        #[case] header_name: &'static str,
        #[case] tag: &'static str,
    ) {
        let labels = InvalidationLabels {
            tags: HashSet::from([tag.to_string()]),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();

        let result = labels.maybe_emit_header(
            &mut headers,
            &cdn_config(|c| c.header_name = header_name.to_string()),
        );

        // The label set itself built fine (there was something to report) — it's only the
        // header name/value validation, a step after `build_header`, that failed.
        assert_eq!(
            result.outcome,
            CdnTagHeaderOutcome::CompleteWithoutTruncation
        );
        assert!(result.header.is_some(), "{result:?}");
        assert!(!result.emitted, "{result:?}");
        assert!(headers.is_empty());
    }
}
