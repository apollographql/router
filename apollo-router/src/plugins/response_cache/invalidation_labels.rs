// TODO: move under invalidation.rs
// TODO: reconcile with Redis

use crate::{Context, plugins::response_cache::plugin::CdnInvalidationConfig};
use http::{HeaderName, HeaderValue};
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

    //pub(crate) fn invalidation_labels(&self) -> Self {
    //    // TODO: consider into()
    //    InvalidationLabels {
    //        tags: self.tags.clone(),
    //        types: self.types.clone(),
    //        subgraphs: self.subgraphs.clone(),
    //    }
    //}

    // WARN: this removes the sorting behavior
    // TODO: test this thoroughly
    fn build_header(&self, config: &CdnInvalidationConfig) -> Option<String> {
        if self.tags.is_empty() {
            return None;
        }

        let mut included: Vec<&str> = Vec::new();
        let mut current_len = 0usize;
        let header_delimiter_size = config.header_delimiter.len();

        let tags: Vec<&String> = self.tags.iter().collect();
        for tag in &tags {
            let next_len = current_len + header_delimiter_size + tag.len();

            if next_len >= config.max_bytes {
                break;
            }

            current_len = next_len;
            included.push(tag.as_str());
        }

        let joined = included.join(&config.header_delimiter);
        let dropped = tags.len() - included.len();

        tracing::warn!(
            max_bytes = %config.max_bytes,
            actual_bytes = %joined.len(),
            dropped_count = %dropped,
            "response_cache cache-tag header exceeds max_bytes; truncated per on_overflow=truncate"
        );

        Some(included.join(&config.header_delimiter))
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
            .unwrap_or(Err(InvalidationLabelsError::FailedToMerge(
                "Missing Context".to_string(),
            ))?)
            .extensions()
            .with_lock(|lock| {
                let invalidation_labels = lock.get_mut::<InvalidationLabels>().unwrap_or(Err(
                    InvalidationLabelsError::FailedToMerge(
                        "Missing InvalidationLabels on Context".to_string(),
                    ),
                )?);

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
}
