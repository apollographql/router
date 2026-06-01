use std::fmt::Display;
use std::fmt::Formatter;
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
    no_store: bool,

    /// request: Indicates that any intermediary (regardless of whether it implements a cache) shouldn't transform the response contents.
    /// response: Indicates that any intermediary (regardless of whether it implements a cache) shouldn't transform the response contents.
    /// The router does not transform response bodies, so this directive does not affect caching behavior.
    /// It is parsed and propagated to the client for downstream caches to honor.
    #[serde(skip_serializing_if = "is_false", default)]
    no_transform: bool,

    /// request: Indicates that the browser is interested in receiving stale content on error from any intermediate server for a particular origin.
    /// response: Indicates that the cache can reuse a stale response when an upstream server generates an error, or when the error is generated locally. Here, an error is considered any response with a status code of 500, 502, 503, or 504.
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
    /// The router does not validate status code semantics before caching, so this directive is parsed and propagated
    /// to the client but not actively enforced.
    #[serde(skip_serializing_if = "is_false", default)]
    must_understand: bool,

    /// Indicates that the response can be stored only in a private cache (e.g., local caches in browsers).
    #[serde(skip_serializing_if = "is_false", default)]
    private: bool,

    /// Indicates that the response can be stored in a shared cache (i.e., despite the presence of the `Authorization` header).
    #[serde(skip_serializing_if = "is_false", default)]
    public: bool,

    /// Indicates that the response will not be updated while it's fresh.
    /// The router does not currently use this to optimize cache revalidation, but it is parsed and propagated to the client.
    #[serde(skip_serializing_if = "is_false", default)]
    immutable: bool,

    /// Indicates that the cache could reuse a stale response while it revalidates it to a cache.
    /// The router does not currently serve stale responses while revalidating; see [`can_use`].
    /// Older schema versions stored this field as a boolean; the custom deserializer handles both.
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "deserialize_stale_while_revalidate"
    )]
    stale_while_revalidate: Option<u64>,
}

