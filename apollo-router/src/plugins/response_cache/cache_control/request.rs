// TODO: may need to be able to serialize and deserialize, probably with
// #[serde(skip_serializing_if = "Option::is_none", default)]
// #[serde(skip_serializing_if = "is_false", default)]

use std::fmt::Display;

use http::HeaderMap;
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use tower::BoxError;

use super::delimitted_formatter::DelimitedFormatter;
use super::now_epoch_seconds;
use super::parse_directive;
use super::remaining_ttl;

/// Cache control header either:
/// * Sent from client to router to control response cache
/// * Sent from router to subgraph to control external cache (ie CDN)
pub(crate) struct CacheControl {
    // TODO docstring - not a real field, metadata we use
    created: u64,

    /// Indicates that the client allows a stored response that is generated on the origin server within N seconds.
    max_age: Option<u64>,

    /// Indicates that the client allows a stored response that is stale within N seconds.
    max_stale: Option<u64>,

    /// Indicates that the client allows a stored response that is fresh for at least N seconds
    min_fresh: Option<u64>,

    /// Asks cache to validate the response with the origin server before reuse.
    no_cache: bool,

    /// Asks cache to refrain from storing the request and corresponding response — even if the origin server's response could be stored.
    no_store: bool,

    /// Indicates that any intermediary (regardless of whether it implements a cache) shouldn't transform the response contents.
    /// TODO: not sure if used by the router
    no_transform: bool,

    /// Indicates that an already-cached response should be returned. If a cache has a stored response, even a stale one, it will be returned. If no cached response is available, a 504 Gateway Timeout response will be returned.
    only_if_cached: bool,

    /// Indicates that the browser is interested in receiving stale content on error from any intermediate server for a particular origin.
    /// TODO: apparently this is not supported by any browser.
    stale_if_error: bool,
}

impl Default for CacheControl {
    fn default() -> Self {
        Self {
            created: now_epoch_seconds(),
            max_age: None,
            max_stale: None,
            min_fresh: None,
            no_cache: false,
            no_store: false,
            no_transform: false,
            only_if_cached: false,
            stale_if_error: false,
        }
    }
}

impl TryFrom<&HeaderMap> for CacheControl {
    type Error = BoxError;

    fn try_from(headers: &HeaderMap) -> Result<Self, Self::Error> {
        let mut cache_control = Self::default();

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
                    ("max-stale", Some(value)) => {
                        cache_control.max_stale = Some(value.parse()?);
                    }
                    ("min-fresh", Some(value)) => {
                        cache_control.min_fresh = Some(value.parse()?);
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
                    ("only-if-cached", None) => {
                        cache_control.only_if_cached = true;
                    }
                    ("stale-if-error", None) => {
                        cache_control.stale_if_error = true;
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

        if self.no_cache {
            write!(&mut formatter, "no-cache")?;
        }

        if let Some(max_age) = self.max_age {
            let max_age = remaining_ttl(max_age, elapsed);
            write!(&mut formatter, "max-age={}", max_age)?;
        }

        if let Some(max_stale) = self.max_stale {
            let max_stale = remaining_ttl(max_stale, elapsed);
            write!(&mut formatter, "max-stale={}", max_stale)?;
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

        if self.stale_if_error {
            write!(&mut formatter, "stale-if-error")?;
        }

        Ok(())
    }
}

impl CacheControl {
    fn elapsed(&self) -> u64 {
        now_epoch_seconds().saturating_sub(self.created)
    }

    fn update_headers(&self, headers: &mut HeaderMap) -> Result<(), BoxError> {
        headers.insert(CACHE_CONTROL, self.to_string().into());

        Ok(())
    }
}
