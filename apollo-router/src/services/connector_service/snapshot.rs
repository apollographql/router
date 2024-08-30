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
//! * The unique key for storing and loading snapshots includes the full URL. This means snapshots
//!   will not work well with APIs running on ephemeral ports, or any other cases where the URL
//!   might be different for the same request.
//! * Headers are not included in the snapshot key, so requests that would return
//!   different data depending on the value of headers will use the same snapshot.
//! * Since no network request is made, running with snapshots will be unrealistically fast,
//!   and should not be used with performance or load testing.

use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use indexmap::IndexMap;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;
use tracing::warn;

use crate::services::router::body::RouterBody;

#[derive(Serialize, Deserialize)]
pub(crate) struct Snapshot<'a> {
    key: Cow<'a, str>,
    headers: IndexMap<String, Vec<String>>,
    body: Cow<'a, Value>,
}

impl<'a> Snapshot<'a> {
    /// Create a new snapshot from an HTTP response.
    pub(crate) fn new(key: &'a str, body: &'a Value, headers: &'a HeaderMap<HeaderValue>) -> Self {
        let headers = headers.iter().fold(
            IndexMap::new(),
            |mut map: IndexMap<String, Vec<String>>, (name, value)| {
                let name = name.to_string();
                let value = value.to_str().unwrap_or_default().to_string();
                map.entry(name).or_default().push(value);
                map
            },
        );
        Snapshot {
            key: Cow::Borrowed(key),
            headers,
            body: Cow::Borrowed(body),
        }
    }

    /// Save the snapshot.
    pub(crate) fn save<P: AsRef<Path>>(&self, base: P) -> Result<(), Box<dyn std::error::Error>> {
        let path = snapshot_path(base, &self.key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&self)?)?;
        Ok(())
    }

    /// Load a snapshot from the file system, if it exists.
    pub(crate) fn load<P: AsRef<Path>>(base: P, key: &'a str) -> Option<Self> {
        let path = snapshot_path(base, key);
        let string = std::fs::read_to_string(path).ok()?;
        let snapshot: Snapshot = serde_json::from_str(&string).ok()?;
        if snapshot.key != key {
            // TODO: handle collisions
            warn!("Snapshot collision: {}, {}", snapshot.key, key);
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
            for (name, values) in snapshot.headers.into_iter() {
                if let Ok(name) = HeaderName::from_str(&name.clone()) {
                    for value in values {
                        if let Ok(value) = HeaderValue::from_str(&value.clone()) {
                            headers.insert(name.clone(), value);
                        }
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

pub(crate) async fn create_snapshot_key<T>(
    request: &http::Request<T>,
    body_hash: Option<String>,
) -> String {
    let url = request.uri().clone().to_string();
    let http_method = String::from(request.method().as_str());
    if let Some(body_hash) = body_hash {
        [http_method, body_hash, url].join("-")
    } else {
        [http_method, url].join("-")
    }
}

pub(crate) fn snapshot_path<P: AsRef<Path>>(base: P, key: &str) -> PathBuf {
    let mut key_hash = {
        let mut hasher = Sha256::new();
        hasher.update(key);
        hex::encode(hasher.finalize().as_slice())
    };
    key_hash.truncate(16);
    base.as_ref().join(key_hash).with_extension("json")
}
