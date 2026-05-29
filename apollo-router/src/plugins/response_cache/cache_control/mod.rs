mod delimitted_formatter;

use std::fmt::Display;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use http::HeaderMap;
use http::HeaderValue;
use http::header::AGE;
use http::header::CACHE_CONTROL;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

use self::delimitted_formatter::DelimitedFormatter;

/// REQUEST Cache control header either:
/// * Sent from client to router to control response cache
/// * Sent from router to subgraph to control external cache (ie CDN)
///
/// RESPONSE Cache control header either:
/// * Returned from subgraph to router to control response cache
/// * Returned from router to client to control external cache (ie CDN)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheControl {
    /////// shared between request and response
    created: u64,

    /// Indicates that the response remains fresh until N seconds after the response is generated
    #[serde(skip_serializing_if = "Option::is_none", default)]
    max_age: Option<u64>,

    /// request: Asks cache to validate the response with the origin server before reuse.
    /// response: Indicates that the response can be stored in caches, but the response must be validated with the origin server before each reuse, even when the cache is disconnected from the origin server.
    #[serde(skip_serializing_if = "is_false", default)]
    no_cache: bool,

    /// request: Asks cache to refrain from storing the request and corresponding response — even if the origin server's response could be stored.
    /// response: Indicates that caches of any kind should not store this response.
    // TODO: remove no_store pub facing
    #[serde(skip_serializing_if = "is_false", default)]
    pub no_store: bool,

    /// request: Indicates that any intermediary (regardless of whether it implements a cache) shouldn't transform the response contents.
    /// response: Indicates that any intermediary (regardless of whether it implements a cache) shouldn't transform the response contents.
    /// TODO: not sure if used by the router
    #[serde(skip_serializing_if = "is_false", default)]
    no_transform: bool,

    /// request: Indicates that the browser is interested in receiving stale content on error from any intermediate server for a particular origin.
    /// response: Indicates that the cache can reuse a stale response when an upstream server generates an error, or when the error is generated locally. Here, an error is considered any response with a status code of 500, 502, 503, or 504.
    /// TODO: apparently this is not supported by any browser.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    stale_if_error: Option<u64>,

    ////// request only
    /// Indicates that the client allows a stored response that is stale within N seconds.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    max_stale: Option<u64>,

    /// Indicates that the client allows a stored response that is fresh for at least N seconds
    #[serde(skip_serializing_if = "Option::is_none", default)]
    min_fresh: Option<u64>,

    /// Indicates that an already-cached response should be returned. If a cache has a stored response, even a stale one, it will be returned. If no cached response is available, a 504 Gateway Timeout response will be returned.
    #[serde(skip_serializing_if = "is_false", default)]
    only_if_cached: bool,

    /////// response only
    /// Not actually part of the header, used to offset the max_age
    #[serde(skip_serializing_if = "Option::is_none", default)]
    age: Option<u64>,

    /// Indicates how long the response remains fresh in a shared cache. Overrides the value specified by max-age.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    s_max_age: Option<u64>,

    /// Indicates that the response can be stored in caches and can be reused while fresh. If the response becomes stale, it must be validated with the origin server before reuse.
    #[serde(skip_serializing_if = "is_false", default)]
    must_revalidate: bool,

    /// The equivalent of `must-revalidate`, but specifically for shared caches.
    #[serde(skip_serializing_if = "is_false", default)]
    proxy_revalidate: bool,

    /// Indicates that a cache should store the response only if it understands the requirements for caching based on status code.
    /// TODO: not sure what this means for our use case
    #[serde(skip_serializing_if = "is_false", default)]
    must_understand: bool,

    /// Indicates that the response can be stored only in a private cache (e.g., local caches in browsers).
    #[serde(skip_serializing_if = "is_false", default)]
    private: bool,

    /// Indicates that the response can be stored in a shared cache (i.e., despite the presence of the `Authorization` header).
    #[serde(skip_serializing_if = "is_false", default)]
    public: bool,

    /// Indicates that the response will not be updated while it's fresh.
    /// TODO: not sure if used by the router
    #[serde(skip_serializing_if = "is_false", default)]
    immutable: bool,

    /// Indicates that the cache could reuse a stale response while it revalidates it to a cache.
    /// TODO: not sure if used by the router
    /// TODO: I think this is a type change, make sure that existing 'true' values are deserialized as none
    /// TODO: make sure that unknown fields in the redis cache control value do not mess up deserialization
    #[serde(skip_serializing_if = "Option::is_none", default)]
    stale_while_revalidate: Option<u64>,
}

