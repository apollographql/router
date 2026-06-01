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

/// Represents a parsed `Cache-Control` header, used in both request and response contexts:
///
/// - **Request**: sent from the client to the router, or from the router to a subgraph,
///   to control how caches should handle the request.
/// - **Response**: returned from a subgraph to the router, or from the router to the client,
///   to control how caches should store and serve the response.
///
/// Fields are annotated to indicate whether they are request-only, response-only, or shared.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheControl {
    // -- shared between request and response --

    /// Unix timestamp (seconds) at which this struct was created. Used to compute elapsed time
    /// for TTL calculations.
    created: u64,

    /// `max-age=N`: in a response, indicates the response remains fresh for N seconds after it
    /// was generated. In a request, indicates the client prefers a response whose age is less
    /// than or equal to N seconds (RFC 9111 §5.2.1.1, §5.2.2.1).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    max_age: Option<u64>,

    /// `no-cache`: the cache must revalidate with the origin before serving a stored response.
    ///
    /// In a request, asks caches to bypass stored responses and revalidate.
    /// In a response, requires revalidation before each reuse even when disconnected from origin.
    #[serde(skip_serializing_if = "is_false", default)]
    no_cache: bool,

    /// `no-store`: caches must not store the request or response.
    ///
    /// In a request, asks caches to refrain from storing the request and corresponding response.
    /// In a response, indicates that caches of any kind should not store this response.
    // TODO: remove no_store pub facing
    #[serde(skip_serializing_if = "is_false", default)]
    no_store: bool,

    /// `no-transform`: intermediaries must not transform the content, in both request and response
    /// contexts (RFC 9111 §5.2.1.6, §5.2.2.6).
    ///
    /// The router does not transform content, so this directive does not affect its caching
    /// behavior. It is parsed and propagated to the client for downstream caches to honor.
    #[serde(skip_serializing_if = "is_false", default)]
    no_transform: bool,

    /// `stale-if-error=N`: the cache may reuse a stale response when an upstream error occurs
    /// (HTTP 500, 502, 503, or 504), for up to N seconds after the response went stale.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    stale_if_error: Option<u64>,

    // -- request only --

    /// `max-stale[=N]`: the client accepts a stale response, optionally capped at N seconds past
    /// expiry. A bare `max-stale` (no value) is stored as `u64::MAX` (~584 billion years).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    max_stale: Option<u64>,

    /// `min-fresh=N`: the client requires a response that will remain fresh for at least N more seconds.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    min_fresh: Option<u64>,

    /// `only-if-cached`: the client wants a stored response; returns 504 if none is available.
    #[serde(skip_serializing_if = "is_false", default)]
    only_if_cached: bool,

    // -- response only --

    /// Value of the `Age` response header, indicating how many seconds old the response is.
    /// Used to offset `max_age` when computing the remaining TTL.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    age: Option<u64>,

    /// `s-maxage=N`: overrides `max-age` for shared caches. Takes precedence over `max-age`
    /// in [`max_age()`] and TTL calculations since the router acts as a shared cache.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    s_max_age: Option<u64>,

    /// `must-revalidate`: once stale, the cache must not serve the response without revalidating.
    #[serde(skip_serializing_if = "is_false", default)]
    must_revalidate: bool,

    /// `proxy-revalidate`: equivalent to `must-revalidate` but applies only to shared caches.
    #[serde(skip_serializing_if = "is_false", default)]
    proxy_revalidate: bool,

    /// `must-understand`: the cache should only store the response if it understands the
    /// caching requirements for the response's status code. The router does not enforce this;
    /// it is parsed and propagated to the client.
    #[serde(skip_serializing_if = "is_false", default)]
    must_understand: bool,

    /// `private`: the response MUST NOT be stored by a shared cache; it may be stored in a
    /// private cache (e.g., a browser cache). Takes precedence over `public` — see [`public()`].
    /// (RFC 9111 §5.2.2.7)
    #[serde(skip_serializing_if = "is_false", default)]
    private: bool,

    /// `public`: the response may be stored in a shared cache even when it would otherwise
    /// not be cacheable (e.g., when an `Authorization` header is present).
    #[serde(skip_serializing_if = "is_false", default)]
    public: bool,

    /// `immutable`: indicates the response body will not change while it is fresh.
    /// The router does not currently use this to skip revalidation; it is parsed and propagated.
    #[serde(skip_serializing_if = "is_false", default)]
    immutable: bool,

    /// `stale-while-revalidate=N`: the cache may serve a stale response for up to N seconds
    /// while revalidating in the background. The router does not currently honor this directive.
    ///
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

