use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;

use crate::router::Event;
use crate::router::Event::NoMoreLicense;
use crate::uplink::UplinkConfig;
use crate::uplink::license_enforcement::Audience;
use crate::uplink::license_enforcement::License;
use crate::uplink::license_stream::LicenseQuery;
use crate::uplink::license_stream::LicenseStreamExt;
use crate::uplink::stream_from_uplink;

const APOLLO_ROUTER_LICENSE_INVALID: &str = "APOLLO_ROUTER_LICENSE_INVALID";

type LicenseStream = Pin<Box<dyn Stream<Item = License> + Send>>;

/// License controls availability of certain features of the Router.
/// This API experimental and is subject to change outside of semver.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum LicenseSource {
    /// A static license. EXPERIMENTAL and not subject to semver.
    #[display("Static")]
    Static { license: License },

    /// A license supplied via APOLLO_ROUTER_LICENSE. EXPERIMENTAL and not subject to semver.
    #[display("Env")]
    Env,

    /// A stream of license. EXPERIMENTAL and not subject to semver.
    #[display("Stream")]
    Stream(#[derivative(Debug = "ignore")] LicenseStream),

    /// A raw file that may be watched for changes. EXPERIMENTAL and not subject to semver.
    #[display("File")]
    File {
        /// The path of the license file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,
    },

    /// Apollo uplink.
    /// Apollo managed federation license.
    /// Validation happens in `into_stream()` when the license is actually fetched.
    #[display("Registry")]
    Registry {
        /// The Apollo key (APOLLO_KEY) - validated in into_stream
        apollo_key: Option<String>,
        /// The Apollo graph reference (APOLLO_GRAPH_REF) - validated in into_stream
        apollo_graph_ref: Option<String>,
        /// The endpoints polled
        endpoints: Option<crate::uplink::Endpoints>,
        /// The duration between polling
        poll_interval: std::time::Duration,
        /// The HTTP client timeout for each poll
        timeout: std::time::Duration,
    },
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

            LicenseSource::Registry {
                apollo_key,
                apollo_graph_ref,
                endpoints,
                poll_interval,
                timeout,
            } => {
                // Validate required fields and create UplinkConfig
                let uplink_config = match (apollo_key, apollo_graph_ref) {
                    (Some(key), Some(graph_ref)) => Some(UplinkConfig {
                        apollo_key: key,
                        apollo_graph_ref: graph_ref,
                        endpoints,
                        poll_interval,
                        timeout,
                    }),
                    (None, _) => {
                        tracing::error!("APOLLO_KEY is required for uplink license source");
                        None
                    }
                    (_, None) => {
                        tracing::error!("APOLLO_GRAPH_REF is required for uplink license source");
                        None
                    }
                };

                if let Some(config) = uplink_config {
                    stream_from_uplink::<LicenseQuery, License>(config)
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
                } else {
                    stream::empty().boxed()
                }
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
