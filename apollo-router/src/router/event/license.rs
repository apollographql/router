use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;

use crate::registry::OciConfig;
use crate::registry::OciError;
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

/// Records that a license fetch failed, tagged by source and a short stable failure reason.
/// A source genuinely reporting "no license configured" (e.g. `OciError::is_not_found()`)
/// is not a fetch failure and must not be recorded here.
fn record_license_fetch_failure(source: &'static str, reason: &'static str) {
    u64_counter_with_unit!(
        "apollo.router.license.fetch.failure.total",
        "Number of license fetch failures, by source and reason",
        "{failure}",
        1u64,
        source = source,
        reason = reason
    );
}

/// Classifies a license parse failure (JWT decode error) as either the router not
/// understanding the license's claim shape, or a generically invalid license.
fn parse_error_reason(is_version_incompatible: bool) -> &'static str {
    if is_version_incompatible {
        "version_incompatible"
    } else {
        "invalid_license"
    }
}

fn uplink_error_reason(e: &crate::uplink::Error) -> &'static str {
    match e {
        crate::uplink::Error::Http(_) => "http_error",
        crate::uplink::Error::FetchFailedSingle
        | crate::uplink::Error::FetchFailedMultiple { .. } => "fetch_failed",
        crate::uplink::Error::UplinkError { code, .. }
        | crate::uplink::Error::UplinkErrorNoRetry { code, .. } => {
            if code == "LICENSE_VERSION_INCOMPATIBLE" {
                "version_incompatible"
            } else {
                "uplink_error"
            }
        }
    }
}

fn oci_error_reason(e: &OciError) -> &'static str {
    match e {
        OciError::LayerNotFound(_) => "not_found",
        OciError::Distribution(_) => "http_error",
        OciError::Parse(_) | OciError::LayerParse(_) => "parse_error",
        OciError::LicenseParse(_) => "invalid_license",
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
    #[display("OCI")]
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
                    record_license_fetch_failure("file", "not_found");
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
                                                record_license_fetch_failure("file", "io_error");
                                            }
                                            result.ok()
                                        }
                                    })
                                    .filter_map(|e| async move {
                                        let result = e.parse::<License>();
                                        if let Err(e) = &result {
                                            log_license_parse_error(e.is_version_incompatible(), e);
                                            record_license_fetch_failure(
                                                "file",
                                                parse_error_reason(e.is_version_incompatible()),
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
                            log_license_parse_error(err.is_version_incompatible(), &err);
                            record_license_fetch_failure(
                                "file",
                                parse_error_reason(err.is_version_incompatible()),
                            );
                            stream::empty().boxed()
                        }
                        Err(err) => {
                            tracing::error!(
                                code = APOLLO_ROUTER_LICENSE_INVALID,
                                "Failed to read license: {}",
                                err
                            );
                            record_license_fetch_failure("file", "io_error");
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
                                tracing::debug!(error = ?e, "uplink license fetch failed");
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
                                record_license_fetch_failure("registry", uplink_error_reason(&e));
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
                                    // `fetch_license_from_reference`, so any `Err` reaching
                                    // here is a transient failure (auth, 5xx, network) that
                                    // should be retried on the next poll, not treated as an
                                    // invalid license.
                                    let reason = oci_error_reason(&e);
                                    tracing::warn!(
                                        source = "oci",
                                        reason,
                                        "transient error fetching license from oci registry, will retry: {}",
                                        e
                                    );
                                    record_license_fetch_failure("oci", reason);
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
                        record_license_fetch_failure("oci", "stream_init_error");
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
                        record_license_fetch_failure(
                            "env",
                            parse_error_reason(err.is_version_incompatible()),
                        );
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
    use crate::metrics::FutureMetricsExt;
    use crate::test_harness::tracing_test;

    #[test]
    fn parse_error_reason_distinguishes_version_incompatible() {
        assert_eq!(parse_error_reason(true), "version_incompatible");
        assert_eq!(parse_error_reason(false), "invalid_license");
    }

    #[test]
    fn uplink_error_reason_classifies_each_variant() {
        assert_eq!(
            uplink_error_reason(&crate::uplink::Error::FetchFailedSingle),
            "fetch_failed"
        );
        assert_eq!(
            uplink_error_reason(&crate::uplink::Error::FetchFailedMultiple { url_count: 2 }),
            "fetch_failed"
        );
        assert_eq!(
            uplink_error_reason(&crate::uplink::Error::UplinkError {
                code: "LICENSE_VERSION_INCOMPATIBLE".to_string(),
                message: "".to_string(),
            }),
            "version_incompatible"
        );
        assert_eq!(
            uplink_error_reason(&crate::uplink::Error::UplinkErrorNoRetry {
                code: "ACCESS_DENIED".to_string(),
                message: "".to_string(),
            }),
            "uplink_error"
        );
    }

    #[test]
    fn oci_error_reason_treats_distribution_errors_as_transient() {
        let err = OciError::LicenseParse(
            "invalid"
                .parse::<License>()
                .expect_err("must fail to parse"),
        );
        assert_eq!(oci_error_reason(&err), "invalid_license");
    }

    #[tokio::test]
    async fn record_license_fetch_failure_increments_the_failure_counter() {
        async {
            record_license_fetch_failure("file", "not_found");

            assert_counter!(
                "apollo.router.license.fetch.failure.total",
                1,
                "source" = "file",
                "reason" = "not_found"
            );
        }
        .with_metrics()
        .await;
    }

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
