// With regards to ELv2 licensing, this entire file is license key functionality

use std::str::FromStr;
use std::time::Duration;

use displaydoc::Display;
use futures::Stream;
use thiserror::Error;
use url::Url;

#[derive(Error, Display, Debug)]
pub enum Error {
    /// invalid entitlement
    InvalidEntitlement,
}

/// Entitlement controls availability of certain features of the Router. It must be constructed from a base64 encoded JWT
/// This API experimental and is subject to change outside of semver.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct Entitlement {
    jwt: Option<String>,
}

impl FromStr for Entitlement {
    type Err = Error;

    fn from_str(_s: &str) -> Result<Self, Self::Err> {
        todo!()
    }
}

pub(crate) fn stream_entitlement(
    _api_key: String,
    _graph_ref: String,
    _urls: Option<Vec<Url>>,
    mut _interval: Duration,
    _timeout: Duration,
) -> impl Stream<Item = Result<Entitlement, String>> {
    futures::stream::empty()
}