fn is_false(b: &bool) -> bool {
    !b
}

impl Default for CacheControl {
    fn default() -> Self {
        Self {
            created: now_epoch_seconds(),
            age: None,
            max_age: None,
            s_max_age: None,
            no_cache: false,
            no_store: false,
            no_transform: false,
            must_revalidate: false,
            proxy_revalidate: false,
            must_understand: false,
            private: false,
            public: false,
            immutable: false,
            stale_while_revalidate: None,
            stale_if_error: None,
            max_stale: None,
            min_fresh: None,
            only_if_cached: false,
        }
    }
}

impl TryFrom<&HeaderMap> for CacheControl {
    type Error = BoxError;

    fn try_from(headers: &HeaderMap) -> Result<Self, Self::Error> {
        let mut cache_control = Self::default();

        if let Some(age) = headers.get(AGE) {
            let age = age.to_str()?.trim().parse()?;
            cache_control.age = Some(age);
        }

        let mut header_values = headers.get_all(CACHE_CONTROL).iter().peekable();

        // if there is no cache-control header at all, return early, setting no_store to true
        if header_values.peek().is_none() {
            // TODO: is this actually desirable behavior? doesn't seem quite right, but matches pre-existing behavior as far as I can tell
            cache_control.no_store = true;
            return Ok(cache_control);
        }

        for header_value in header_values {
            for directive in header_value.to_str()?.split(',') {
                let (key, value) = parse_directive(directive)?;

                match (key, value) {
                    ("max-age", Some(value)) => {
                        cache_control.max_age = Some(value.parse()?);
                    }
                    ("s-maxage", Some(value)) => {
                        cache_control.s_max_age = Some(value.parse()?);
                    }
                    ("no-cache", None) => {
                        cache_control.no_cache = true;
                    }
                    ("no-store", None) => {
                        cache_control.no_store = true;
                    }
                    ("no-transform", None) => {
                        cache_control.no_transform = true;
                    }
                    ("must-revalidate", None) => {
                        cache_control.must_revalidate = true;
                    }
                    ("proxy-revalidate", None) => {
                        cache_control.proxy_revalidate = true;
                    }
                    ("must-understand", None) => {
                        cache_control.must_understand = true;
                    }
                    ("private", None) => {
                        cache_control.private = true;
                    }
                    ("public", None) => {
                        cache_control.public = true;
                    }
                    ("immutable", None) => {
                        cache_control.immutable = true;
                    }
                    ("stale-while-revalidate", Some(value)) => {
                        cache_control.stale_while_revalidate = Some(value.parse()?);
                    }
                    // TODO: handle ("stale-while-revalidate", None)?
                    ("stale-if-error", Some(value)) => {
                        cache_control.stale_if_error = Some(value.parse()?);
                    }
                    // TODO: handle ("stale-if-error", None)?
                    ("max-stale", Some(value)) => {
                        cache_control.max_stale = Some(value.parse()?);
                    }
                    ("min-fresh", Some(value)) => {
                        cache_control.min_fresh = Some(value.parse()?);
                    }
                    ("only-if-cached", None) => {
                        cache_control.only_if_cached = true;
                    }
                    _ => {
                        return Err(
                            format!("invalid Cache-Control header value: {directive}").into()
                        );
                    }
                }
            }
        }

        Ok(cache_control)
    }
}

