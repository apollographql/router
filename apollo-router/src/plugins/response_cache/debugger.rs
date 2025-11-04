use serde::Deserialize;
use serde::Serialize;

use crate::graphql;
use crate::json_ext::Object;
use crate::plugins::response_cache::cache_control::CacheControl;

pub(super) type CacheKeysContext = Vec<CacheKeyContext>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CacheKeyContext {
    pub(super) key: String,
    pub(super) invalidation_keys: Vec<String>,
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
        if self.cache_control.is_no_store() {
            self.warnings.push(Warning {
                code: "CACHE_CONTROL_NO_STORE".to_string(),
                links: vec![cache_control_mdn_docs.clone()],
                message: "The subgraph returned a Cache-Control header containing no-store, so the data was not cached".to_string(),
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
        match self.cache_control.s_max_age_or_max_age() {
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
                        message: "The subgraph returned a 'Cache-Control' header with a max-age smaller than the value of 'Age' header which means it's already expired, the Router won't cache this data.".to_string(),
                    });
                }
            }
            None => {
                // Default ttl
                self.warnings.push(Warning {
                    code: "CACHE_CONTROL_WITHOUT_MAX_AGE".to_string(),
                    links: vec![Link { url: String::from("https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/invalidation#configure-default-ttl"), title: "Configure default TTL in the Router".to_string() }, cache_control_mdn_docs.clone()],
                    message: "The subgraph returned a 'Cache-Control' header without any max-age set so the Router will use the one configured in Router's configuration.".to_string(),
                });
            }
        }
        if let CacheEntryKind::RootFields { root_fields } = &self.kind {
            // No cache tags on root fields
            if self.invalidation_keys.is_empty() {
                self.warnings.push(Warning {
                    code: "NO_CACHE_TAG_ON_ROOT_FIELD".to_string(),
                    links: vec![Link { url: String::from("https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/invalidation#invalidation-methods"), title: "Add '@cacheTag' in your schema".to_string() }],
                    message: "No cache tags are specified on your root fields query, if you want to use active invalidation you'll need to add cache tags on your root fields to actively invalidate cached data.".to_string(),
                });
            }

            let root_fields_len = root_fields.len();
            // Several root fields cached in the same entry
            if root_fields_len > 1 {
                self.warnings.push(Warning {
                    code: "SEVERAL_ROOT_FIELDS".to_string(),
                    links: vec![Link { url: String::from("https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/faq#how-does-caching-work-for-operations-with-multiple-root-fields"), title: "Caching for operations with multiple root fields".to_string() }],
                    message: format!("The query contains several root fields query, even if you set separate cache tags on each root fields you won't be able to only invalidate the specific root fields because we cache these {root_fields_len} root fields in the same cache entry per subgraph. It will invalidate this cache entry and so the data for these {root_fields_len} root fields you'll invalidate the data for all these root fields."),
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