impl CacheControl {
    /// Formats this cache control as a `Cache-Control` response header value.
    ///
    /// TTL fields (`max-age`, `s-maxage`, `stale-while-revalidate`, `stale-if-error`) are
    /// decremented by elapsed time since this struct was created, so the emitted value reflects
    /// the remaining freshness at the time of serialization.
    ///
    /// Request-only directives (`min-fresh`, `max-stale`, `only-if-cached`) are intentionally
    /// omitted — they have no meaning in a response header.
    pub(crate) fn to_response_header_value(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // If no-store, emit just that directive. Per RFC 9111, no-store is the strongest cache
        // directive and makes all others irrelevant. Note that max-age=0 is intentionally not
        // treated as no-store: max-age=0 means "expired, always revalidate", whereas no-store
        // means "do not cache at all". The router determines cacheability via should_store().
        // See: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#preventing_storing
        if self.no_store {
            return "no-store".to_string();
        }

        let elapsed = self.elapsed();

        if let Some(max_age) = self.max_age {
            parts.push(format!("max-age={}", remaining_ttl(max_age, elapsed)));
        }
        if let Some(s_max_age) = self.s_max_age {
            parts.push(format!("s-maxage={}", remaining_ttl(s_max_age, elapsed)));
        }
        if self.no_cache {
            parts.push("no-cache".to_string());
        }
        if self.no_transform {
            parts.push("no-transform".to_string());
        }
        if self.must_revalidate {
            parts.push("must-revalidate".to_string());
        }
        if self.proxy_revalidate {
            parts.push("proxy-revalidate".to_string());
        }
        if self.must_understand {
            parts.push("must-understand".to_string());
        }
        if self.private {
            parts.push("private".to_string());
        }
        if self.public {
            parts.push("public".to_string());
        }
        if self.immutable {
            parts.push("immutable".to_string());
        }
        if let Some(stale) = self.stale_while_revalidate {
            parts.push(format!(
                "stale-while-revalidate={}",
                remaining_ttl(stale, elapsed)
            ));
        }
        if let Some(stale) = self.stale_if_error {
            parts.push(format!("stale-if-error={}", remaining_ttl(stale, elapsed)));
        }

        parts.join(",")
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

    /// Returns the number of seconds elapsed since this struct was created.
    fn elapsed(&self) -> u64 {
        now_epoch_seconds().saturating_sub(self.created)
    }

    /// Writes the `Cache-Control` (and `Age` if applicable) headers into the given header map.
    pub(crate) fn update_response_headers(&self, headers: &mut HeaderMap) -> Result<(), BoxError> {
        headers.insert(
            CACHE_CONTROL,
            HeaderValue::from_str(&self.to_response_header_value())?,
        );

        if let Some(age) = self.age
            && age > 0
        {
            headers.insert(AGE, age.into());
        }

        Ok(())
    }

    /// Propagates `no_store` from `other` into `self`, leaving all other fields unchanged.
    /// Used to apply a request's `no-store` directive to the accumulated response cache control.
    pub(crate) fn merge_no_store(&mut self, other: &Self) {
        self.no_store |= other.no_store;
    }

    /// Merges two `CacheControl` values, taking the most restrictive of each directive.
    /// TTL fields are decremented by elapsed time so the result reflects freshness as of now.
    pub(crate) fn merge(&self, other: &Self) -> Self {
        self.merge_inner(other, now_epoch_seconds(), true)
    }

    /// Like [`merge`], but does not account for elapsed time when computing remaining TTLs.
    /// Used when merging cached entries for telemetry, where the original TTL should be preserved.
    pub(crate) fn merge_without_ttl_update(&self, other: &Self) -> Self {
        self.merge_inner(other, now_epoch_seconds(), false)
    }

    fn merge_inner(&self, other: &Self, now_epoch: u64, update_ttl: bool) -> Self {
        // If either side has no-store, the merged result is no-store only. Per RFC 9111, no-store
        // is the strongest cache directive and makes all others irrelevant.
        // See: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#preventing_storing
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

impl CacheControl {
    /// Returns `true` if the `no-cache` directive is set.
    pub(crate) fn no_cache(&self) -> bool {
        self.no_cache
    }

    /// Returns `true` if the `no-store` directive is set.
    pub(crate) fn no_store(&self) -> bool {
        self.no_store
    }

    /// Returns `true` if the `private` directive is set.
    pub(crate) fn private(&self) -> bool {
        self.private
    }

    /// Returns `true` if the `public` directive is set.
    /// Note: `private` takes precedence — both will not be true simultaneously.
    pub(crate) fn public(&self) -> bool {
        self.public
    }

    /// Returns `true` if this response should be stored in the cache.
    /// A response should be stored if `no-store` is not set and the TTL (if present) is > 0.
    // TODO: consider using this as the inverse of no_store and replacing no_store with no_store_raw
    pub(crate) fn should_store(&self) -> bool {
        !self.no_store && self.ttl().is_none_or(|ttl| ttl > 0)
    }

    /// Returns `true` if a cached response can be served to the client right now.
    /// A response can be used if `no-cache` is not set and the remaining TTL (if present) is > 0.
    // TODO: honor stale-while-revalidate
    pub(crate) fn can_use(&self) -> bool {
        !self.no_cache && self.remaining_ttl().is_none_or(|ttl| ttl > 0)
    }

    /// Returns the value of the `Age` header, if present.
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

impl CacheControl {
    /// Computes a remaining duration in seconds, subtracting `age` and optionally elapsed time.
    ///
    /// If `now` is `Some`, elapsed time since `created` is also subtracted. If `None`, only
    /// the `Age` header offset is applied (useful for merge operations that don't update the TTL).
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

/// Returns the smaller of two optional values. If one is `None`, the other is returned.
fn minimum_optional_value<T: Ord>(x: Option<T>, y: Option<T>) -> Option<T> {
    match (x, y) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(std::cmp::min(x, y)),
    }
}

/// Returns the larger of two optional values. If one is `None`, the other is returned.
fn maximum_optional_value<T: Ord>(x: Option<T>, y: Option<T>) -> Option<T> {
    match (x, y) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(std::cmp::max(x, y)),
    }
}

/// Returns the current time as seconds since the Unix epoch.
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
        let cache_control_header = first.to_response_header_value();
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
        let header = cc.to_response_header_value();
        assert!(header.contains("s-maxage=30"), "got: {header}");
        assert!(header.contains("max-age=60"), "got: {header}");
    }

    #[test]
    fn s_maxage_round_trip() {
        // Parse s-maxage from a header, serialize back, confirm it's preserved
        let cc =
            CacheControl::try_from(&header_map(&[("cache-control", "s-maxage=30,max-age=60")])).unwrap();
        let header = cc.to_response_header_value();
        assert!(header.contains("s-maxage="), "got: {header}");
        assert!(header.contains("max-age="), "got: {header}");
    }

    // --- Display ---

    #[test]
    fn response_header_decrements_max_age_by_elapsed() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now - 10,
            max_age: Some(60),
            ..Default::default()
        };
        let header = cc.to_response_header_value();
        // elapsed is ~10s, so max-age should be ~50
        let emitted: u64 = header
            .split(',')
            .find(|d| d.starts_with("max-age="))
            .and_then(|d| d.trim_start_matches("max-age=").parse().ok())
            .unwrap();
        assert!(emitted <= 50 && emitted >= 48, "expected ~50, got {emitted}");
    }

    #[test]
    fn response_header_no_store_suppresses_other_directives() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now,
            no_store: true,
            max_age: Some(60),
            no_cache: true,
            ..Default::default()
        };
        assert_eq!(cc.to_response_header_value(), "no-store");
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

    // --- update_response_headers ---

    #[test]
    fn update_response_headers_sets_cache_control() {
        let now = now_epoch_seconds();
        let cc = CacheControl {
            created: now,
            max_age: Some(60),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();
        cc.update_response_headers(&mut headers).unwrap();
        assert!(headers.contains_key(http::header::CACHE_CONTROL));
        let value = headers[http::header::CACHE_CONTROL].to_str().unwrap();
        assert!(value.contains("max-age="), "got: {value}");
    }

    #[test]
    fn update_response_headers_sets_age_when_positive() {
        let cc = CacheControl {
            age: Some(15),
            max_age: Some(60),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();
        cc.update_response_headers(&mut headers).unwrap();
        assert!(headers.contains_key(http::header::AGE));
    }

    #[test]
    fn update_response_headers_omits_age_when_zero() {
        let cc = CacheControl {
            age: Some(0),
            max_age: Some(60),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();
        cc.update_response_headers(&mut headers).unwrap();
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
