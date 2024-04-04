use std::fmt::Write;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use http::header::AGE;
use http::header::CACHE_CONTROL;
use http::HeaderMap;
use http::HeaderValue;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CacheControl {
    created: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    max_age: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    age: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    s_max_age: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    stale_while_revalidate: Option<u32>,
    #[serde(skip_serializing_if = "is_false", default)]
    no_cache: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    must_revalidate: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    proxy_revalidate: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    pub(super) no_store: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    private: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    public: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    must_understand: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    no_transform: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    immutable: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    stale_if_error: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("we should not run before EPOCH")
        .as_secs()
}

impl Default for CacheControl {
    fn default() -> Self {
        Self {
            created: now_epoch_seconds(),
            max_age: None,
            age: None,
            s_max_age: None,
            stale_while_revalidate: None,
            no_cache: false,
            must_revalidate: false,
            proxy_revalidate: false,
            no_store: false,
            private: false,
            public: false,
            must_understand: false,
            no_transform: false,
            immutable: false,
            stale_if_error: false,
        }
    }
}

impl CacheControl {
    pub(crate) fn new(
        headers: &HeaderMap,
        default_ttl: Option<Duration>,
    ) -> Result<Self, BoxError> {
        let mut result = CacheControl::default();
        if let Some(duration) = default_ttl {
            result.max_age = Some(duration.as_secs() as u32);
        }

        for header_value in headers.get_all(CACHE_CONTROL) {
            for value in header_value.to_str()?.split(',') {
                let mut it = value.trim().split('=');
                let (k, v) = (it.next(), it.next());
                if k.is_none() || it.next().is_some() {
                    return Err("invalid Cache-Control header value".into());
                }

                match (k.expect("the key was checked"), v) {
                    ("max-age", Some(v)) => {
                        result.max_age = Some(v.parse()?);
                    }
                    ("s-maxage", Some(v)) => {
                        result.s_max_age = Some(v.parse()?);
                    }
                    ("stale-while-revalidate", Some(v)) => {
                        result.stale_while_revalidate = Some(v.parse()?);
                    }
                    ("no-cache", None) => {
                        result.no_cache = true;
                    }
                    ("must-revalidate", None) => {
                        result.must_revalidate = true;
                    }
                    ("proxy-revalidate", None) => {
                        result.proxy_revalidate = true;
                    }
                    ("no-store", None) => {
                        result.no_store = true;
                    }
                    ("private", None) => {
                        result.private = true;
                    }
                    ("public", None) => {
                        result.public = true;
                    }
                    ("must-understand", None) => {
                        result.must_understand = true;
                    }
                    ("no-transform", None) => {
                        result.no_transform = true;
                    }
                    ("immutable", None) => {
                        result.immutable = true;
                    }
                    ("stale-if-error", None) => {
                        result.stale_if_error = true;
                    }
                    _ => {
                        return Err("invalid Cache-Control header value".into());
                    }
                }
            }
        }

        if let Some(value) = headers.get("Age") {
            result.age = Some(value.to_str()?.trim().parse()?);
        }

        //TODO etag

        Ok(result)
    }

