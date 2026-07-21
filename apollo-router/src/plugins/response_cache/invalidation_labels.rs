// TODO: move under invalidation.rs
// TODO: reconcile with Redis

use crate::{Context, plugins::response_cache::plugin::CdnInvalidationConfig};
use http::{HeaderName, HeaderValue};
use itertools::Itertools;
use std::collections::HashSet;

use crate::plugins::response_cache::INTERNAL_CACHE_TAG_PREFIX;

/// Per-request aggregator of cache tags surfaced by response_cache.
///
/// Populated as the router walks the resolution tree: each subgraph response — cache hit,
/// cache miss, or partial entity hit — contributes its tag set. The supergraph_service
/// `map_response` consumes the union to emit the configured cache-tag header when
/// `cdn_invalidation.enabled` is true.
///
/// Stored as a typed `Context` extension; the type itself is the key. Tags pushed through
/// `add_many` are expected to be filtered through `external_invalidation_keys` first so
/// internal `__apollo_internal::` prefixed tags do not leak.
/// TODO: document fields
#[derive(Debug, Default, Clone)]
pub(crate) struct InvalidationLabels {
    pub(crate) tags: HashSet<String>,
    pub(crate) types: HashSet<(String, String)>,
    pub(crate) subgraphs: HashSet<String>,
    pub(crate) context: Option<Context>,
}

/// TODO: docs
impl InvalidationLabels {
    // TODO: docs on context
    pub(crate) fn get_or_create(context: &Context) -> Self {
        let invalidation_labels = context.extensions().with_lock(|lock| {
            let invalidation_labels = lock.get_or_default_mut::<InvalidationLabels>();
            // TODO: docs on why cheap to clone (arc, small vals)
            invalidation_labels.context = Some(context.clone());
            // TODO: explain why the clone matters here; we always want what's from context, but we
            // need to clone it to get it--more, etc
            invalidation_labels.clone()
        });
        invalidation_labels
    }

    // TODO: docs
    fn format_subgraph_labels(&self) -> Vec<String> {
        self.subgraphs
            .iter()
            .map(|subgraph| format!("subgraph-{subgraph}"))
            .collect()
    }
    // TODO: docs
    fn format_type_labels(&self) -> Vec<String> {
        self.types
            .iter()
            .map(|(subgraph, r#type)| format!("type-{subgraph}-{type}"))
            .collect()
    }

    // WARN: this removes the sorting behavior
    // TODO: test this thoroughly
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
            return None;
        }

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
            // TODO: metric here
            tracing::warn!(
                max_bytes = %config.max_bytes,
                actual_bytes = %header.len(),
                dropped_count = %dropped,
                "response_cache cache-tag header exceeds max_bytes; truncated per on_overflow=truncate"
            );
        }