impl Display for CacheControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut formatter = DelimitedFormatter::from(f);

        // If no-store, write just that and return early. This prevents potentially conflicting
        // directives (https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#preventing_storing).
        // TODO: write no-store if max-age = 0?
        // TODO: is this really what we want? what if it's not no-cache?
        if self.no_store {
            write!(&mut formatter, "no-store")?;
            return Ok(());
        }

        let elapsed = self.elapsed();

        if let Some(max_age) = self.max_age {
            let max_age = remaining_ttl(max_age, elapsed);
            write!(&mut formatter, "max-age={}", max_age)?;
        }

        if let Some(max_age) = self.s_max_age {
            let max_age = remaining_ttl(max_age, elapsed);
            write!(&mut formatter, "s-maxage={}", max_age)?;
        }

        if self.no_cache {
            write!(&mut formatter, "no-cache")?;
        }

        if self.no_transform {
            write!(&mut formatter, "no-transform")?;
        }

        if self.must_revalidate {
            write!(&mut formatter, "must-revalidate")?;
        }

        if self.proxy_revalidate {
            write!(&mut formatter, "proxy-revalidate")?;
        }

        if self.must_understand {
            write!(&mut formatter, "must_understand")?;
        }

        if self.private {
            write!(&mut formatter, "private")?;
        }

        if self.public {
            write!(&mut formatter, "public")?;
        }

        if self.immutable {
            write!(&mut formatter, "immutable")?;
        }

        if let Some(stale) = self.stale_while_revalidate {
            let stale = remaining_ttl(stale, elapsed);
            write!(&mut formatter, "stale-while-revalidate={}", stale)?;
        }

        if let Some(stale) = self.stale_if_error {
            let stale = remaining_ttl(stale, elapsed);
            write!(&mut formatter, "stale-if-error={}", stale)?;
        }

        // TODO: check this logic, pretty sure it's right though
        if let Some(min_fresh) = self.min_fresh {
            let min_fresh = remaining_ttl(min_fresh, elapsed);
            write!(&mut formatter, "min-fresh={}", min_fresh)?;
        }

        if self.no_transform {
            write!(&mut formatter, "no-transform")?;
        }

        if self.only_if_cached {
            write!(&mut formatter, "only-if-cached")?;
        }

        Ok(())
    }
}

impl CacheControl {
    pub(crate) fn default_no_store() -> Self {
        Self {
            no_store: true,
            ..Self::default()
        }
    }

    // TODO better docstring - set max-age based on ttl if max-age not already present
    //  NOTE: doesn't set it if no-store is set
    pub(crate) fn with_default_ttl(mut self, ttl: Option<Duration>) -> Self {
        if self.no_store {
            return self;
        }

        let ttl: Option<u64> = ttl.as_ref().map(Duration::as_secs);
        self.max_age = self.max_age.or(ttl);

        self
    }

    fn elapsed(&self) -> u64 {
        now_epoch_seconds().saturating_sub(self.created)
    }

    pub(crate) fn update_headers(&self, headers: &mut HeaderMap) -> Result<(), BoxError> {
        let cache_control_header_value = self.to_string();
        headers.insert(
            CACHE_CONTROL,
            HeaderValue::from_str(&cache_control_header_value)?,
        );

        if let Some(age) = self.age
            && age > 0
        {
            headers.insert(AGE, age.into());
        }

        Ok(())
    }

    pub(crate) fn merge_and_update_ttl(&self, other: &Self) -> Self {
        self.merge(other, Some(now_epoch_seconds()))
    }

    pub(crate) fn merge_without_ttl_update(&self, other: &Self) -> Self {
        self.merge(other, None)
    }