/// Deserializes `stale_while_revalidate` from either a `u64` (current schema) or a `bool`
/// (legacy schema). A boolean value is treated as `None` since no duration is available.
fn deserialize_stale_while_revalidate<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrU64 {
        Duration(u64),
        Bool(bool),
    }

    Ok(match Option::<BoolOrU64>::deserialize(deserializer)? {
        Some(BoolOrU64::Duration(n)) => Some(n),
        Some(BoolOrU64::Bool(_)) | None => None,
    })
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
            // TODO: defaulting to no_store here is correct for subgraph responses (no header = don't
            // cache), but wrong for client requests (no header = no preference). TryFrom is currently
            // used for both. Consider splitting into separate constructors so that parsing a client
            // request with no Cache-Control header doesn't incorrectly block cache lookups.
            cache_control.no_store = true;
            return Ok(cache_control);
        }

        for header_value in header_values {
            for directive in header_value.to_str()?.split(',') {
                let (key, value) = parse_directive(directive)?;

                let parse_value = |value: Option<&str>| -> Result<u64, BoxError> {
                    let value = value.ok_or_else(|| {
                        format!("invalid Cache-Control header value: {directive}")
                    })?;
                    Ok(value.parse()?)
                };

                match key {
                    "immutable" => cache_control.immutable = true,
                    "must-revalidate" => cache_control.must_revalidate = true,
                    "must-understand" => cache_control.must_understand = true,
                    "no-cache" => cache_control.no_cache = true,
                    "no-store" => cache_control.no_store = true,
                    "no-transform" => cache_control.no_transform = true,
                    "only-if-cached" => cache_control.only_if_cached = true,
                    "private" => cache_control.private = true,
                    "proxy-revalidate" => cache_control.proxy_revalidate = true,
                    "public" => cache_control.public = true,

                    "max-age" => cache_control.max_age = Some(parse_value(value)?),
                    "s-maxage" => cache_control.s_max_age = Some(parse_value(value)?),
                    "stale-if-error" => cache_control.stale_if_error = Some(parse_value(value)?),
                    "stale-while-revalidate" => {
                        cache_control.stale_while_revalidate = Some(parse_value(value)?)
                    }
                    "min-fresh" => cache_control.min_fresh = Some(parse_value(value)?),

                    "max-stale" => {
                        // max-stale without a value means to accept any age, so use u64::MAX if no
                        // value is provided (works out to 584 billion years)
                        let value = value.map_or(Ok(u64::MAX), |v| v.parse())?;
                        cache_control.max_stale = Some(value);
                    }

                    // RFC 9111 §5.2 allows extension directives, so don't error on unrecognized keys
                    _ => {}
                }
            }
        }

        // private overrules public
        if cache_control.public && cache_control.private {
            cache_control.public = false;
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
            write!(&mut formatter, "must-understand")?;
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

    /// Sets `max_age` to the given TTL if `max_age` is not already present in the header.
    /// Has no effect if `no_store` is set.
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

    pub(crate) fn merge_no_store(&mut self, other: &Self) {
        self.no_store |= other.no_store;
    }

    pub(crate) fn merge(&self, other: &Self) -> Self {
        self.merge_inner(other, now_epoch_seconds(), true)
    }

    pub(crate) fn merge_without_ttl_update(&self, other: &Self) -> Self {
        self.merge_inner(other, now_epoch_seconds(), false)
    }

    fn merge_inner(&self, other: &Self, now_epoch: u64, update_ttl: bool) -> Self {
        // If no-store, write just that and return early. This prevents potentially conflicting
        // directives (https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#preventing_storing).
        // TODO: is this really what we want? what if it's not no-cache?
        if self.no_store || other.no_store {
            return Self {
                created: now_epoch,
                no_store: true,
                ..Self::default()
            };
        }

        let now = update_ttl.then_some(now_epoch);

        let private = self.private || other.private;
        Self {
            created: now_epoch,
            age: None,
            max_age: minimum_optional_value(
                self.remaining_max_age(now),
                other.remaining_max_age(now),
            ),
            s_max_age: minimum_optional_value(
                self.remaining_s_max_age(now),
                other.remaining_s_max_age(now),
            ),
            no_cache: self.no_cache || other.no_cache,
            no_store: false,
            no_transform: self.no_transform || other.no_transform,
            must_revalidate: self.must_revalidate || other.must_revalidate,
            proxy_revalidate: self.proxy_revalidate || other.proxy_revalidate,
            must_understand: self.must_understand || other.must_understand,
            private,
            public: !private && (self.public || other.public),
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
        self.public
    }

    // TODO: consider using this as the inverse of no_store and replacing no_store with no_store_raw --
    //  ideally we'd just use this
    pub(crate) fn should_store(&self) -> bool {
        !self.no_store && self.ttl().is_none_or(|ttl| ttl > 0)
    }

    pub(crate) fn can_use(&self) -> bool {
        // TODO: honor stale-while-revalidate
        !self.no_cache && self.remaining_ttl().is_none_or(|ttl| ttl > 0)
    }

    pub(crate) fn age(&self) -> Option<u64> {
        self.age
    }

    /// Returns the effective max age in seconds, preferring `s-maxage` over `max-age`.
    /// `s-maxage` is used because the router acts as a shared cache.
    pub(crate) fn max_age(&self) -> Option<u64> {
        self.s_max_age.or(self.max_age)
    }

    /// Returns the remaining TTL in seconds, computed as `max_age - age`.
    /// This accounts for the `Age` header (how old the response was when received) but not
    /// for time elapsed since this struct was created. Use [`remaining_ttl`] for a fully time-relative TTL.
    pub(crate) fn ttl(&self) -> Option<u64> {
        self.remaining_duration(self.max_age(), None)
    }

    /// Returns the remaining TTL in seconds as of now, computed as `max_age - age - elapsed`
    /// where `elapsed` is the time since this struct was created.
    pub(crate) fn remaining_ttl(&self) -> Option<u64> {
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

/// Parses a single Cache-Control directive of the form `key` or `key=value`.
/// Returns an error if the directive contains more than one `=` sign.
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

struct DelimitedFormatter<'a, 'b> {
    formatter: &'a mut Formatter<'b>,
    delimiter: &'a str,
    wrote_prev: bool,
}

impl<'a, 'b> From<&'a mut Formatter<'b>> for DelimitedFormatter<'a, 'b> {
    fn from(formatter: &'a mut Formatter<'b>) -> Self {
        Self {
            formatter,
            delimiter: ",",
            wrote_prev: false,
        }
    }
}

impl<'a, 'b> DelimitedFormatter<'a, 'b> {
    fn write_fmt(&mut self, fmt: std::fmt::Arguments<'_>) -> std::fmt::Result {
        if self.wrote_prev {
            self.formatter.write_str(self.delimiter)?;
        }

        self.formatter.write_fmt(fmt)?;
        self.wrote_prev = true;

        Ok(())
    }
}

#[cfg(test)]
impl CacheControl {
    /// Returns the remaining TTL in seconds at the given Unix timestamp.
    /// Used in tests to assert TTL values at a specific point in time without depending on the current clock.
    fn remaining_ttl_at(&self, now: u64) -> Option<u64> {
        self.remaining_duration(self.max_age(), Some(now))
    }

    /// Sets `created` to 0, making time-dependent fields deterministic in snapshot tests.
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

        assert_eq!(first.remaining_ttl_at(now), Some(30));
        assert_eq!(second.remaining_ttl_at(now), Some(40));

        let merged = first.merge_inner(&second, now, true);

        assert_eq!(merged.ttl(), Some(30));
        assert_eq!(merged.remaining_ttl_at(now), Some(30));
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

        let merged = first.merge_inner(&second, now, true);
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

        let merged = first.merge_inner(&second, now, true);
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

        let merged = first.merge_inner(&second, now, true);
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

    #[test]
    fn deserialize_stale_while_revalidate_as_bool_succeeds() {
        // Old Redis entries may have stored stale_while_revalidate as a boolean.
        // A boolean value is treated as None since no duration is available.
        let json = r#"{"created":0,"staleWhileRevalidate":true}"#;
        let cc: CacheControl = serde_json::from_str(json).unwrap();
        assert_eq!(cc.stale_while_revalidate, None);

        let json = r#"{"created":0,"staleWhileRevalidate":false}"#;
        let cc: CacheControl = serde_json::from_str(json).unwrap();
        assert_eq!(cc.stale_while_revalidate, None);
    }

    #[test]
    fn deserialize_stale_while_revalidate_as_number_works() {
        let json = r#"{"created":0,"staleWhileRevalidate":60}"#;
        let cc: CacheControl = serde_json::from_str(json).unwrap();
        assert_eq!(cc.stale_while_revalidate, Some(60));
    }

    // --- Parsing (TryFrom<&HeaderMap>) ---

    fn header_map(pairs: &[(&str, &str)]) -> http::HeaderMap {
        pairs
            .iter()
            .fold(http::HeaderMap::new(), |mut map, (k, v)| {
                map.insert(
                    http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    http::HeaderValue::from_str(v).unwrap(),
                );
                map
            })
    }

    #[test]
    fn parse_missing_cache_control_header() {
        // No Cache-Control header should default to no_store per pre-existing behavior
        let cc = CacheControl::try_from(&http::HeaderMap::new()).unwrap();
        assert!(cc.no_store());
    }

    #[test]
    fn parse_max_age() {
        let cc = CacheControl::try_from(&header_map(&[("cache-control", "max-age=60")])).unwrap();
        assert_eq!(cc.max_age, Some(60));
        assert_eq!(cc.s_max_age, None);
    }

    #[test]
    fn parse_s_maxage() {
        // s-maxage should be stored separately, not collapsed into max_age
        let cc =
            CacheControl::try_from(&header_map(&[("cache-control", "s-maxage=30")])).unwrap();
        assert_eq!(cc.s_max_age, Some(30));
        assert_eq!(cc.max_age, None);
    }

    #[test]
    fn parse_s_maxage_and_max_age_kept_separate() {
        // Both directives present: must not be collapsed together
        let cc = CacheControl::try_from(&header_map(&[("cache-control", "max-age=60,s-maxage=30")])).unwrap();
        assert_eq!(cc.max_age, Some(60));
        assert_eq!(cc.s_max_age, Some(30));
        // max_age() getter prefers s-maxage (router acts as shared cache)
        assert_eq!(cc.max_age(), Some(30));
    }

    #[test]
    fn parse_no_cache() {
        let cc =
            CacheControl::try_from(&header_map(&[("cache-control", "no-cache")])).unwrap();
        assert!(cc.no_cache());
    }

    #[test]
    fn parse_no_store() {
        let cc =
            CacheControl::try_from(&header_map(&[("cache-control", "no-store")])).unwrap();
        assert!(cc.no_store());
    }

    #[test]
    fn parse_private_overrides_public() {
        // If both private and public are present, private wins
        let cc = CacheControl::try_from(&header_map(&[("cache-control", "public,private")])).unwrap();
        assert!(cc.private());
        assert!(!cc.public());
    }

    #[test]
    fn parse_age_header() {
        let cc = CacheControl::try_from(&header_map(&[
            ("cache-control", "max-age=60"),
            ("age", "10"),
        ]))
        .unwrap();
        assert_eq!(cc.age(), Some(10));
        // TTL should account for age: 60 - 10 = 50
        assert_eq!(cc.ttl(), Some(50));
    }

    #[test]
    fn parse_max_stale_without_value() {
        // Bare max-stale means accept any stale age (u64::MAX sentinel)
        let cc =
            CacheControl::try_from(&header_map(&[("cache-control", "max-stale")])).unwrap();
        assert_eq!(cc.max_stale, Some(u64::MAX));
    }

    #[test]
    fn parse_max_stale_with_value() {
        let cc =
            CacheControl::try_from(&header_map(&[("cache-control", "max-stale=120")])).unwrap();
        assert_eq!(cc.max_stale, Some(120));
    }

    #[test]
    fn parse_extension_directives_ignored() {
        // RFC 9111 §5.2: unknown directives must be silently ignored
        let cc = CacheControl::try_from(&header_map(&[(
            "cache-control",
            "max-age=60,cdn-cache-control=120,some-future-directive",
        )]))
        .unwrap();
        assert_eq!(cc.max_age, Some(60));
    }

    #[test]
    fn parse_multiple_cache_control_headers() {
        // Multiple Cache-Control headers should be merged
        let mut map = http::HeaderMap::new();
        map.append(
            http::header::CACHE_CONTROL,
            http::HeaderValue::from_static("max-age=60"),
        );
        map.append(
            http::header::CACHE_CONTROL,
            http::HeaderValue::from_static("no-transform"),
        );
        let cc = CacheControl::try_from(&map).unwrap();
        assert_eq!(cc.max_age, Some(60));
        assert!(cc.no_transform);
    }

    // --- s-maxage preservation through merge and display ---

    #[test]
    fn s_maxage_preserved_through_merge() {
        // The old code collapsed s-maxage into max-age during merge.
        // Verify the new code keeps them as separate fields.
        let now = now_epoch_seconds();
        let first = CacheControl {
            created: now,
            s_max_age: Some(30),
            max_age: None,
            ..Default::default()
        };
        let second = CacheControl {
            created: now,
            s_max_age: Some(60),
            max_age: Some(120),
            ..Default::default()
        };
        let merged = first.merge_inner(&second, now, true);
        assert_eq!(merged.s_max_age, Some(30));
        assert_eq!(merged.max_age, Some(120));
        // Effective TTL uses s_max_age
        assert_eq!(merged.max_age(), Some(30));
    }

    #[test]
    fn s_maxage_in_display() {
        // s-maxage and max-age should be emitted as separate directives
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now,
            s_max_age: Some(30),
            max_age: Some(60),
            ..Default::default()
        };
        let header = cc.to_string();
        assert!(header.contains("s-maxage=30"), "got: {header}");
        assert!(header.contains("max-age=60"), "got: {header}");
    }

    #[test]
    fn s_maxage_round_trip() {
        // Parse s-maxage from a header, serialize back, confirm it's preserved
        let cc =
            CacheControl::try_from(&header_map(&[("cache-control", "s-maxage=30,max-age=60")])).unwrap();
        let header = cc.to_string();
        assert!(header.contains("s-maxage="), "got: {header}");
        assert!(header.contains("max-age="), "got: {header}");
    }

    // --- Display ---

    #[test]
    fn display_decrements_max_age_by_elapsed() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now - 10,
            max_age: Some(60),
            ..Default::default()
        };
        let header = cc.to_string();
        // elapsed is ~10s, so max-age should be ~50
        let emitted: u64 = header
            .split(',')
            .find(|d| d.starts_with("max-age="))
            .and_then(|d| d.trim_start_matches("max-age=").parse().ok())
            .unwrap();
        assert!(emitted <= 50 && emitted >= 48, "expected ~50, got {emitted}");
    }

    #[test]
    fn display_no_store_suppresses_other_directives() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now,
            no_store: true,
            max_age: Some(60),
            no_cache: true,
            ..Default::default()
        };
        assert_eq!(cc.to_string(), "no-store");
    }

    // --- with_default_ttl ---

    #[test]
    fn with_default_ttl_sets_max_age_when_absent() {
        let cc = CacheControl::default().with_default_ttl(Some(Duration::from_secs(60)));
        assert_eq!(cc.max_age, Some(60));
    }

    #[test]
    fn with_default_ttl_does_not_override_existing_max_age() {
        let cc = CacheControl {
            max_age: Some(30),
            ..Default::default()
        }
        .with_default_ttl(Some(Duration::from_secs(60)));
        assert_eq!(cc.max_age, Some(30));
    }

    #[test]
    fn with_default_ttl_no_op_when_no_store() {
        let cc = CacheControl::default_no_store().with_default_ttl(Some(Duration::from_secs(60)));
        assert_eq!(cc.max_age, None);
    }

    // --- should_store ---

    #[test]
    fn should_store_true_when_fresh() {
        let cc = CacheControl {
            max_age: Some(60),
            ..Default::default()
        };
        assert!(cc.should_store());
    }

    #[test]
    fn should_store_true_when_no_max_age() {
        // No max_age means no expiry, so should still store
        let cc = CacheControl::default();
        assert!(cc.should_store());
    }

    #[test]
    fn should_store_false_when_no_store() {
        assert!(!CacheControl::default_no_store().should_store());
    }

    #[test]
    fn should_store_false_when_ttl_zero() {
        // Already expired at time of receipt (age >= max_age)
        let cc = CacheControl {
            max_age: Some(10),
            age: Some(10),
            ..Default::default()
        };
        assert!(!cc.should_store());
    }

    // --- update_headers ---

    #[test]
    fn update_headers_sets_cache_control() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now,
            max_age: Some(60),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();
        cc.update_headers(&mut headers).unwrap();
        assert!(headers.contains_key(http::header::CACHE_CONTROL));
        let value = headers[http::header::CACHE_CONTROL].to_str().unwrap();
        assert!(value.contains("max-age="), "got: {value}");
    }

    #[test]
    fn update_headers_sets_age_when_positive() {
        let cc = CacheControl {
            age: Some(15),
            max_age: Some(60),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();
        cc.update_headers(&mut headers).unwrap();
        assert!(headers.contains_key(http::header::AGE));
    }

    #[test]
    fn update_headers_omits_age_when_zero() {
        let cc = CacheControl {
            age: Some(0),
            max_age: Some(60),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();
        cc.update_headers(&mut headers).unwrap();
        assert!(!headers.contains_key(http::header::AGE));
    }

    // --- merge_without_ttl_update ---

    #[test]
    fn merge_without_ttl_update_ignores_elapsed() {
        // The key distinction between merge and merge_without_ttl_update:
        // merge subtracts elapsed time since creation; merge_without_ttl_update does not.
        // This matters for the telemetry use case where we want to report the original TTL.
        let now = now_epoch_seconds();
        let first = CacheControl {
            created: now - 10,
            max_age: Some(60),
            ..Default::default()
        };
        let second = CacheControl {
            created: now - 10,
            max_age: Some(60),
            ..Default::default()
        };

        // merge accounts for elapsed: 60 - 10s elapsed = 50
        let with_update = first.merge_inner(&second, now, true);
        assert_eq!(with_update.ttl(), Some(50));

        // merge_without_ttl_update ignores elapsed: stays at 60
        let without_update = first.merge_inner(&second, now, false);
        assert_eq!(without_update.ttl(), Some(60));
    }

    #[test]
    fn merge_without_ttl_update_still_takes_minimum() {
        let now = now_epoch_seconds();
        let first = CacheControl {
            created: now,
            max_age: Some(30),
            ..Default::default()
        };
        let second = CacheControl {
            created: now,
            max_age: Some(60),
            ..Default::default()
        };
        let merged = first.merge_inner(&second, now, false);
        assert_eq!(merged.ttl(), Some(30));
    }

    // --- remaining_ttl (public, time-relative) ---

    #[test]
    fn remaining_ttl_accounts_for_elapsed() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now - 10,
            max_age: Some(60),
            ..Default::default()
        };
        // ttl() = 60 (age-relative only, ignores elapsed)
        assert_eq!(cc.ttl(), Some(60));
        // remaining_ttl() = 60 - 10s elapsed = ~50
        let remaining = cc.remaining_ttl().unwrap();
        assert!(remaining <= 50 && remaining >= 48, "expected ~50, got {remaining}");
    }

    #[test]
    fn remaining_ttl_accounts_for_age_and_elapsed() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now - 5,
            max_age: Some(60),
            age: Some(10),
            ..Default::default()
        };
        // remaining_ttl() = 60 - 10(age) - ~5(elapsed) = ~45
        let remaining = cc.remaining_ttl().unwrap();
        assert!(remaining <= 45 && remaining >= 43, "expected ~45, got {remaining}");
    }

    #[test]
    fn remaining_ttl_returns_zero_when_expired() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now,
            max_age: Some(10),
            age: Some(20), // age > max_age: already expired at receipt
            ..Default::default()
        };
        assert_eq!(cc.remaining_ttl(), Some(0));
    }

    #[test]
    fn remaining_ttl_returns_none_without_max_age() {
        let cc = CacheControl::default();
        assert_eq!(cc.remaining_ttl(), None);
    }

    // --- merge_no_store ---

    #[test]
    fn merge_no_store_propagates_from_other() {
        let mut cc = CacheControl {
            max_age: Some(60),
            ..Default::default()
        };
        cc.merge_no_store(&CacheControl::default_no_store());
        assert!(cc.no_store());
        // Other fields should be unaffected
        assert_eq!(cc.max_age, Some(60));
    }

    #[test]
    fn merge_no_store_no_op_when_other_is_not_no_store() {
        let mut cc = CacheControl {
            max_age: Some(60),
            ..Default::default()
        };
        cc.merge_no_store(&CacheControl::default());
        assert!(!cc.no_store());
        assert_eq!(cc.max_age, Some(60));
    }

    // --- max_age() getter ---

    #[test]
    fn max_age_getter_prefers_s_max_age() {
        let cc = CacheControl {
            max_age: Some(60),
            s_max_age: Some(30),
            ..Default::default()
        };
        assert_eq!(cc.max_age(), Some(30));
    }

    #[test]
    fn max_age_getter_falls_back_to_max_age() {
        let cc = CacheControl {
            max_age: Some(60),
            s_max_age: None,
            ..Default::default()
        };
        assert_eq!(cc.max_age(), Some(60));
    }

    #[test]
    fn max_age_getter_returns_none_when_neither_set() {
        assert_eq!(CacheControl::default().max_age(), None);
    }
}