        Some(header)
    }

    pub(crate) fn maybe_emit_header(
        &self,
        headers: &mut http::HeaderMap,
        config: &CdnInvalidationConfig,
    ) {
        let header = if let Some(labels) = self.build_header(config) {
            labels
        } else {
            // TODO: add metric for empty invalidation labels header
            // TODO: add debug-level log for ^
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

                // TODO: metric for error/skipping ^
                return;
            }
        };

        match HeaderValue::from_str(&header) {
            Ok(invalidation_labels) => {
                headers.insert(header_name, invalidation_labels);
                tracing::debug!("response_cache emitted aggregated cache-tag header");
                // TODO: decide on whether to emit the full header (might be large--per req)
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "response_cache aggregated cache-tag header value is not a valid HTTP header value; skipping emission"
                );
                // TODO: metric for error/skipping ^
            }
        }
    }

    /// Filters out any key that starts with whatever is represented by `INTERNAL_CACHE_TAG_PREFIX`,
    /// which denotes internal keys versus external. The difference between the two is that internal
    /// tags are not exposed to users; external are, and they're used for invalidation
    /// TODO: docs
    /// TODO: rename to `user_facing_only`
    pub(crate) fn user_facing_only(&self) -> Vec<String> {
        // TODO: use all labels
        self.tags
            .iter()
            // TODO: justify as being useful while also retaining tags (no into_iter, ie)
            .cloned()
            .filter(|k| !k.starts_with(INTERNAL_CACHE_TAG_PREFIX))
            .collect()
    }

    // TODO: docs
    pub(crate) fn merge(
        &self,
        other_invalidatoin_labels: InvalidationLabels,
    ) -> Result<Self, InvalidationLabelsError> {
        self.context
            .clone()
            .ok_or(InvalidationLabelsError::FailedToMerge(
                "Missing Context".to_string(),
            ))?
            .extensions()
            .with_lock(|lock| {
                let invalidation_labels = lock.get_mut::<InvalidationLabels>().ok_or(
                    InvalidationLabelsError::FailedToMerge(
                        "Missing InvalidationLabels on Context".to_string(),
                    ),
                )?;

                invalidation_labels
                    .tags
                    .extend(other_invalidatoin_labels.tags);
                invalidation_labels
                    .types
                    .extend(other_invalidatoin_labels.types);
                invalidation_labels
                    .subgraphs
                    .extend(other_invalidatoin_labels.subgraphs);

                // TODO: clone explanation
                // fresh view of InvalidationLabels after writing to context
                Ok(invalidation_labels.clone())
            })
    }

    // TODO: docs
    pub(crate) fn add_tags(&mut self, tags: Vec<String>) -> Result<Self, InvalidationLabelsError> {
        self.context
            .clone()
            .ok_or(InvalidationLabelsError::FailedToMerge(
                "Missing Context".to_string(),
            ))?
            .extensions()
            .with_lock(|lock| {
                let invalidation_labels = lock.get_mut::<InvalidationLabels>().ok_or(
                    InvalidationLabelsError::FailedToMerge(
                        "Missing InvalidationLabels on Context".to_string(),
                    ),
                )?;
                invalidation_labels.tags.extend(tags);
                Ok(invalidation_labels.clone())
            })
    }

    // TODO: docs
    pub(crate) fn add_type(
        &mut self,
        subgraph: &str,
        r#type: &str,
    ) -> Result<Self, InvalidationLabelsError> {
        self.context
            .clone()
            .ok_or(InvalidationLabelsError::FailedToMerge(
                "Missing Context".to_string(),
            ))?
            .extensions()
            .with_lock(|lock| {
                let invalidation_labels = lock.get_mut::<InvalidationLabels>().ok_or(
                    InvalidationLabelsError::FailedToMerge(
                        "Missing InvalidationLabels on Context".to_string(),
                    ),
                )?;
                invalidation_labels
                    .types
                    .insert((subgraph.to_string(), r#type.to_string()));
                Ok(invalidation_labels.clone())
            })
    }

    // TODO: docs
    pub(crate) fn add_subgraph(&mut self, subgraph: &str) -> Result<Self, InvalidationLabelsError> {
        self.context
            .clone()
            .ok_or(InvalidationLabelsError::FailedToMerge(
                "Missing Context".to_string(),
            ))?
            .extensions()
            .with_lock(|lock| {
                let invalidation_labels = lock.get_mut::<InvalidationLabels>().ok_or(
                    InvalidationLabelsError::FailedToMerge(
                        "Missing InvalidationLabels on Context".to_string(),
                    ),
                )?;
                invalidation_labels.subgraphs.insert(subgraph.to_string());
                Ok(invalidation_labels.clone())
            })
    }
}

// TODO: thiserror enumify this
#[derive(Debug)]
pub(crate) enum InvalidationLabelsError {
    // TODO: bettername
    FailedToMerge(String),
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

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
    fn mutators_without_context_return_failed_to_merge_error() {
        let mut labels = InvalidationLabels::default();
        assert!(labels.context.is_none());

        assert!(matches!(
            labels.add_tags(vec!["a".to_string()]),
            Err(InvalidationLabelsError::FailedToMerge(_))
        ));
        assert!(matches!(
            labels.add_type("accounts", "User"),
            Err(InvalidationLabelsError::FailedToMerge(_))
        ));
        assert!(matches!(
            labels.add_subgraph("accounts"),
            Err(InvalidationLabelsError::FailedToMerge(_))
        ));
        assert!(matches!(
            labels.merge(InvalidationLabels::default()),
            Err(InvalidationLabelsError::FailedToMerge(_))
        ));
    }

    #[test]
    fn mutators_with_unseeded_context_return_failed_to_merge_error() {
        let context = Context::new();
        let mut labels = unseeded_handle(&context);

        assert!(matches!(
            labels.add_tags(vec!["a".to_string()]),
            Err(InvalidationLabelsError::FailedToMerge(_))
        ));
        assert!(matches!(
            labels.add_type("accounts", "User"),
            Err(InvalidationLabelsError::FailedToMerge(_))
        ));
        assert!(matches!(
            labels.add_subgraph("accounts"),
            Err(InvalidationLabelsError::FailedToMerge(_))
        ));
        assert!(matches!(
            labels.merge(InvalidationLabels::default()),
            Err(InvalidationLabelsError::FailedToMerge(_))
        ));
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
}
