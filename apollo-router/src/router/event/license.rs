use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;
use thiserror::Error;

use crate::router::Event;
use crate::router::Event::NoMoreLicense;
use crate::uplink::license_enforcement::Audience;
use crate::uplink::license_enforcement::License;
use crate::uplink::license_stream::LicenseQuery;
use crate::uplink::license_stream::LicenseStreamExt;
use crate::uplink::stream_from_uplink;
use crate::uplink::UplinkConfig;

const APOLLO_ROUTER_LICENSE_INVALID: &str = "APOLLO_ROUTER_LICENSE_INVALID";

type LicenseStream = Pin<Box<dyn Stream<Item = License> + Send>>;

#[derive(Debug, Display, From, Error)]
enum Error {
    /// The license is invalid.
    InvalidLicense,
}

/// License controls availability of certain features of the Router.
/// This API experimental and is subject to change outside of semver.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum LicenseSource {
    /// A static license. EXPERIMENTAL and not subject to semver.
    #[display(fmt = "Static")]
    Static { license: License },

    /// A license supplied via APOLLO_ROUTER_LICENSE. EXPERIMENTAL and not subject to semver.
    #[display(fmt = "Env")]
    Env,

    /// A stream of license. EXPERIMENTAL and not subject to semver.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] LicenseStream),

    /// A raw file that may be watched for changes. EXPERIMENTAL and not subject to semver.
    #[display(fmt = "File")]
    File {
        /// The path of the license file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,
    },

    /// Apollo uplink.
    #[display(fmt = "Registry")]
    Registry(UplinkConfig),
}

impl Default for LicenseSource {
    fn default() -> Self {
        LicenseSource::Static {
            license: Default::default(),
        }
    }
}

const VALID_AUDIENCES_USER_SUPLIED_LICENSES: [Audience; 2] = [Audience::Offline, Audience::Cloud];

impl LicenseSource {
    /// Convert this license into a stream regardless of if is static or not. Allows for unified handling later.
    pub(crate) fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            LicenseSource::Static { license } => stream::once(future::ready(license))
                .validate_audience(VALID_AUDIENCES_USER_SUPLIED_LICENSES)
                .boxed(),
            LicenseSource::Stream(stream) => stream
                .validate_audience(VALID_AUDIENCES_USER_SUPLIED_LICENSES)
                .boxed(),
            LicenseSource::File { path, watch } => {
                // Sanity check, does the schema file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "License file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    // The license file exists try and load it
                    match std::fs::read_to_string(&path).map(|e| e.parse()) {
                        Ok(Ok(license)) => {
                            if watch {
                                crate::files::watch(&path)
                                    .filter_map(move |_| {
                                        let path = path.clone();
                                        async move {
                                            let result = tokio::fs::read_to_string(&path).await;
                                            if let Err(e) = &result {
                                                tracing::error!(
                                                    "failed to read license file, {}",
                                                    e
                                                );
                                            }
                                            result.ok()
                                        }
                                    })
                                    .filter_map(|e| async move {
                                        let result = e.parse();
                                        if let Err(e) = &result {
                                            tracing::error!(
                                                code = APOLLO_ROUTER_LICENSE_INVALID,
                                                "failed to parse license file, {}",
                                                e
                                            );
                                        }
                                        result.ok()
                                    })
                                    .validate_audience(VALID_AUDIENCES_USER_SUPLIED_LICENSES)
                                    .boxed()
                            } else {
                                stream::once(future::ready(license))
                                    .validate_audience(VALID_AUDIENCES_USER_SUPLIED_LICENSES)
                                    .boxed()
                            }
                        }
                        Ok(Err(err)) => {
                            tracing::error!(
                                code = APOLLO_ROUTER_LICENSE_INVALID,
                                "Failed to parse license: {}",
                                err
                            );
                            stream::empty().boxed()
                        }
                        Err(err) => {
                            tracing::error!(
                                code = APOLLO_ROUTER_LICENSE_INVALID,
                                "Failed to read license: {}",
                                err
                            );
                            stream::empty().boxed()
                        }
                    }
                }
            }

            LicenseSource::Registry(uplink_config) => {
                stream_from_uplink::<LicenseQuery, License>(uplink_config)
                    .filter_map(|res| {
                        future::ready(match res {
                            Ok(license) => Some(license),
                            Err(e) => {
                                tracing::error!(code = APOLLO_ROUTER_LICENSE_INVALID, "{}", e);
                                None
                            }
                        })
                    })
                    .boxed()
            }
            LicenseSource::Env => {
                // EXPERIMENTAL and not subject to semver.
                match std::env::var("APOLLO_ROUTER_LICENSE").map(|e| License::from_str(&e)) {
                    Ok(Ok(license)) => stream::once(future::ready(license)).boxed(),
                    Ok(Err(err)) => {
                        tracing::error!("Failed to parse license: {}", err);
                        stream::empty().boxed()
                    }
                    Err(_) => stream::once(future::ready(License::default()))
                        .validate_audience(VALID_AUDIENCES_USER_SUPLIED_LICENSES)
                        .boxed(),
                }
            }
        }
        .expand_licenses()
        .chain(stream::iter(vec![NoMoreLicense]))
    }
}