    fn merge(&self, other: &Self, now: Option<u64>) -> Self {
        // If no-store, write just that and return early. This prevents potentially conflicting
        // directives (https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#preventing_storing).
        // TODO: is this really what we want? what if it's not no-cache?
        if self.no_store || other.no_store {
            return Self {
                no_store: true,
                ..Self::default()
            };
        }

        Self {
            created: now.unwrap_or(0),
            age: None,
            max_age: minimum_optional_value(
                self.remaining_max_age(now),
                other.remaining_max_age(now),
            ),
            // TODO: prev logic would eliminate s_max_age in favor of bundling it with max_age. ideally this would not happen during the merge
            s_max_age: minimum_optional_value(
                self.remaining_s_max_age(now),
                other.remaining_s_max_age(now),
            ),
            no_cache: self.no_cache || other.no_cache,
            no_store: self.no_store || other.no_store,
            no_transform: self.no_transform || other.no_transform,
            must_revalidate: self.must_revalidate || other.must_revalidate,
            proxy_revalidate: self.proxy_revalidate || other.proxy_revalidate,
            must_understand: self.must_understand || other.must_understand,
            private: self.private || other.private,
            // TODO: prev logic would public based on value of private. ideally this would not happen during the merge
            public: self.public || other.public,
            immutable: self.immutable || other.immutable,
            stale_while_revalidate: minimum_optional_value(
                self.remaining_stale_while_revalidate(now),
                other.remaining_stale_while_revalidate(now),
            ),
            stale_if_error: minimum_optional_value(
                self.remaining_stale_if_error(now),
                other.remaining_stale_if_error(now),
            ),
            max_stale: minimum_optional_value(
                self.remaining_max_stale(now),
                other.remaining_max_stale(now),
            ),
            min_fresh: maximum_optional_value(
                self.remaining_min_fresh(now),
                other.remaining_min_fresh(now),
            ),
            only_if_cached: self.only_if_cached || other.only_if_cached,
        }
    }
}

// various getters
impl CacheControl {
    pub(crate) fn no_cache(&self) -> bool {
        self.no_cache
    }
    pub(crate) fn no_store(&self) -> bool {
        self.no_store
    }
    pub(crate) fn private(&self) -> bool {
        self.private
    }
    pub(crate) fn public(&self) -> bool {
        // NB: private overrides public
        !self.private && self.public
    }

    // TODO: consider using this as the inverse of no_store and replacing no_store with no_store_raw --
    //  ideally we'd just use this
    pub(crate) fn should_store(&self) -> bool {
        !self.no_store && self.ttl().is_none_or(|ttl| ttl > 0)
    }

    pub(crate) fn can_use(&self) -> bool {
        // TODO: honor stale-while-revalidate
        !self.no_cache && self.ttl_new().is_none_or(|ttl| ttl > 0)
    }

    pub(crate) fn age(&self) -> Option<u64> {
        self.age
    }

    // TODO doc that this includes s_max_age adn why
    pub(crate) fn max_age(&self) -> Option<u64> {
        self.s_max_age.or(self.max_age)
    }

    // TODO: this is relative to age, not now!!
    pub(crate) fn ttl(&self) -> Option<u64> {
        self.remaining_duration(self.max_age(), None)
    }

    // TODO: this is relative to now as well!! I think we should use it
    pub(crate) fn ttl_new(&self) -> Option<u64> {
        let max_age = self.s_max_age.or(self.max_age);
        self.remaining_duration(max_age, Some(now_epoch_seconds()))
    }
}

// various helpers - not sure where they should really live
impl CacheControl {
    fn remaining_duration(&self, value: Option<u64>, now: Option<u64>) -> Option<u64> {
        let value = value?;
        let elapsed = now.map(|now| now.saturating_sub(self.created));

        let subtrahend = self.age.unwrap_or(0) + elapsed.unwrap_or(0);
        Some(value.saturating_sub(subtrahend))
    }

    fn remaining_max_age(&self, now: Option<u64>) -> Option<u64> {
        self.remaining_duration(self.max_age, now)
    }

    fn remaining_s_max_age(&self, now: Option<u64>) -> Option<u64> {
        self.remaining_duration(self.s_max_age, now)
    }

    fn remaining_stale_while_revalidate(&self, now: Option<u64>) -> Option<u64> {
        self.remaining_duration(self.stale_while_revalidate, now)
    }

    fn remaining_stale_if_error(&self, now: Option<u64>) -> Option<u64> {
        self.remaining_duration(self.stale_if_error, now)
    }

    fn remaining_max_stale(&self, now: Option<u64>) -> Option<u64> {
        self.remaining_duration(self.max_stale, now)
    }

    fn remaining_min_fresh(&self, now: Option<u64>) -> Option<u64> {
        self.remaining_duration(self.min_fresh, now)
    }
}

fn minimum_optional_value<T: Ord>(x: Option<T>, y: Option<T>) -> Option<T> {
    match (x, y) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(std::cmp::min(x, y)),
    }
}

