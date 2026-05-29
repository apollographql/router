mod combined;
mod delimitted_formatter;

use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub(crate) use combined::CacheControl;
use tower::BoxError;

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
//
// #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
// #[serde(rename_all = "camelCase")]
// pub(crate) struct CacheControl {
//     created: u64,
//     #[serde(skip_serializing_if = "Option::is_none", default)]
//     max_age: Option<u64>,
//     #[serde(skip_serializing_if = "Option::is_none", default)]
//     age: Option<u64>,
//     #[serde(skip_serializing_if = "Option::is_none", default)]
//     s_max_age: Option<u64>,
//     #[serde(skip_serializing_if = "Option::is_none", default)]
//     stale_while_revalidate: Option<u64>,
//     #[serde(skip_serializing_if = "is_false", default)]
//     no_cache: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     must_revalidate: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     proxy_revalidate: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     pub(super) no_store: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     private: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     public: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     must_understand: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     no_transform: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     immutable: bool,
//     #[serde(skip_serializing_if = "is_false", default)]
//     stale_if_error: bool,
// }
//
// fn is_false(b: &bool) -> bool {
//     !b
// }
//
// impl Default for CacheControl {
//     fn default() -> Self {
//         Self {
//             created: now_epoch_seconds(),
//             max_age: None,
//             age: None,
//             s_max_age: None,
//             stale_while_revalidate: None,
//             no_cache: false,
//             must_revalidate: false,
//             proxy_revalidate: false,
//             no_store: false,
//             private: false,
//             public: false,
//             must_understand: false,
//             no_transform: false,
//             immutable: false,
//             stale_if_error: false,
//         }
//     }
// }
//
// impl CacheControl {
//     pub(crate) fn new(
//         headers: &HeaderMap,
//         default_ttl: Option<Duration>,
//     ) -> Result<Self, BoxError> {
//         let mut result = CacheControl::default();
//         if let Some(duration) = default_ttl {
//             result.max_age = Some(duration.as_secs());
//         }
//
//         let mut found = false;
//         for header_value in headers.get_all(CACHE_CONTROL) {
//             found = true;
//             for value in header_value.to_str()?.split(',') {
//                 let mut it = value.trim().split('=');
//                 let (k, v) = (it.next(), it.next());
//                 if k.is_none() || it.next().is_some() {
//                     return Err("invalid Cache-Control header value".into());
//                 }
//
//                 match (k.expect("the key was checked"), v) {
//                     ("max-age", Some(v)) => {
//                         result.max_age = Some(v.parse()?);
//                     }
//                     ("s-maxage", Some(v)) => {
//                         result.s_max_age = Some(v.parse()?);
//                     }
//                     ("stale-while-revalidate", Some(v)) => {
//                         result.stale_while_revalidate = Some(v.parse()?);
//                     }
//                     ("no-cache", None) => {
//                         result.no_cache = true;
//                     }
//                     ("must-revalidate", None) => {
//                         result.must_revalidate = true;
//                     }
//                     ("proxy-revalidate", None) => {
//                         result.proxy_revalidate = true;
//                     }
//                     ("no-store", None) => {
//                         result.no_store = true;
//                     }
//                     ("private", None) => {
//                         result.private = true;
//                     }
//                     ("public", None) => {
//                         result.public = true;
//                     }
//                     ("must-understand", None) => {
//                         result.must_understand = true;
//                     }
//                     ("no-transform", None) => {
//                         result.no_transform = true;
//                     }
//                     ("immutable", None) => {
//                         result.immutable = true;
//                     }
//                     ("stale-if-error", None) => {
//                         result.stale_if_error = true;
//                     }
//                     _ => {
//                         return Err("invalid Cache-Control header value".into());
//                     }
//                 }
//             }
//         }
//
//         if !found {
//             result.no_store = true;
//         }
//
//         if let Some(value) = headers.get(http::header::AGE) {
//             result.age = Some(value.to_str()?.trim().parse()?);
//         }
//
//         //TODO etag
//
//         Ok(result)
//     }
//
//     /// Fill the header map with cache-control header and age header
//     pub(crate) fn to_headers(&self, headers: &mut HeaderMap) -> Result<(), BoxError> {
//         headers.insert(
//             CACHE_CONTROL,
//             HeaderValue::from_str(&self.to_cache_control_header()?)?,
//         );
//
//         if let Some(age) = self.age
//             && age != 0
//         {
//             headers.insert(AGE, age.into());
//         }
//
//         Ok(())
//     }
//
//     /// Only for cache control header and not age
//     pub(crate) fn to_cache_control_header(&self) -> Result<String, BoxError> {
//         let mut s = String::new();
//         let mut prev = false;
//         let now = now_epoch_seconds();
//         if self.no_store {
//             write!(&mut s, "no-store")?;
//             // Early return to avoid conflicts https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#preventing_storing
//             return Ok(s);
//         }
//         if self.no_cache {
//             write!(&mut s, "{}no-cache", if prev { "," } else { "" },)?;
//             prev = true;
//         }
//         if let Some(max_age) = self.max_age {
//             //FIXME: write no-store if max_age = 0?
//             write!(
//                 &mut s,
//                 "{}max-age={}",
//                 if prev { "," } else { "" },
//                 self.update_ttl(max_age, now)
//             )?;
//             prev = true;
//         }
//         if let Some(s_max_age) = self.s_max_age {
//             write!(
//                 &mut s,
//                 "{}s-maxage={}",
//                 if prev { "," } else { "" },
//                 self.update_ttl(s_max_age, now)
//             )?;
//             prev = true;
//         }
//         if let Some(swr) = self.stale_while_revalidate {
//             write!(
//                 &mut s,
//                 "{}stale-while-revalidate={}",
//                 if prev { "," } else { "" },
//                 swr
//             )?;
//             prev = true;
//         }
//         if self.must_revalidate {
//             write!(&mut s, "{}must-revalidate", if prev { "," } else { "" },)?;
//             prev = true;
//         }
//         if self.proxy_revalidate {
//             write!(&mut s, "{}proxy-revalidate", if prev { "," } else { "" },)?;
//             prev = true;
//         }
//         if self.private {
//             write!(&mut s, "{}private", if prev { "," } else { "" },)?;
//             prev = true;
//         }
//         if self.public && !self.private {
//             write!(&mut s, "{}public", if prev { "," } else { "" },)?;
//             prev = true;
//         }
//         if self.must_understand {
//             write!(&mut s, "{}must-understand", if prev { "," } else { "" },)?;
//             prev = true;
//         }
//         if self.no_transform {
//             write!(&mut s, "{}no-transform", if prev { "," } else { "" },)?;
//             prev = true;
//         }
//         if self.immutable {
//             write!(&mut s, "{}immutable", if prev { "," } else { "" },)?;
//             prev = true;
//         }
//         if self.stale_if_error {
//             write!(&mut s, "{}stale-if-error", if prev { "," } else { "" },)?;
//         }
//
//         Ok(s)
//     }
//
//     pub(crate) fn no_store() -> Self {
//         CacheControl {
//             no_store: true,
//             ..Default::default()
//         }
//     }
//
//     fn update_ttl(&self, ttl: u64, now: u64) -> u64 {
//         let elapsed = self.elapsed_inner(now);
//         if elapsed < 0 {
//             0
//         } else {
//             ttl.saturating_sub(elapsed as u64)
//         }
//     }
//
//     /// Merge cache control values without updating the TTL
//     pub(crate) fn merge_without_update(&self, other: &CacheControl) -> CacheControl {
//         self.merge_inner(other, None)
//     }
//
//     pub(crate) fn merge(&self, other: &CacheControl) -> CacheControl {
//         self.merge_inner(other, now_epoch_seconds().into())
//     }
//
//     fn merge_inner(&self, other: &CacheControl, now: Option<u64>) -> CacheControl {
//         // Early return to avoid conflicts https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#preventing_storing
//         if self.no_store || other.no_store {
//             return CacheControl {
//                 no_store: true,
//                 ..Default::default()
//             };
//         }
//         CacheControl {
//             created: now.unwrap_or_default(),
//             max_age: match now {
//                 Some(now) => match (self.ttl(), other.ttl()) {
//                     (None, None) => None,
//                     (None, Some(ttl)) => Some(other.update_ttl(ttl, now)),
//                     (Some(ttl), None) => Some(self.update_ttl(ttl, now)),
//                     (Some(ttl1), Some(ttl2)) => Some(std::cmp::min(
//                         self.update_ttl(ttl1, now),
//                         other.update_ttl(ttl2, now),
//                     )),
//                 },
//                 None => match (self.ttl(), other.ttl()) {
//                     (None, None) => None,
//                     (None, Some(ttl)) => Some(ttl),
//                     (Some(ttl), None) => Some(ttl),
//                     (Some(ttl1), Some(ttl2)) => Some(std::cmp::min(ttl1, ttl2)),
//                 },
//             },
//             age: None,
//             s_max_age: None,
//             stale_while_revalidate: match now {
//                 Some(now) => match (self.stale_while_revalidate, other.stale_while_revalidate) {
//                     (None, None) => None,
//                     (None, Some(ttl)) => Some(other.update_ttl(ttl, now)),
//                     (Some(ttl), None) => Some(self.update_ttl(ttl, now)),
//                     (Some(ttl1), Some(ttl2)) => Some(std::cmp::min(
//                         self.update_ttl(ttl1, now),
//                         other.update_ttl(ttl2, now),
//                     )),
//                 },
//                 None => match (self.stale_while_revalidate, other.stale_while_revalidate) {
//                     (None, None) => None,
//                     (None, Some(ttl)) => Some(ttl),
//                     (Some(ttl), None) => Some(ttl),
//                     (Some(ttl1), Some(ttl2)) => Some(std::cmp::min(ttl1, ttl2)),
//                 },
//             },
//             no_cache: self.no_cache || other.no_cache,
//             must_revalidate: self.must_revalidate || other.must_revalidate,
//             proxy_revalidate: self.proxy_revalidate || other.proxy_revalidate,
//             no_store: self.no_store || other.no_store,
//             private: self.private || other.private,
//             // private takes precedence over public
//             public: if self.private || other.private {
//                 false
//             } else {
//                 self.public || other.public
//             },
//             must_understand: self.must_understand || other.must_understand,
//             no_transform: self.no_transform || other.no_transform,
//             immutable: self.immutable || other.immutable,
//             stale_if_error: self.stale_if_error || other.stale_if_error,
//         }
//     }
//
//     pub(crate) fn elapsed(&self) -> i128 {
//         self.elapsed_inner(now_epoch_seconds())
//     }
//
//     pub(crate) fn elapsed_inner(&self, now: u64) -> i128 {
//         now as i128 - self.created as i128
//     }
//
//     pub(crate) fn ttl(&self) -> Option<u64> {
//         match (
//             self.s_max_age.as_ref().or(self.max_age.as_ref()),
//             self.age.as_ref(),
//         ) {
//             (None, _) => None,
//             (Some(max_age), None) => Some(*max_age),
//             (Some(max_age), Some(age)) => Some(max_age.saturating_sub(*age)),
//         }
//     }
//
//     pub(crate) fn should_store(&self) -> bool {
//         // FIXME: should we add support for must-understand?
//         // public will be the default case
//         !self.no_store && self.ttl().map(|ttl| ttl > 0).unwrap_or(true)
//     }
//
//     pub(crate) fn private(&self) -> bool {
//         self.private
//     }
//
//     pub(crate) fn public(&self) -> bool {
//         self.public
//     }
//
//     pub(crate) fn can_use(&self) -> bool {
//         let elapsed = self.elapsed();
//         let expired = if elapsed < 0 {
//             true
//         } else {
//             self.ttl().map(|ttl| ttl < elapsed as u64).unwrap_or(false)
//         };
//
//         // FIXME: we don't honor stale-while-revalidate yet
//         // !expired || self.stale_while_revalidate
//         !expired && !self.no_cache
//     }
//
//     pub(crate) fn is_no_cache(&self) -> bool {
//         self.no_cache
//     }
//
//     pub(crate) fn is_no_store(&self) -> bool {
//         self.no_store
//     }
//
//     pub(crate) fn s_max_age_or_max_age(&self) -> Option<u64> {
//         self.s_max_age.or(self.max_age)
//     }
//
//     pub(crate) fn age(&self) -> Option<u64> {
//         self.age
//     }
//
//     #[cfg(test)]
//     pub(crate) fn remaining_time(&self, now: u64) -> Option<u64> {
//         self.ttl().map(|ttl| {
//             let elapsed = self.elapsed_inner(now);
//             if elapsed < 0 {
//                 0
//             } else {
//                 ttl.saturating_sub(elapsed as u64)
//             }
//         })
//     }
//
//     // Export this for tests to avoid exporting field in pub(super) and create mistakes
//     #[cfg(test)]
//     #[allow(dead_code)] //False positive in clippy
//     pub(crate) fn set_created(&mut self, created: u64) {
//         self.created = created;
//     }
// }
