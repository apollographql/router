use std::fmt::Display;
use std::time::Duration;

use clap::builder::TypedValueParser;
use http::HeaderMap;
use http::HeaderValue;
use http::header::AGE;
use http::header::CACHE_CONTROL;
use tower::BoxError;

use crate::plugins::cache::cache_control::parse_directive;
use crate::plugins::cache::cache_control::remaining_ttl;
use crate::plugins::response_cache::cache_control::delimitted_formatter::DelimitedFormatter;
use crate::plugins::response_cache::cache_control::now_epoch_seconds;

/// Cache control header either:
/// * Returned from subgraph to router to control response cache
/// * Returned from router to client to control external cache (ie CDN)
pub(crate) struct CacheControl {
    created: u64,

    // Not actually part of the header, used to offset the max_age
    age: Option<u64>,

    /// Indicates that the response remains fresh until N seconds after the response is generated
    max_age: Option<u64>,

    /// Indicates how long the response remains fresh in a shared cache. Overrides the value specified by max-age.
    s_max_age: Option<u64>,

    /// Indicates that the response can be stored in caches, but the response must be validated with the origin server before each reuse, even when the cache is disconnected from the origin server.
    no_cache: bool,

    /// Indicates that caches of any kind should not store this response.
    no_store: bool,

    /// Indicates that any intermediary (regardless of whether it implements a cache) shouldn't transform the response contents.
    /// TODO: not sure if used by the router
    no_transform: bool,

    /// Indicates that the response can be stored in caches and can be reused while fresh. If the response becomes stale, it must be validated with the origin server before reuse.
    must_revalidate: bool,

    /// The equivalent of `must-revalidate`, but specifically for shared caches.
    proxy_revalidate: bool,

    /// Indicates that a cache should store the response only if it understands the requirements for caching based on status code.
    /// TODO: not sure what this means for our use case
    must_understand: bool,

    /// Indicates that the response can be stored only in a private cache (e.g., local caches in browsers).
    private: bool,

    /// Indicates that the response can be stored in a shared cache (i.e., despite the presence of the `Authorization` header).
    public: bool,

    /// Indicates that the response will not be updated while it's fresh.
    /// TODO: not sure if used by the router
    immutable: bool,

    /// Indicates that the cache could reuse a stale response while it revalidates it to a cache.
    /// TODO: not sure if used by the router
    stale_while_revalidate: Option<u64>,

    /// Indicates that the cache can reuse a stale response when an upstream server generates an error, or when the error is generated locally. Here, an error is considered any response with a status code of 500, 502, 503, or 504.
    /// TODO: not sure if used by the router
    stale_if_error: Option<u64>,
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
        Ok(())
    }
}

impl CacheControl {
    // TODO better docstring - set max-age based on ttl if max-age not already present
    fn with_default_ttl(mut self, ttl: Option<Duration>) -> Self {
        let ttl: Option<u64> = ttl.map(Duration::as_secs);
        self.max_age = self.max_age.or(ttl);

        self
    }

    fn elapsed(&self) -> u64 {
        now_epoch_seconds().saturating_sub(self.created)
    }

    fn update_headers(&self, headers: &mut HeaderMap) -> Result<(), BoxError> {
        headers.insert(CACHE_CONTROL, self.to_string().into());

        if let Some(age) = self.age
            && age > 0
        {
            headers.insert(AGE, age.into());
        }

        Ok(())
    }
}
