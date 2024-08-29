//! Snapshot responses from REST APIs for Connectors.
//!
//! Snapshots store the response from a REST API (headers and body) to the local file system.
//! The response can then be reused for subsequent requests to the same URL. The files are JSON,
//! and can be manually edited, for example to redact or remove sensitive fields or headers, or
//! to change values for testing.
//!
//! Snapshots are useful for:
//! * Development and debugging without repeatedly hitting the backend REST service
//! * Development and debugging where short-lived auth tokens are required by the REST API
//! * Creating tests that capture output from a real REST API to replay as a mock
//!
//! A few things to be aware of:
//! * Saving REST responses to the file system could potentially result in saving sensitive
//!   information in plain text on the file system. This feature is for development purposes
//!   and should not be used with sensitive production data (though snapshot content can be
//!   manually redacted as mentioned above). There is a warning emitted on router startup
//!   when this feature is turned on.
//! * Snapshots for requests that would modify data (such as POST requests) will obviously
//!   not have any side effects when replayed. Snapshots are not useful if you rely on those
//!   side effects.
//! * The unique key for storing and loading snapshots is the full URL. This means snapshots
//!   will not work well with APIs running on ephemeral ports, or any other cases where the
//!   URL might be different for the same request.
//! * Since headers are not included in the snapshot key, requests that would return
//!   different data depending on the value of headers will use the same snapshot.
//! * Since no network request is made, running with snapshots will be unrealistically fast,
//!   and should not be used with performance or load testing.

use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::PathBuf;
use std::str::FromStr;

use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use indexmap::IndexMap;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tracing::warn;

use crate::services::router::body::RouterBody;

#[derive(Serialize, Deserialize)]
pub(crate) struct Snapshot<'a> {
    url: Cow<'a, str>,
    headers: IndexMap<String, String>,
    body: Cow<'a, Value>,
}

impl<'a> Snapshot<'a> {
    pub(crate) fn new(url: &'a str, body: &'a Value, headers: &'a HeaderMap<HeaderValue>) -> Self {
        let headers = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect();
        Snapshot {
            url: Cow::Borrowed(url),
            headers,
            body: Cow::Borrowed(body),
        }
    }

    pub(crate) fn save(&self, base: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
        let path = snapshot_path(base.clone(), &self.url);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&self)?)?;
        Ok(())
    }

    pub(crate) fn load(base: PathBuf, url: &'a str) -> Option<Self> {
        let path = snapshot_path(base, url);
        let string = std::fs::read_to_string(path).ok()?;
        let snapshot: Snapshot = serde_json::from_str(&string).ok()?;
        if snapshot.url != url {
            // TODO: handle URL collisions
            warn!("Snapshot URL collision: {}, {}", snapshot.url, url);
            return None;
        }
        Some(snapshot)
    }
}

impl<'a> TryFrom<Snapshot<'a>> for crate::plugins::connectors::http::Result<RouterBody> {
    type Error = ();

    fn try_from(snapshot: Snapshot) -> Result<Self, Self::Error> {
        let mut response = http::Response::builder().status(http::StatusCode::OK);
        if let Some(headers) = response.headers_mut() {
            for (name, value) in snapshot.headers.into_iter() {
                if let Ok(name) = HeaderName::from_str(&name.clone()) {
                    if let Ok(value) = HeaderValue::from_str(&value.clone()) {
                        headers.insert(name, value);
                    }
                }
            }
        }
        if let Ok(string) = serde_json::to_string(&*snapshot.body) {
            if let Ok(response) = response.body(RouterBody::from(string)) {
                return Ok(crate::plugins::connectors::http::Result::HttpResponse(
                    response,
                ));
            }
        }
        Err(())
    }
}

fn snapshot_path(base: PathBuf, url: &str) -> PathBuf {
    let url_hash = {
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        hasher.finish()
    };
    base.join(url_hash.to_string()).with_extension("json")
}