fn maximum_optional_value<T: Ord>(x: Option<T>, y: Option<T>) -> Option<T> {
    match (x, y) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(std::cmp::max(x, y)),
    }
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("we should not run before EPOCH")
        .as_secs()
}

/// TODO needs docstring
fn parse_directive(directive: &str) -> Result<(&str, Option<&str>), BoxError> {
    let mut directive_kv = directive.trim().split('=');
    let (key, value) = (directive_kv.next(), directive_kv.next());

    if key.is_none() || directive_kv.next().is_some() {
        return Err("invalid Cache-Control header value".into());
    }

    let key = key.expect("key was checked above");
    Ok((key, value))
}

fn remaining_ttl(ttl: u64, elapsed: u64) -> u64 {
    ttl.saturating_sub(elapsed)
}

#[cfg(test)]
impl CacheControl {
    /// TODO doc (used below)
    fn remaining_ttl(&self, now: u64) -> Option<u64> {
        self.remaining_duration(self.ttl(), Some(now))
    }

    /// TODO doc - used to standardize snapshots
    pub(crate) fn zero_out_created(&mut self) {
        self.created = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_ttl() {
        let now = now_epoch_seconds();

        let first = CacheControl {
            created: now - 10,
            max_age: Some(40),
            ..Default::default()
        };

        let second = CacheControl {
            created: now - 20,
            max_age: Some(60),
            ..Default::default()
        };

        assert_eq!(first.remaining_ttl(now), Some(30));
        assert_eq!(second.remaining_ttl(now), Some(40));

        let merged = first.merge(&second, now.into());
        assert_eq!(merged.created, now);

        assert_eq!(merged.ttl(), Some(30));
        assert_eq!(merged.remaining_ttl(now), Some(30));
        assert!(merged.can_use());
    }

    #[test]
    fn merge_nostore() {
        let now = now_epoch_seconds();

        let first = CacheControl {
            created: now,
            max_age: Some(40),
            no_store: true,
            ..Default::default()
        };

        let second = CacheControl {
            created: now,
            max_age: Some(60),
            no_store: false,
            public: true,
            ..Default::default()
        };

        let merged = first.merge(&second, now.into());
        assert!(merged.no_store);
        assert!(!merged.public);
        assert!(merged.can_use());
    }

    #[test]
    fn merge_nocache() {
        let now = now_epoch_seconds();

        let first = CacheControl {
            no_cache: true,
            ..Default::default()
        };

        let second = CacheControl {
            no_cache: false,
            ..Default::default()
        };

        let merged = first.merge(&second, now.into());
        assert!(merged.no_cache);
        assert!(!merged.can_use());
    }

    #[test]
    fn remove_conflicts() {
        let now = now_epoch_seconds();

        let first = CacheControl {
            created: now,
            max_age: Some(40),
            no_store: true,
            must_revalidate: true,
            no_cache: true,
            private: true,
            ..Default::default()
        };
        let cache_control_header = first.to_string();
        assert_eq!(cache_control_header, "no-store".to_string());
    }

    #[test]
    fn merge_public_private() {
        let now = now_epoch_seconds();

        let first = CacheControl {
            created: now,
            max_age: Some(40),
            public: true,
            private: false,
            ..Default::default()
        };

        let second = CacheControl {
            created: now,
            max_age: Some(60),
            public: false,
            private: true,
            ..Default::default()
        };

        let merged = first.merge(&second, now.into());
        assert!(!merged.public());
        assert!(merged.private());
        assert!(merged.can_use());
    }

    #[test]
    fn create_expired_cache_control() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now,
            max_age: Some(40),
            age: Some(50),
            public: true,
            private: false,
            ..Default::default()
        };
        assert!(!cc.should_store()); // Because age is bigger than max_age

        let cc = CacheControl {
            created: now + 1000,
            max_age: Some(40),
            age: Some(50),
            public: true,
            private: false,
            ..Default::default()
        };
        assert!(!cc.can_use()); // Because created is bigger than now
        assert!(!cc.should_store()); // Because age is bigger than max_age
    }
}