    pub(crate) fn to_headers(&self, headers: &mut HeaderMap) -> Result<(), BoxError> {
        let mut s = String::new();
        let mut prev = false;
        if let Some(max_age) = self.max_age {
            write!(&mut s, "{}max-age={}", if prev { "," } else { "" }, max_age)?;
            prev = true;
        }
        if let Some(s_max_age) = self.s_max_age {
            write!(
                &mut s,
                "{}s-maxage={}",
                if prev { "," } else { "" },
                s_max_age
            )?;
            prev = true;
        }
        if let Some(swr) = self.stale_while_revalidate {
            write!(
                &mut s,
                "{}stale-while-revalidate={}",
                if prev { "," } else { "" },
                swr
            )?;
            prev = true;
        }
        if self.no_cache {
            write!(&mut s, "{}no_cache", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.must_revalidate {
            write!(&mut s, "{}must-revalidate", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.proxy_revalidate {
            write!(&mut s, "{}proxy-revalidate", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.no_store {
            write!(&mut s, "{}no-store", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.private {
            write!(&mut s, "{}private", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.public && !self.private {
            write!(&mut s, "{}public", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.must_understand {
            write!(&mut s, "{}must-understand", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.no_transform {
            write!(&mut s, "{}no-transform", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.immutable {
            write!(&mut s, "{}immutable", if prev { "," } else { "" },)?;
            prev = true;
        }
        if self.stale_if_error {
            write!(&mut s, "{}stale-if-error", if prev { "," } else { "" },)?;
        }
        headers.insert(CACHE_CONTROL, HeaderValue::from_str(&s)?);

        if let Some(age) = self.age {
            if age != 0 {
                headers.insert(AGE, age.into());
            }
        }

        Ok(())
    }

    pub(crate) fn merge(&self, other: &CacheControl) -> CacheControl {
        CacheControl {
            created: std::cmp::min(self.created, other.created),
            max_age: match (self.ttl(), other.ttl()) {
                (None, None) => None,
                (None, Some(ttl)) => Some(ttl),
                (Some(ttl), None) => Some(ttl),
                (Some(ttl1), Some(ttl2)) => Some(std::cmp::min(ttl1, ttl2)),
            },
            age: None,
            s_max_age: None,
            stale_while_revalidate: match (
                self.stale_while_revalidate,
                other.stale_while_revalidate,
            ) {
                (None, None) => None,
                (None, Some(ttl)) => Some(ttl),
                (Some(ttl), None) => Some(ttl),
                (Some(ttl1), Some(ttl2)) => Some(std::cmp::min(ttl1, ttl2)),
            },
            no_cache: self.no_cache || other.no_cache,
            must_revalidate: self.must_revalidate || other.must_revalidate,
            proxy_revalidate: self.proxy_revalidate || other.proxy_revalidate,
            no_store: self.no_store || other.no_store,
            private: self.private || other.private,
            // private takes precedence over public
            public: if self.private || other.private {
                false
            } else {
                self.public || other.public
            },
            must_understand: self.must_understand || other.must_understand,
            no_transform: self.no_transform || other.no_transform,
            immutable: self.immutable || other.immutable,
            stale_if_error: self.stale_if_error || other.stale_if_error,
        }
    }

    pub(crate) fn ttl(&self) -> Option<u32> {
        match (
            self.s_max_age.as_ref().or(self.max_age.as_ref()),
            self.age.as_ref(),
        ) {
            (None, _) => None,
            (Some(max_age), None) => Some(*max_age),
            (Some(max_age), Some(age)) => Some(max_age - age),
        }
    }

    pub(crate) fn should_store(&self) -> bool {
        // FIXME: should we add support for must-understand?
        // public will be the default case
        !self.no_store
    }

    pub(crate) fn private(&self) -> bool {
        self.private
    }

    // We don't support revalidation yet
    #[allow(dead_code)]
    pub(crate) fn should_revalidate(&self) -> bool {
        if self.no_cache {
            return true;
        }

        let elapsed = now_epoch_seconds() - self.created;
        let expired = self
            .ttl()
            .map(|ttl| (ttl as u64) < elapsed)
            .unwrap_or(false);

        if self.immutable && !expired {
            return false;
        }

        if (self.must_revalidate || self.proxy_revalidate) && expired {
            return true;
        }

        false
    }

    pub(crate) fn can_use(&self) -> bool {
        let elapsed = now_epoch_seconds() - self.created;
        let expired = self
            .ttl()
            .map(|ttl| (ttl as u64) < elapsed)
            .unwrap_or(false);

        // FIXME: we don't honor stale-while-revalidate yet
        // !expired || self.stale_while_revalidate
        !expired
    }
}
