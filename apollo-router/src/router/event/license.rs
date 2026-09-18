use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;

use crate::registry::OciConfig;
use crate::registry::create_oci_license_stream;
use crate::router::Event;
use crate::router::Event::NoMoreLicense;
use crate::uplink::UplinkConfig;
use crate::uplink::license_enforcement::APOLLO_ROUTER_LICENSE_VERSION_INCOMPATIBLE;
use crate::uplink::license_enforcement::Audience;
use crate::uplink::license_enforcement::LICENSE_INVALID_SHORT_MESSAGE;
use crate::uplink::license_enforcement::LICENSE_VERSION_INCOMPATIBLE_SHORT_MESSAGE;
use crate::uplink::license_enforcement::License;
use crate::uplink::license_stream::LicenseQuery;
use crate::uplink::license_stream::LicenseStreamExt;
use crate::uplink::stream_from_uplink;

const APOLLO_ROUTER_LICENSE_INVALID: &str = "APOLLO_ROUTER_LICENSE_INVALID";

/// Logs the license parse failure under the version-incompatible code/message when
/// `is_version_incompatible` reports the router doesn't understand the claim shape it
/// was given, otherwise under the generic invalid-license code.
fn log_license_parse_error(is_version_incompatible: bool, err: impl std::fmt::Display) {
    if is_version_incompatible {
        tracing::error!(
            code = APOLLO_ROUTER_LICENSE_VERSION_INCOMPATIBLE,
            "{}: {}",
            LICENSE_VERSION_INCOMPATIBLE_SHORT_MESSAGE,
            err
        );
    } else {
        tracing::error!(
            code = APOLLO_ROUTER_LICENSE_INVALID,
            "{}: {}",
            LICENSE_INVALID_SHORT_MESSAGE,
            err
        );
    }
}

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
    #[display("Registry")]
    Registry(UplinkConfig),

    /// Apollo graph artifact OCI registry.
    #[display("Registry")]
    OCI(OciConfig),
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
                                        let result = e.parse::<License>();
                                        if let Err(e) = &result {
                                            log_license_parse_error(e.is_version_incompatible(), e);
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
                            log_license_parse_error(err.is_version_incompatible(), &err);
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
                                let code = match &e {
                                    crate::uplink::Error::UplinkError { code, .. }
                                    | crate::uplink::Error::UplinkErrorNoRetry { code, .. } => {
                                        Some(code.as_str())
                                    }
                                    _ => None,
                                };
                                log_license_parse_error(
                                    code == Some("LICENSE_VERSION_INCOMPATIBLE"),
                                    &e,
                                );
                                None
                            }
                        })
                    })
                    .boxed()
            }
            LicenseSource::OCI(oci_config) => {
                tracing::debug!("using oci as license source");
                match create_oci_license_stream(oci_config) {
                    Ok(stream) => stream
                        .filter_map(|res| {
                            future::ready(match res {
                                Ok(license) => Some(license),
                                Err(e) => {
                                    // A genuine "no entitlement" (`OciError::is_not_found()`)
                                    // is already converted to `Ok(License::default())` inside
                                    // `fetch_license_oci` / `fetch_license_from_reference`
                                    // (missing annotation, missing entitlement manifest, or
                                    // missing license layer), so any `Err` reaching here is a
                                    // transient failure (auth, 5xx, network) that should be
                                    // retried on the next poll, not treated as an invalid
                                    // license.
                                    tracing::warn!(
                                        "transient error fetching license from oci registry, will retry: {}",
                                        e
                                    );
                                    None
                                }
                            })
                        })
                        .boxed(),
                    Err(e) => {
                        tracing::error!(
                            code = APOLLO_ROUTER_LICENSE_INVALID,
                            "failed to create OCI license stream: {}",
                            e
                        );
                        stream::empty().boxed()
                    }
                }
            }
            LicenseSource::Env => {
                // EXPERIMENTAL and not subject to semver.
                match std::env::var("APOLLO_ROUTER_LICENSE").map(|e| License::from_str(&e)) {
                    Ok(Ok(license)) => stream::once(future::ready(license)).boxed(),
                    Ok(Err(err)) => {
                        log_license_parse_error(err.is_version_incompatible(), &err);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_harness::tracing_test;

    #[test]
    fn log_license_parse_error_uses_version_incompatible_code_when_classified_as_such() {
        let _guard = tracing_test::dispatcher_guard();

        log_license_parse_error(true, "missing required claim: warnAt");

        assert!(tracing_test::logs_contain(
            APOLLO_ROUTER_LICENSE_VERSION_INCOMPATIBLE
        ));
        assert!(tracing_test::logs_contain(
            LICENSE_VERSION_INCOMPATIBLE_SHORT_MESSAGE
        ));
    }

    #[test]
    fn log_license_parse_error_falls_back_to_generic_invalid_code() {
        let _guard = tracing_test::dispatcher_guard();

        log_license_parse_error(false, "invalid signature");

        assert!(tracing_test::logs_contain(APOLLO_ROUTER_LICENSE_INVALID));
        assert!(tracing_test::logs_contain(LICENSE_INVALID_SHORT_MESSAGE));
        assert!(!tracing_test::logs_contain(
            APOLLO_ROUTER_LICENSE_VERSION_INCOMPATIBLE
        ));
    }
}
