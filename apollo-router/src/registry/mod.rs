use std::pin::Pin;
use std::string::FromUtf8Error;
use std::time::Duration;
use std::time::Instant;

use docker_credential::CredentialRetrievalError;
use docker_credential::DockerCredential;
use futures::Stream;
use futures::StreamExt;
use futures::stream;
use oci_client::Client;
use oci_client::Reference;
use oci_client::client::ClientConfig;
use oci_client::client::ClientProtocol;
use oci_client::errors::OciDistributionError;
use oci_client::errors::OciErrorCode;
use oci_client::secrets::RegistryAuth;
use thiserror::Error;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;
use tracing::instrument::WithSubscriber;
use url::Url;

use crate::uplink::license_enforcement::Error as LicenseError;
use crate::uplink::license_enforcement::License;
use crate::uplink::schema::SchemaState;

/// Type of OCI reference
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OciReferenceType {
    /// Tag reference (e.g., `:latest`, `:v1.0.0`)
    Tag,
    /// SHA256 digest reference (e.g., `@sha256:...`)
    Digest,
}

/// Validate an OCI reference string and determine its type.
///
/// Uses the OCI distribution spec reference parser to validate the reference,
/// then determines if it's a tag or digest reference.
pub(crate) fn validate_oci_reference(
    reference: &str,
) -> Result<(String, OciReferenceType), anyhow::Error> {
    // Quick check if the reference contains a domain name since the parser will accept
    // no domain and default to docker.io which is not appropriate.
    if reference.starts_with([':', '@']) {
        return Err(anyhow::anyhow!(
            "invalid graph artifact reference '{}': must specify registry before reference",
            reference
        ));
    }

    // Parse the reference using OCI distribution spec parser
    reference
        .parse::<Reference>()
        .map_err(|e| {
            anyhow::anyhow!(
                "invalid graph artifact reference '{}': {}",
                reference,
                e
            )
        })
        .and_then(|parsed_reference| {
            // Determine reference type using pattern matching
            match (parsed_reference.digest(), parsed_reference.tag()) {
                (Some(digest), None) => {
                    tracing::debug!("validated OCI digest reference: {}", digest);
                    Ok((reference.to_string(), OciReferenceType::Digest))
                }
                (None, Some(tag)) => {
                    tracing::debug!("validated OCI tag reference: {}", tag);
                    Ok((reference.to_string(), OciReferenceType::Tag))
                }
                (Some(_), Some(_)) => {
                    // This shouldn't happen with proper OCI references, but handle it gracefully
                    Err(anyhow::anyhow!(
                        "invalid graph artifact reference '{}': reference cannot have both digest and tag",
                        reference
                    ))
                }
                (None, None) => {
                    Err(anyhow::anyhow!(
                        "invalid graph artifact reference '{}': must specify either a digest (@algorithm:digest) or tag (:tag)",
                        reference
                    ))
                }
            }
        })
}

/// Configuration for fetching an OCI Bundle
/// This struct does not change on router reloads - they are all sourced from CLI options.
#[derive(Debug, Clone)]
pub struct OciConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: String,

    /// OCI Compliant URL pointing to the release bundle
    pub reference: String,

    /// Hot reload enabled
    pub hot_reload: bool,

    /// The duration between polling
    pub poll_interval: Duration,

    /// Whether to use SSL (HTTPS) when connecting to the OCI registry.
    /// Determined once at config creation from the registry hostname and
    /// the `APOLLO_GRAPH_ARTIFACT_UNSECURE_HOSTS` environment variable.
    pub use_ssl: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct OciContent {
    pub schema: String,
    pub launch_id: Option<String>,
}

#[derive(Debug, Error)]
pub(crate) enum OciError {
    #[error("expected oci layer with media type '{0}' not found in manifest")]
    LayerNotFound(String),
    #[error("oci distribution error: {0}")]
    Distribution(OciDistributionError),
    #[error("oci parsing error: {0}")]
    Parse(oci_client::ParseError),
    #[error("unable to parse layer: {0}")]
    LayerParse(FromUtf8Error),
    #[error("unable to parse license: {0}")]
    LicenseParse(LicenseError),
}

const APOLLO_REGISTRY_ENDING: &str = "apollographql.com";
const APOLLO_REGISTRY_USERNAME: &str = "apollo-registry";
const APOLLO_SCHEMA_MEDIA_TYPE: &str = "application/apollo.schema";
//  Keep in sync with value in mdg-private/monorepo/libs/entitlements/oci/model/src/main/kotlin/apollo/entitlements/oci/model/EntitlementArtifact.kt:15
const ENTITLEMENT_MEDIA_TYPE: &str = "application/vnd.apollographql.entitlement.v1+jwt";
const APOLLO_MANIFEST_LAUNCH_ID_ANNOTATION: &str = "com.apollograph.launch.id";

impl From<oci_client::ParseError> for OciError {
    fn from(value: oci_client::ParseError) -> Self {
        OciError::Parse(value)
    }
}

impl From<OciDistributionError> for OciError {
    fn from(value: OciDistributionError) -> Self {
        OciError::Distribution(value)
    }
}

impl From<FromUtf8Error> for OciError {
    fn from(value: FromUtf8Error) -> Self {
        OciError::LayerParse(value)
    }
}

impl From<LicenseError> for OciError {
    fn from(value: LicenseError) -> Self {
        OciError::LicenseParse(value)
    }
}

fn build_auth(reference: &Reference, apollo_key: &str) -> RegistryAuth {
    let server = reference
        .resolve_registry()
        .strip_suffix('/')
        .unwrap_or_else(|| reference.resolve_registry());

    // Check if the server registry ends with apollographql.com
    if server.ends_with(APOLLO_REGISTRY_ENDING) {
        tracing::debug!("using registry authentication");
        return RegistryAuth::Basic(APOLLO_REGISTRY_USERNAME.to_string(), apollo_key.to_string());
    }

    match docker_credential::get_credential(server) {
        Err(CredentialRetrievalError::ConfigNotFound)
        | Err(CredentialRetrievalError::NoCredentialConfigured) => RegistryAuth::Anonymous,
        Err(e) => {
            tracing::warn!("error handling docker configuration file: {e}");
            RegistryAuth::Anonymous
        }
        Ok(DockerCredential::UsernamePassword(username, password)) => {
            tracing::debug!("found username/password docker credentials");
            RegistryAuth::Basic(username, password)
        }
        Ok(DockerCredential::IdentityToken(token)) => {
            tracing::debug!("found identity token docker credentials");
            RegistryAuth::Bearer(token)
        }
    }
}

/// Fetch the manifest, extract the blob location, and fetch the blob.
async fn fetch_oci_from_reference(
    client: &mut Client,
    auth: &RegistryAuth,
    reference: &Reference,
    oci_config: Option<&OciConfig>,
) -> Result<OciContent, OciError> {
    tracing::debug!("pulling oci manifest");
    // The OCI Client has a pull() function, but that validates that all the layers are in the list of
    // supported layers. Apollo wants to add new layers as features evolve and routers in the field will
    // break if they get an unsupported layer type. Instead, this code narrowly fetches only the layers
    // understands.
    let (manifest, _) = fetch_oci_manifest(client, auth, reference, oci_config).await?;

    let schema_layer = manifest
        .layers
        .iter()
        .find(|layer| layer.media_type == APOLLO_SCHEMA_MEDIA_TYPE)
        .ok_or_else(|| OciError::LayerNotFound(APOLLO_SCHEMA_MEDIA_TYPE.to_string()))?
        .clone();

    tracing::debug!("pulling oci blob");
    let schema = fetch_oci_blob(client, reference, &schema_layer).await?;

    let annotations = manifest.annotations;

    let launch_id = match &annotations {
        Some(a) => a.get(APOLLO_MANIFEST_LAUNCH_ID_ANNOTATION),
        None => None,
    }
    .cloned();

    Ok(OciContent {
        schema: String::from_utf8(schema)?,
        launch_id,
    })
}

/// Fetch the full OCI manifest to determine the location of the schema blob
async fn fetch_oci_manifest(
    client: &mut Client,
    auth: &RegistryAuth,
    reference: &Reference,
    oci_config: Option<&OciConfig>,
) -> Result<(oci_client::manifest::OciImageManifest, String), OciError> {
    let before_request = Instant::now();
    let registry = reference.registry().to_string();

    let result = client.pull_image_manifest(reference, auth).await;
    let status = if result.is_ok() { "success" } else { "failure" };
    let duration = before_request.elapsed().as_secs_f64();

    u64_counter_with_unit!(
        "apollo.router.oci.manifest",
        "Number of requests to get graph artifact manifest",
        "{request}",
        1u64,
        registry = registry.clone(),
        kind = "get_manifest",
        status = status
    );
    f64_histogram_with_unit!(
        "apollo.router.oci.manifest.duration",
        "Duration of request to get graph artifact manifest",
        "s",
        duration,
        registry = registry,
        kind = "get_manifest",
        status = status
    );

    match result {
        Ok(result) => Ok(result),
        Err(err) => {
            // Log error with consistent message format when oci_config is provided
            if oci_config.is_some() {
                tracing::error!("error fetching manifest digest from oci registry: {}", err);
            }
            Err(err.into())
        }
    }
}

/// Fetch the schema from the OCI blob
async fn fetch_oci_blob(
    client: &mut Client,
    reference: &Reference,
    schema_layer: &oci_client::manifest::OciDescriptor,
) -> Result<Vec<u8>, OciError> {
    let before_request = Instant::now();
    let registry = reference.registry().to_string();

    let mut blob_data = Vec::new();
    let result = client
        .pull_blob(reference, schema_layer, &mut blob_data)
        .await;

    let status = if result.is_ok() { "success" } else { "failure" };
    let duration = before_request.elapsed().as_secs_f64();

    u64_counter_with_unit!(
        "apollo.router.oci.blob",
        "Number of requests to get graph artifact blob",
        "{request}",
        1u64,
        registry = registry.clone(),
        kind = "get_blob",
        status = status
    );
    f64_histogram_with_unit!(
        "apollo.router.oci.blob.duration",
        "Duration of request to get graph artifact blob",
        "s",
        duration,
        registry = registry,
        kind = "get_blob",
        status = status
    );

    result?;
    Ok(blob_data)
}

const UNSECURE_HOSTS_ENV_VAR: &str = "APOLLO_GRAPH_ARTIFACT_UNSECURE_HOSTS";
const DEFAULT_UNSECURE_HOSTS: &[&str] = &["localhost", "127.0.0.1", "dockerhost"];

/// Parse a comma-separated string of unsecure hosts. Empty entries are ignored.
fn parse_unsecure_hosts(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn unsecure_hosts() -> Vec<String> {
    match std::env::var(UNSECURE_HOSTS_ENV_VAR) {
        Ok(val) => parse_unsecure_hosts(&val),
        Err(_) => DEFAULT_UNSECURE_HOSTS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// Extract the hostname from a registry string like "host", "host:port",
/// "http://host:port", or an IPv6 address like "[::1]:port".
/// If a scheme is already present, it is parsed directly; otherwise a dummy
/// scheme is prepended so `url::Url` can parse it.
/// IPv6 addresses are returned without brackets (e.g. "::1" not "[::1]").
fn extract_host(registry: &str) -> Option<String> {
    let url = if registry.contains("://") {
        Url::parse(registry).ok()?
    } else {
        Url::parse(&format!("dummy://{registry}")).ok()?
    };
    url.host().map(|h| match h {
        url::Host::Ipv6(addr) => addr.to_string(),
        other => other.to_string(),
    })
}

/// Check whether `registry` matches any entry in `hosts`, comparing only the
/// hostname portion (stripping any port).
fn is_unsecure_host(registry: &str, hosts: &[String]) -> bool {
    extract_host(registry).is_some_and(|host| hosts.iter().any(|h| h == &host))
}

/// Determine whether SSL should be used for the given OCI reference.
///
/// Consults the `APOLLO_GRAPH_ARTIFACT_UNSECURE_HOSTS` environment variable,
/// which contains a comma-separated list of hostnames that should use HTTP
/// instead of HTTPS. When the variable is unset, the defaults are "localhost",
/// "127.0.0.1", and "dockerhost". Setting it to an empty string disables all
/// HTTP overrides.
pub(crate) fn should_use_ssl(reference: &str) -> bool {
    reference
        .parse::<Reference>()
        .map_or(true, |r| !is_unsecure_host(r.registry(), &unsecure_hosts()))
}

impl OciConfig {
    fn client_protocol(&self) -> ClientProtocol {
        if self.use_ssl {
            ClientProtocol::Https
        } else {
            ClientProtocol::Http
        }
    }
}

/// Fetch the manifest digest (without fetching the full manifest) to detect changes
pub(crate) async fn fetch_oci_manifest_digest(oci_config: &OciConfig) -> Result<String, OciError> {
    let reference: Reference = oci_config.reference.as_str().parse()?;
    let auth = build_auth(&reference, &oci_config.apollo_key);
    let protocol = oci_config.client_protocol();

    let client = Client::new(ClientConfig {
        protocol,
        ..Default::default()
    });
    let before_request = Instant::now();
    let registry = reference.registry().to_string();
    let result = client.fetch_manifest_digest(&reference, &auth).await;
    let status = if result.is_ok() { "success" } else { "failure" };
    let duration = before_request.elapsed().as_secs_f64();

    u64_counter_with_unit!(
        "apollo.router.oci.manifest",
        "Number of requests to get graph artifact manifest",
        "{request}",
        1u64,
        registry = registry.clone(),
        kind = "head_manifest",
        status = status
    );
    f64_histogram_with_unit!(
        "apollo.router.oci.manifest.duration",
        "Duration of request to get graph artifact manifest",
        "s",
        duration,
        registry = registry,
        kind = "head_manifest",
        status = status
    );

    match result {
        Ok(digest) => Ok(digest),
        Err(err) => {
            tracing::error!("error fetching manifest digest from oci registry: {}", err);
            Err(err.into())
        }
    }
}

/// Fetch an OCI bundle by parsing the graph artifact reference, building auth,
/// inferring the correct protocol, and calling the internal fetch function.
pub(crate) async fn fetch_oci(oci_config: &OciConfig) -> Result<OciContent, OciError> {
    let reference: Reference = oci_config.reference.as_str().parse()?;
    let auth = build_auth(&reference, &oci_config.apollo_key);
    let protocol = oci_config.client_protocol();

    tracing::debug!(
        "prepared to fetch schema from oci over {:?}, auth anonymous? {:?}",
        protocol,
        auth == RegistryAuth::Anonymous
    );

    match fetch_oci_from_reference(
        &mut Client::new(ClientConfig {
            protocol,
            ..Default::default()
        }),
        &auth,
        &reference,
        Some(oci_config),
    )
    .await
    {
        Ok(content) => Ok(content),
        Err(err) => {
            tracing::error!("error fetching schema from oci registry: {}", err);
            Err(err)
        }
    }
}

/// Type alias for OCI schema stream
type OciSchemaStream = Pin<Box<dyn Stream<Item = Result<SchemaState, OciError>> + Send>>;

/// Create a schema stream from OCI config based on reference type and hot-reload setting.
///
/// Returns a stream that yields schema updates based on the configuration:
/// - Tag + hot-reload: Streams updates as the tag changes
/// - Tag + no hot-reload: Returns an error (not yet allowed)
/// - Digest + hot-reload: Returns an error (digests never change)
/// - Digest + no hot-reload: Fetches schema once and returns it as a single-item stream
pub(crate) fn create_oci_schema_stream(
    oci_config: OciConfig,
) -> Result<OciSchemaStream, anyhow::Error> {
    // Validate the reference to determine its type
    let (_, ref_type) = validate_oci_reference(&oci_config.reference)?;

    match (ref_type, oci_config.hot_reload) {
        (OciReferenceType::Tag, true) => Ok(Box::pin(stream_from_oci(oci_config))),
        (OciReferenceType::Tag, false) => Err(anyhow::anyhow!(
            "Tag references without --hot-reload are not yet supported."
        )),
        (OciReferenceType::Digest, true) => Err(anyhow::anyhow!(
            "Digest references are immutable so --hot-reload flag is not allowed."
        )),
        (OciReferenceType::Digest, false) => {
            let oci_config_clone = oci_config.clone();
            let stream = stream::once(async move {
                fetch_oci(&oci_config_clone)
                    .await
                    .map(|oci_content| SchemaState {
                        sdl: oci_content.schema,
                        launch_id: None,
                    })
            });
            Ok(Box::pin(stream))
        }
    }
}

/// Regularly fetch from OCI registry at the configured polling interval
pub(crate) fn stream_from_oci(
    oci_config: OciConfig,
) -> impl Stream<Item = Result<SchemaState, OciError>> {
    let (sender, receiver) = channel(2);

    let task = async move {
        let mut last_digest: Option<String> = None;
        let mut polling_time = oci_config.poll_interval;
        loop {
            match fetch_oci_manifest_digest(&oci_config).await {
                Ok(current_digest) => {
                    if last_digest.as_deref() == Some(current_digest.as_str()) {
                        // Digest unchanged, skip fetching the full schema
                        tracing::debug!("oci manifest digest unchanged, skipping schema fetch");
                    } else {
                        // Digest changed, fetch the full schema
                        tracing::debug!("oci manifest digest changed, fetching schema");

                        match fetch_oci(&oci_config).await {
                            Ok(oci_result) => {
                                tracing::debug!("fetched schema from oci registry");
                                let schema_state = SchemaState {
                                    sdl: oci_result.schema,
                                    launch_id: oci_result.launch_id,
                                };
                                if let Err(e) = sender.send(Ok(schema_state)).await {
                                    tracing::debug!(
                                        "failed to push to stream. This is likely to be because the router is shutting down: {e}"
                                    );
                                    break;
                                } else {
                                    // Only update the digest if the schema fetch was successful
                                    last_digest = Some(current_digest);
                                }
                            }
                            Err(err) => {
                                if let Some(retry_after) = parse_rate_limit_error(&err) {
                                    polling_time = retry_after.max(Duration::from_secs(10)); // Minimum 10 second backoff
                                }

                                // Error logging is now handled in fetch_oci
                                if let Err(e) = sender.send(Err(err)).await {
                                    tracing::debug!(
                                        "failed to send error to oci stream. This is likely to be because the router is shutting down: {e}"
                                    );
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    // It should not be possible to get a rate limit error here since the client will automatically move to a get request if the digest is not found, but just in case
                    if let Some(retry_after) = parse_rate_limit_error(&err) {
                        polling_time = retry_after.max(Duration::from_secs(10)); // Minimum 10 second backoff
                    }

                    if let Err(e) = sender.send(Err(err)).await {
                        tracing::debug!(
                            "failed to send error to oci stream. This is likely to be because the router is shutting down: {e}"
                        );
                        break;
                    }
                }
            }

            tokio::time::sleep(polling_time).await;
            polling_time = oci_config.poll_interval;
        }
    };
    drop(tokio::task::spawn(task.with_current_subscriber()));

    ReceiverStream::new(receiver).boxed()
}

fn parse_rate_limit_error(error: &OciError) -> Option<Duration> {
    if let OciError::Distribution(OciDistributionError::RegistryError { envelope, .. }) = error
        && let Some(error) = envelope
            .errors
            .iter()
            .find(|error| error.code == OciErrorCode::Toomanyrequests)
    {
        return error
            .detail
            .get("retryAfter")
            .and_then(|value| value.as_u64())
            .map(Duration::from_secs);
    }
    None
}

type OciLicenseStream = Pin<Box<dyn Stream<Item = Result<License, OciError>> + Send>>;

pub(crate) fn create_oci_license_stream(
    oci_config: OciConfig,
) -> Result<OciLicenseStream, anyhow::Error> {
    // Validate the reference to determine its type
    validate_oci_reference(&oci_config.reference)?;
    // An infinite polling stream is intended here
    Ok(Box::pin(stream_license_from_oci(oci_config)))
}

fn stream_license_from_oci(oci_config: OciConfig) -> impl Stream<Item = Result<License, OciError>> {
    let (sender, receiver) = channel(2);

    // Build an async task to poll for the license
    let task = async move {
        let mut last_digest: Option<String> = None;
        let mut polling_time = oci_config.poll_interval;
        loop {
            match fetch_oci_manifest_digest(&oci_config).await {
                Ok(current_digest) => {
                    tracing::debug!("oci manifest digest fetch succeeded");
                    if last_digest.as_deref() == Some(current_digest.as_str()) {
                        tracing::debug!("oci manifest digest unchanged, skip fetching license");
                    } else {
                        tracing::debug!("oci manifest digest changed, fetch license");
                        match fetch_license_oci(&oci_config).await {
                            Ok(license) => {
                                tracing::debug!("fetched license from oci registry");
                                if let Err(e) = sender.send(Ok(license)).await {
                                    tracing::debug!(
                                        "failed to send license to stream. This is likely to be because the router is shutting down: {e}"
                                    );
                                    break;
                                } else {
                                    // Only update the digest if the license fetch was successful
                                    last_digest = Some(current_digest);
                                }
                            }
                            Err(err) => {
                                tracing::debug!("failed to fetch license");
                                if let Some(retry_after) = parse_rate_limit_error(&err) {
                                    polling_time = retry_after.max(Duration::from_secs(10));
                                }
                                if let Err(e) = sender.send(Err(err)).await {
                                    tracing::debug!(
                                        "failed to send error to oci stream. This is likely to be because the router is shutting down: {e}"
                                    );
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::debug!("failed to fetch oci manifest digest");
                    if let Some(retry_after) = parse_rate_limit_error(&err) {
                        polling_time = retry_after.max(Duration::from_secs(10)); // Minimum 10 second backoff
                    }
                    if let Err(e) = sender.send(Err(err)).await {
                        tracing::debug!(
                            "failed to send error to oci stream. This is likely to be because the router is shutting down: {e}"
                        );
                        break;
                    }
                }
            }
            // Repeat the fetch every polling_interval
            tokio::time::sleep(polling_time).await;
            polling_time = oci_config.poll_interval;
        }
    };

    // Here we drop the JoinHandle that tokio::task::spawn returns in order to explicitly
    // detach it from this function since the stream is intended to outlive the function call
    drop(tokio::task::spawn(task.with_current_subscriber()));
    ReceiverStream::new(receiver).boxed()
}

async fn fetch_license_oci(oci_config: &OciConfig) -> Result<License, OciError> {
    let reference: Reference = oci_config.reference.as_str().parse()?;
    let auth = build_auth(&reference, &oci_config.apollo_key);
    let protocol = oci_config.client_protocol();

    tracing::debug!(
        "prepared to fetch license from oci over {:?}, auth anonymous? {:?}",
        protocol,
        auth == RegistryAuth::Anonymous
    );

    match fetch_license_from_reference(
        &mut Client::new(ClientConfig {
            protocol,
            ..Default::default()
        }),
        &auth,
        &reference,
        Some(oci_config),
    )
    .await
    {
        Ok(license) => Ok(license),
        Err(err) => {
            tracing::error!("error fetching license from oci registry: {}", err);
            Err(err)
        }
    }
}

async fn fetch_license_from_reference(
    client: &mut Client,
    auth: &RegistryAuth,
    reference: &Reference,
    oci_config: Option<&OciConfig>,
) -> Result<License, OciError> {
    tracing::debug!("pulling oci manifest for license");
    let (manifest, _) = fetch_oci_manifest(client, auth, reference, oci_config).await?;

    let license_layer = manifest
        .layers
        .iter()
        .find(|layer| layer.media_type == ENTITLEMENT_MEDIA_TYPE)
        .ok_or_else(|| OciError::LayerNotFound(ENTITLEMENT_MEDIA_TYPE.to_string()))?
        .clone();

    tracing::debug!("pulling oci blob for license layer");
    let license_blob_bytes = fetch_oci_blob(client, reference, &license_layer).await?;

    // Convert the license blob bytes into a License object (assuming it's json)
    let jwt = String::from_utf8(license_blob_bytes)?;
    let license: License = jwt.parse::<License>()?;

    Ok(license)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::VecDeque;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use futures::StreamExt;
    use futures::future::join_all;
    use oci_client::client::ClientConfig;
    use oci_client::client::ClientProtocol;
    use oci_client::client::ImageLayer;
    use oci_client::manifest::IMAGE_MANIFEST_MEDIA_TYPE;
    use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
    use oci_client::manifest::OciDescriptor;
    use oci_client::manifest::OciImageManifest;
    use oci_client::manifest::OciManifest;
    use parking_lot::Mutex;
    use sha2::Digest;
    use sha2::Sha256;
    use tokio::time::timeout;
    use url::Url;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Request;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    // Same test JWT used by `license_enforcement::test_license_parse`. Signed
    // against the JWKS bundled at `src/uplink/license.jwks.json` via
    // `include_str!`, so this token verifies in any test in the crate.
    const TEST_LICENSE_JWT: &str = "eyJhbGciOiJFZERTQSJ9.eyJpc3MiOiJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLCJzdWIiOiJhcG9sbG8iLCJhdWQiOiJTRUxGX0hPU1RFRCIsIndhcm5BdCI6MTY3NjgwODAwMCwiaGFsdEF0IjoxNjc4MDE3NjAwfQ.tXexfjZ2SQeqSwkWQ7zD4XBoxS_Hc5x7tSNJ3ln-BCL_GH7i3U9hsIgdRQTczCAjA_jjk34w39DeSV0nTc5WBw"; // gitleaks:allow

    fn calculate_manifest_digest(manifest: &OciManifest) -> String {
        let manifest_bytes = serde_json::to_vec(manifest).unwrap();
        let hash = Sha256::digest(&manifest_bytes);
        format!("sha256:{:x}", hash)
    }

    fn mock_oci_config_with_reference(reference: String) -> OciConfig {
        OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: reference.clone(),
            hot_reload: false,
            poll_interval: Duration::from_millis(10),
            use_ssl: false,
        }
    }

    struct SchemaLayerManifest {
        oci_manifest: OciManifest,
        manifest_digest: String,
        blob_digest: String,
        schema_data: Vec<u8>,
    }

    fn create_manifest_from_schema_layer(
        schema_data: &str,
        annotations: Option<BTreeMap<String, String>>,
    ) -> SchemaLayerManifest {
        let schema_layer = ImageLayer {
            data: schema_data.to_string().into(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let blob_digest = schema_layer.sha256_digest();
        let oci_manifest = OciManifest::Image(OciImageManifest {
            schema_version: 2,
            media_type: Some(IMAGE_MANIFEST_MEDIA_TYPE.to_string()),
            config: Default::default(),
            layers: vec![OciDescriptor {
                media_type: schema_layer.media_type.clone(),
                digest: blob_digest.clone(),
                size: schema_layer.data.len().try_into().unwrap(),
                urls: None,
                annotations: None,
                artifact_type: None,
            }],
            subject: None,
            artifact_type: None,
            annotations,
        });
        let manifest_digest = calculate_manifest_digest(&oci_manifest);
        SchemaLayerManifest {
            oci_manifest,
            manifest_digest,
            blob_digest,
            schema_data: schema_layer.data.to_vec(),
        }
    }

    struct LicenseLayerManifest {
        oci_manifest: OciManifest,
        manifest_digest: String,
        blob_digest: String,
        license_data: Vec<u8>,
    }

    fn create_manifest_from_license_layer(
        license_data: &[u8],
        annotations: Option<BTreeMap<String, String>>,
    ) -> LicenseLayerManifest {
        let license_layer = ImageLayer {
            data: license_data.to_owned().into(),
            media_type: ENTITLEMENT_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let blob_digest = license_layer.sha256_digest();
        let oci_manifest = OciManifest::Image(OciImageManifest {
            schema_version: 2,
            media_type: Some(IMAGE_MANIFEST_MEDIA_TYPE.to_string()),
            config: Default::default(),
            layers: vec![OciDescriptor {
                media_type: license_layer.media_type.clone(),
                digest: blob_digest.clone(),
                size: license_layer.data.len().try_into().unwrap(),
                ..Default::default()
            }],
            subject: None,
            artifact_type: None,
            annotations,
        });
        let manifest_digest = calculate_manifest_digest(&oci_manifest);
        LicenseLayerManifest {
            oci_manifest,
            manifest_digest,
            blob_digest,
            license_data: license_layer.data.into(),
        }
    }

    struct SequentialManifestDigests {
        digests: Mutex<VecDeque<String>>,
    }

    impl Respond for SequentialManifestDigests {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let digest = self
                .digests
                .lock()
                .pop_front()
                .expect("should have enough digests");
            ResponseTemplate::new(200)
                .append_header("Docker-Content-Digest", digest)
                .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
        }
    }

    struct SequentialManifests {
        manifests: Mutex<VecDeque<(String, Vec<u8>)>>,
    }

    impl Respond for SequentialManifests {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let (digest, body) = self
                .manifests
                .lock()
                .pop_front()
                .expect("should have enough manifests");
            ResponseTemplate::new(200)
                .append_header("Docker-Content-Digest", digest)
                .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                .set_body_bytes(body)
        }
    }

    #[test]
    fn test_build_auth_apollo_registry() {
        // Create a reference for an Apollo registry
        let reference: Reference = "registry.apollographql.com/my-graph:latest"
            .parse()
            .unwrap();
        let apollo_key = "test-api-key".to_string();

        // Call build_auth
        let auth = build_auth(&reference, &apollo_key);

        // Check that it returns the correct RegistryAuth
        match auth {
            RegistryAuth::Basic(username, password) => {
                assert_eq!(username, APOLLO_REGISTRY_USERNAME);
                assert_eq!(password, apollo_key);
            }
            _ => panic!("expected basic authentication, got something else"),
        }
    }

    #[test]
    fn test_build_auth_non_apollo_registry() {
        // Create a reference for a non-Apollo registry
        let reference: Reference = "docker.io/library/alpine:latest".parse().unwrap();
        let apollo_key = "test-api-key".to_string();

        // Mock the docker_credential::get_credential function
        // Since we can't easily mock this in Rust without additional libraries,
        // we'll just verify that it doesn't return the Apollo registry auth
        let auth = build_auth(&reference, &apollo_key);

        // Check that it doesn't return the Apollo registry auth
        if let RegistryAuth::Basic(username, _) = auth {
            assert_ne!(username, "apollo_registry");
        }
    }

    fn generate_manifest_annotations(launch_id: Option<&str>) -> BTreeMap<String, String> {
        let mut manifest_annotations = BTreeMap::new();

        if let Some(lid) = launch_id {
            manifest_annotations.insert(
                APOLLO_MANIFEST_LAUNCH_ID_ANNOTATION.to_string(),
                lid.to_string(),
            );
        }

        manifest_annotations
    }

    fn schema_layer(data: &str) -> ImageLayer {
        ImageLayer::new(data.to_string(), APOLLO_SCHEMA_MEDIA_TYPE.to_string(), None)
    }

    fn license_layer(data: impl Into<bytes::Bytes>) -> ImageLayer {
        ImageLayer::new(data, ENTITLEMENT_MEDIA_TYPE.to_string(), None)
    }

    fn unrelated_layer() -> ImageLayer {
        ImageLayer::new("foo_bar".to_string(), "foo_bar".to_string(), None)
    }

    async fn setup_mocks(
        mock_server: &MockServer,
        layers: Vec<ImageLayer>,
        manifest_annotations: Option<BTreeMap<String, String>>,
    ) -> Reference {
        let graph_id = "test-graph-id";
        let reference = "latest";

        let layer_descriptors = join_all(layers.iter().map(async |layer| {
            let blob_digest = layer.sha256_digest();
            let blob_url = Url::parse(&format!(
                "{}/v2/{graph_id}/blobs/{blob_digest}",
                mock_server.uri()
            ))
            .expect("url must be valid");
            Mock::given(method("GET"))
                .and(path(blob_url.path()))
                .respond_with(
                    ResponseTemplate::new(200)
                        .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                        .set_body_bytes(layer.data.clone()),
                )
                .mount(mock_server)
                .await;
            OciDescriptor {
                media_type: layer.media_type.clone(),
                digest: blob_digest,
                size: layer.data.len().try_into().unwrap(),
                urls: None,
                annotations: None,
                artifact_type: None,
            }
        }))
        .await;

        let manifest_url = Url::parse(&format!(
            "{}/v2/{}/manifests/{}",
            mock_server.uri(),
            graph_id,
            reference
        ))
        .expect("url must be valid");
        let oci_manifest = OciManifest::Image(OciImageManifest {
            schema_version: 2,
            media_type: Some(IMAGE_MANIFEST_MEDIA_TYPE.to_string()),
            config: Default::default(),
            layers: layer_descriptors,
            subject: None,
            artifact_type: None,
            annotations: manifest_annotations,
        });
        let manifest_digest = calculate_manifest_digest(&oci_manifest);

        // Set up HEAD request for manifest digest (used by fetch_oci_manifest_digest)
        let _ = Mock::given(method("HEAD"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", manifest_digest.clone())
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE),
            )
            .mount(mock_server)
            .await;

        // Set up GET request for full manifest (used by pull_image_manifest)
        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                    .set_body_bytes(serde_json::to_vec(&oci_manifest).unwrap()),
            )
            .mount(mock_server)
            .await;

        format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid")
    }

    #[rstest::rstest]
    #[case::single_layer(vec![schema_layer("test schema")], Some("test schema"))]
    #[case::extra_layers(vec![schema_layer("test schema"), unrelated_layer()], Some("test schema"))]
    #[case::missing_layer(vec![unrelated_layer()], None)]
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_oci_from_reference_cases(
        #[case] layers: Vec<ImageLayer>,
        #[case] expected_schema: Option<&str>,
    ) {
        let mock_server = &MockServer::start().await;
        let mut client = Client::new(ClientConfig {
            protocol: ClientProtocol::Http,
            ..Default::default()
        });
        let image_reference = setup_mocks(mock_server, layers, None).await;
        let result = fetch_oci_from_reference(
            &mut client,
            &RegistryAuth::Anonymous,
            &image_reference,
            None,
        )
        .await;

        match expected_schema {
            Some(schema) => {
                assert_eq!(result.expect("failed to fetch oci bundle").schema, schema);
            }
            None => {
                let err = result.expect_err("expect can't fetch oci bundle");
                assert!(
                    matches!(err, OciError::LayerNotFound(_)),
                    "expected LayerNotFound, got {err:?}"
                );
            }
        }
    }

    fn assert_license_fetch_success(result: Result<License, OciError>) {
        let expected = License::from_str(TEST_LICENSE_JWT).expect("test JWT must parse");
        assert_eq!(
            result.expect("failed to fetch license").claims,
            expected.claims
        );
    }

    fn assert_license_fetch_missing_layer(result: Result<License, OciError>) {
        let err = result.expect_err("expected missing entitlements layer");
        assert!(
            matches!(err, OciError::LayerNotFound(_)),
            "expected LayerNotFound, got {err:?}"
        );
    }

    fn assert_license_fetch_bad_utf8(result: Result<License, OciError>) {
        let err = result.expect_err("expected utf8 conversion error");
        assert!(
            matches!(err, OciError::LayerParse(_)),
            "expected LayerParse, got {err:?}"
        );
    }

    fn assert_license_fetch_bad_jwt(result: Result<License, OciError>) {
        let err = result.expect_err("expected JWT parse error");
        assert!(
            matches!(err, OciError::LicenseParse(_)),
            "expected LicenseParse, got {err:?}"
        );
    }

    #[rstest::rstest]
    #[case::success(vec![license_layer(TEST_LICENSE_JWT)], assert_license_fetch_success)]
    #[case::ignores_extra_layers(
        vec![license_layer(TEST_LICENSE_JWT), unrelated_layer()],
        assert_license_fetch_success
    )]
    #[case::missing_layer(vec![unrelated_layer()], assert_license_fetch_missing_layer)]
    // 0xFF/0xFE are not valid UTF-8 start bytes.
    #[case::bad_utf8(vec![license_layer(vec![0xFF, 0xFE, 0xFD])], assert_license_fetch_bad_utf8)]
    #[case::bad_jwt(vec![license_layer("not a jwt")], assert_license_fetch_bad_jwt)]
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_license_from_reference_cases(
        #[case] layers: Vec<ImageLayer>,
        #[case] assert_result: fn(Result<License, OciError>),
    ) {
        let mock_server = &MockServer::start().await;
        let mut client = Client::new(ClientConfig {
            protocol: ClientProtocol::Http,
            ..Default::default()
        });
        let image_reference = setup_mocks(mock_server, layers, None).await;

        let result = fetch_license_from_reference(
            &mut client,
            &RegistryAuth::Anonymous,
            &image_reference,
            None,
        )
        .await;

        assert_result(result);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_success() {
        let mock_server = &MockServer::start().await;
        let license_layer = ImageLayer {
            data: TEST_LICENSE_JWT.into(),
            media_type: ENTITLEMENT_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![license_layer], None).await;
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let results = stream_license_from_oci(oci_config)
            .take(1)
            .collect::<Vec<_>>()
            .await;

        assert_eq!(results.len(), 1);
        let expected = License::from_str(TEST_LICENSE_JWT).expect("test JWT must parse");
        match &results[0] {
            Ok(license) => assert_eq!(license.claims, expected.claims),
            Err(e) => panic!("expected success, got error: {e}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_digest_unchanged_no_fetch() {
        let mock_server = &MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";
        let manifest_info = create_manifest_from_license_layer(TEST_LICENSE_JWT.as_bytes(), None);
        let blob_url = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{}",
            mock_server.uri(),
            manifest_info.blob_digest
        ))
        .expect("url must be valid");

        // Count blob (data) requests: should only fire on the first poll.
        let blob_request_count = Arc::new(AtomicUsize::new(0));
        let blob_count = blob_request_count.clone();
        let license_data = manifest_info.license_data;
        Mock::given(method("GET"))
            .and(path(blob_url.path()))
            .respond_with(move |_request: &Request| {
                blob_count.fetch_add(1, Ordering::Relaxed);
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(license_data.clone())
            })
            .mount(mock_server)
            .await;

        let manifest_url = Url::parse(&format!(
            "{}/v2/{}/manifests/{}",
            mock_server.uri(),
            graph_id,
            reference
        ))
        .expect("url must be valid");

        // Increment a counter for HEAD (digest) requests: used below to prove
        // the poll loop has completed an additional unchanged-digest cycle.
        let head_request_count = Arc::new(AtomicUsize::new(0));
        let head_count = head_request_count.clone();
        let head_manifest_digest = manifest_info.manifest_digest.clone();
        let _ = Mock::given(method("HEAD"))
            .and(path(manifest_url.path()))
            .respond_with(move |_request: &Request| {
                head_count.fetch_add(1, Ordering::Relaxed);
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &head_manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
            })
            .mount(mock_server)
            .await;

        // Respond to a GET request with a valid OCI manifest and required headers
        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                    .set_body_bytes(serde_json::to_vec(&manifest_info.oci_manifest).unwrap()),
            )
            .mount(mock_server)
            .await;

        let image_reference = format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid");
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let mut stream = stream_license_from_oci(oci_config);

        // First poll: new digest, license should be fetched.
        let first_result = stream.next().await;
        assert!(first_result.is_some());
        let expected = License::from_str(TEST_LICENSE_JWT).expect("test JWT must parse");
        match first_result.unwrap() {
            Ok(license) => assert_eq!(license.claims, expected.claims),
            Err(e) => panic!("expected success, got error: {e}"),
        }
        assert_eq!(
            blob_request_count.load(Ordering::Relaxed),
            1,
            "Blob should be fetched once on first poll"
        );

        // Second poll: digest is unchanged, so blob should not be fetched again.
        // Wait for a third HEAD before asserting: the polling loop is
        // sequential (HEAD -> fetch -> sleep -> HEAD), so once HEAD #3 has
        // been observed, any blob fetch the second cycle would have made has
        // already been counted.
        // The outer timeout Duration prevents hanging by giving us a hard limit
        let poll_completed = timeout(Duration::from_secs(5), async {
            while head_request_count.load(Ordering::Relaxed) < 3 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        assert!(
            poll_completed.is_ok(),
            "expected a second unchanged-digest poll within timeout"
        );
        assert_eq!(
            blob_request_count.load(Ordering::Relaxed),
            1,
            "Blob should not be fetched again when digest is unchanged"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_digest_changed_fetches() {
        let mock_server = &MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";

        // Use different annotations with the same data (the license blob) to simulate
        // a change in data. The different annotations result in different manifest
        // digests, so the stream sees a "changed" manifest and re-fetches
        // the data (the license) even though it hasn't changed.
        // [Using two distinct valid JWTs isn't possible here because the JWKS bundled
        // via `include_str!` only signs one test token.]
        let mut ann1 = BTreeMap::new();
        ann1.insert("v".to_string(), "1".to_string());
        let mut ann2 = BTreeMap::new();
        ann2.insert("v".to_string(), "2".to_string());

        let manifest_info1 =
            create_manifest_from_license_layer(TEST_LICENSE_JWT.as_bytes(), Some(ann1));
        let manifest_info2 =
            create_manifest_from_license_layer(TEST_LICENSE_JWT.as_bytes(), Some(ann2));

        assert_eq!(manifest_info1.blob_digest, manifest_info2.blob_digest);
        assert_ne!(
            manifest_info1.manifest_digest,
            manifest_info2.manifest_digest
        );

        let blob_url = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{}",
            mock_server.uri(),
            manifest_info1.blob_digest
        ))
        .expect("url must be valid");

        let blob_request_count = Arc::new(AtomicUsize::new(0));
        let blob_count = blob_request_count.clone();
        let license_data = manifest_info1.license_data.clone();
        Mock::given(method("GET"))
            .and(path(blob_url.path()))
            .respond_with(move |_request: &Request| {
                blob_count.fetch_add(1, Ordering::Relaxed);
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(license_data.clone())
            })
            .mount(mock_server)
            .await;

        let manifest_url = Url::parse(&format!(
            "{}/v2/{}/manifests/{}",
            mock_server.uri(),
            graph_id,
            reference
        ))
        .expect("url must be valid");

        let _ = Mock::given(method("HEAD"))
            .and(path(manifest_url.path()))
            .respond_with(SequentialManifestDigests {
                digests: Mutex::new(VecDeque::from([
                    manifest_info1.manifest_digest.clone(),
                    manifest_info2.manifest_digest.clone(),
                ])),
            })
            .expect(2..=3)
            .mount(mock_server)
            .await;

        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(SequentialManifests {
                manifests: Mutex::new(VecDeque::from([
                    (
                        manifest_info1.manifest_digest.clone(),
                        serde_json::to_vec(&manifest_info1.oci_manifest).unwrap(),
                    ),
                    (
                        manifest_info2.manifest_digest.clone(),
                        serde_json::to_vec(&manifest_info2.oci_manifest).unwrap(),
                    ),
                ])),
            })
            .expect(2..=3)
            .mount(mock_server)
            .await;

        let image_reference = format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid");
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let mut stream = stream_license_from_oci(oci_config);
        let expected = License::from_str(TEST_LICENSE_JWT).expect("test JWT must parse");

        // First poll: manifest digest 1 is new → fetch.
        let first_result = stream.next().await;
        assert!(first_result.is_some());
        match first_result.unwrap() {
            Ok(license) => assert_eq!(license.claims, expected.claims),
            Err(e) => panic!("expected success, got error: {e}"),
        }

        // Second poll: manifest digest 2 differs → refetch.
        let second_result = stream.next().await;
        assert!(second_result.is_some());
        match second_result.unwrap() {
            Ok(license) => assert_eq!(license.claims, expected.claims),
            Err(e) => panic!("expected success, got error: {e}"),
        }
        assert_eq!(
            blob_request_count.load(Ordering::Relaxed),
            2,
            "Blob should be fetched twice when manifest digest changes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_backoff_error_retry() {
        let mock_server = &MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";

        let manifest_info = create_manifest_from_license_layer(TEST_LICENSE_JWT.as_bytes(), None);
        let blob_url = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{}",
            mock_server.uri(),
            manifest_info.blob_digest
        ))
        .expect("url must be valid");

        Mock::given(method("GET"))
            .and(path(blob_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(manifest_info.license_data.clone()),
            )
            .mount(mock_server)
            .await;

        let manifest_url = Url::parse(&format!(
            "{}/v2/{}/manifests/{}",
            mock_server.uri(),
            graph_id,
            reference
        ))
        .expect("url must be valid");

        let oci_error_body = serde_json::json!({
            "errors": [{
                "code": "TOOMANYREQUESTS",
                "message": "pull request limit exceeded",
                "detail": { "retryAfter": 10 }
            }]
        });

        let _ = Mock::given(method("HEAD"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE),
            )
            .expect(2)
            .mount(mock_server)
            .await;

        // First GET: 429 with Retry-After. Second GET: 200 with the manifest.
        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(SequentialBackoffResponse {
                responses: Mutex::new(VecDeque::from([
                    ResponseTemplate::new(429)
                        .append_header("Retry-After", "10")
                        .append_header(http::header::CONTENT_TYPE, "application/json")
                        .set_body_json(&oci_error_body),
                    ResponseTemplate::new(200)
                        .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                        .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                        .set_body_bytes(serde_json::to_vec(&manifest_info.oci_manifest).unwrap()),
                ])),
            })
            .mount(mock_server)
            .await;

        let image_reference = format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid");
        let oci_config = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: image_reference.to_string(),
            hot_reload: true,
            poll_interval: Duration::from_millis(10),
            use_ssl: false,
        };

        let start_time = tokio::time::Instant::now();
        let mut stream = stream_license_from_oci(oci_config);

        // First stream item should be the 429 error.
        let result = timeout(Duration::from_secs(20), stream.next()).await;
        assert!(
            result.is_ok(),
            "Stream should produce an error within timeout"
        );
        let first_result = result.unwrap();
        assert!(
            first_result.is_some() && first_result.as_ref().unwrap().is_err(),
            "First result should be an error"
        );

        // Second item should be the successfully-parsed license after backoff.
        let result = timeout(Duration::from_secs(20), stream.next()).await;
        assert!(
            result.is_ok(),
            "Stream should produce a result after backoff within timeout"
        );
        let elapsed = start_time.elapsed();
        let expected = License::from_str(TEST_LICENSE_JWT).expect("test JWT must parse");

        match result.unwrap() {
            Some(Ok(license)) => assert_eq!(license.claims, expected.claims),
            Some(Err(e)) => panic!("expected success after backoff retry, got error: {e}"),
            None => panic!("expected stream to yield a result"),
        }

        assert!(
            elapsed >= Duration::from_secs(10),
            "Should have slept for at least 10 seconds due to backoff, but elapsed time was {:?}",
            elapsed
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_oci_license_stream_valid_reference() {
        let mock_server = &MockServer::start().await;
        let license_layer = ImageLayer {
            data: TEST_LICENSE_JWT.into(),
            media_type: ENTITLEMENT_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![license_layer], None).await;
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let result = create_oci_license_stream(oci_config);
        assert!(result.is_ok(), "valid reference should build a stream");

        let mut stream = result.unwrap();
        let first_result = stream.next().await;
        assert!(first_result.is_some(), "stream should yield a first item");

        let expected = License::from_str(TEST_LICENSE_JWT).expect("test JWT must parse");
        match first_result.unwrap() {
            Ok(license) => assert_eq!(license.claims, expected.claims),
            Err(e) => panic!("expected success, got error: {e}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_oci_license_stream_invalid_reference() {
        // Empty reference fails `validate_oci_reference`, so `create_oci_license_stream`
        // should surface the error rather than build a stream.
        let oci_config = mock_oci_config_with_reference(String::new());
        let result = create_oci_license_stream(oci_config);
        assert!(
            result.is_err(),
            "invalid reference should fail before building a stream"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_license_oci_success() {
        let mock_server = &MockServer::start().await;
        let license_layer = ImageLayer {
            data: TEST_LICENSE_JWT.into(),
            media_type: ENTITLEMENT_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![license_layer], None).await;
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let license = fetch_license_oci(&oci_config)
            .await
            .expect("failed to fetch license via outer wrapper");

        let expected = License::from_str(TEST_LICENSE_JWT).expect("test JWT must parse");
        assert_eq!(license.claims, expected.claims);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_license_oci_surfaces_fetch_error() {
        // MockServer with no mounts — every request 404s. Proves the outer
        // wrapper doesn't swallow the underlying `OciDistributionError` and
        // maps it through `?` into `OciError` cleanly.
        let mock_server = &MockServer::start().await;
        let image_reference = format!("{}/test-graph-id:latest", mock_server.address());
        let oci_config = mock_oci_config_with_reference(image_reference);

        let err = fetch_license_oci(&oci_config)
            .await
            .expect_err("fetch should fail when the registry returns nothing");

        assert!(
            matches!(err, OciError::Distribution(_)),
            "expected OciError::Distribution, got {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_yields_error_and_continues() {
        // First blob GET returns 500, second returns the license. This proves
        // three things at once:
        //   1. an error from `fetch_license_oci` propagates as a stream Err item,
        //   2. the poll loop keeps running after emitting an error, and
        //   3. `last_digest` is NOT updated on a failed fetch — otherwise the
        //      second poll would see an "unchanged" digest and skip refetching,
        //      and the stream would never emit an Ok item.
        let mock_server = &MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";
        let manifest_info = create_manifest_from_license_layer(TEST_LICENSE_JWT.as_bytes(), None);

        // Manifest HEAD/GET always succeed with the same digest.
        let manifest_url = Url::parse(&format!(
            "{}/v2/{}/manifests/{}",
            mock_server.uri(),
            graph_id,
            reference
        ))
        .expect("url must be valid");
        let _ = Mock::given(method("HEAD"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE),
            )
            .mount(mock_server)
            .await;
        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                    .set_body_bytes(serde_json::to_vec(&manifest_info.oci_manifest).unwrap()),
            )
            .mount(mock_server)
            .await;

        // Blob GET fails once, then succeeds.
        let blob_url = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{}",
            mock_server.uri(),
            manifest_info.blob_digest
        ))
        .expect("url must be valid");
        let license_data = manifest_info.license_data.clone();
        let _ = Mock::given(method("GET"))
            .and(path(blob_url.path()))
            .respond_with(SequentialBackoffResponse {
                responses: Mutex::new(VecDeque::from([
                    ResponseTemplate::new(500),
                    ResponseTemplate::new(200)
                        .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                        .set_body_bytes(license_data),
                ])),
            })
            .mount(mock_server)
            .await;

        let image_reference = format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid");
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let mut stream = stream_license_from_oci(oci_config);

        // Poll 1: blob 500 → stream emits Err.
        let first_result = timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("first item should arrive")
            .expect("stream should not have closed");
        assert!(
            first_result.is_err(),
            "expected first result to be an error, got {first_result:?}"
        );

        // Poll 2: same manifest digest, but because the previous fetch failed
        // `last_digest` is still None, so the stream re-fetches and succeeds.
        let second_result = timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("second item should arrive")
            .expect("stream should not have closed");
        let expected = License::from_str(TEST_LICENSE_JWT).expect("test JWT must parse");
        match second_result {
            Ok(license) => assert_eq!(license.claims, expected.claims),
            Err(e) => panic!("expected success after retry, got error: {e}"),
        }
    }

    #[rstest::rstest]
    #[case::external_registry("registry.apollographql.com/my-graph:latest")]
    #[case::docker_io("docker.io/library/alpine:latest")]
    #[case::invalid_reference_defaults_true("")]
    #[case::no_substring_match("localhost.example.com/my-graph:latest")]
    fn should_use_ssl_true(#[case] reference: &str) {
        assert!(should_use_ssl(reference));
    }

    #[rstest::rstest]
    #[case::localhost("localhost:5000/test-graph:latest")]
    #[case::loopback("127.0.0.1:5000/test-graph:latest")]
    #[case::dockerhost("dockerhost:5000/test-graph:latest")]
    fn should_use_ssl_false(#[case] reference: &str) {
        assert!(!should_use_ssl(reference));
    }

    #[rstest::rstest]
    #[case::comma_separated("host1,host2,host3", vec!["host1", "host2", "host3"])]
    #[case::with_whitespace(" host1 , host2 , host3 ", vec!["host1", "host2", "host3"])]
    #[case::empty_string("", vec![])]
    #[case::trailing_commas("host1,,host2,", vec!["host1", "host2"])]
    #[case::single_host("myregistry.local", vec!["myregistry.local"])]
    fn parse_unsecure_hosts_cases(#[case] input: &str, #[case] expected: Vec<&str>) {
        assert_eq!(parse_unsecure_hosts(input), expected);
    }

    #[rstest::rstest]
    #[case::exact("myregistry.local", &["myregistry.local"], true)]
    #[case::with_port("myregistry.local:5000", &["myregistry.local"], true)]
    #[case::no_match("other.registry.com", &["myregistry.local"], false)]
    #[case::empty_list("localhost", &[], false)]
    #[case::default_localhost("localhost", DEFAULT_UNSECURE_HOSTS, true)]
    #[case::default_localhost_port("localhost:5000", DEFAULT_UNSECURE_HOSTS, true)]
    #[case::default_loopback("127.0.0.1", DEFAULT_UNSECURE_HOSTS, true)]
    #[case::default_loopback_port("127.0.0.1:5000", DEFAULT_UNSECURE_HOSTS, true)]
    #[case::default_dockerhost("dockerhost", DEFAULT_UNSECURE_HOSTS, true)]
    #[case::default_dockerhost_port("dockerhost:5000", DEFAULT_UNSECURE_HOSTS, true)]
    #[case::default_docker_io("docker.io", DEFAULT_UNSECURE_HOSTS, false)]
    #[case::default_apollo("registry.apollographql.com", DEFAULT_UNSECURE_HOSTS, false)]
    #[case::no_substring("localhost.example.com", &["localhost"], false)]
    #[case::no_prefix_match("notlocalhost", &["localhost"], false)]
    #[case::custom_replaces_defaults("internal.registry.corp", &["internal.registry.corp"], true)]
    #[case::custom_port("internal.registry.corp:8080", &["internal.registry.corp"], true)]
    #[case::custom_missing_localhost("localhost", &["internal.registry.corp"], false)]
    #[case::ipv6_match("[::1]", &["::1"], true)]
    #[case::ipv6_match_port("[::1]:5000", &["::1"], true)]
    #[case::ipv6_no_match("localhost", &["::1"], false)]
    fn is_unsecure_host_cases(
        #[case] registry: &str,
        #[case] hosts: &[&str],
        #[case] expected: bool,
    ) {
        let hosts: Vec<String> = hosts.iter().map(|s| s.to_string()).collect();
        assert_eq!(is_unsecure_host(registry, &hosts), expected);
    }

    #[rstest::rstest]
    #[case::simple("localhost", "localhost")]
    #[case::simple_port("localhost:5000", "localhost")]
    #[case::ipv4("127.0.0.1", "127.0.0.1")]
    #[case::ipv4_port("127.0.0.1:5000", "127.0.0.1")]
    #[case::ipv6("[::1]", "::1")]
    #[case::ipv6_port("[::1]:5000", "::1")]
    #[case::domain_port("registry.example.com:443", "registry.example.com")]
    #[case::http_scheme("http://localhost:5000", "localhost")]
    #[case::http_ipv4("http://127.0.0.1:5000", "127.0.0.1")]
    #[case::https_scheme("https://registry.example.com", "registry.example.com")]
    #[case::https_port("https://registry.example.com:443", "registry.example.com")]
    #[case::https_path("https://registry.example.com/v2/repo", "registry.example.com")]
    #[case::http_path("http://localhost:5000/v2/my-graph/manifests/latest", "localhost")]
    #[case::http_ipv6("http://[::1]:5000", "::1")]
    fn extract_host_cases(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(extract_host(input), Some(expected.to_string()));
    }

    #[test]
    fn test_validate_oci_reference_valid_cases() {
        // Test valid digest references with different algorithms
        // Using full OCI reference format: registry/repo@algorithm:digest
        let valid_digest_refs = vec![
            "artifact.api.apollographql.com/my-graph@sha256:142067152bd8e2c1411c87ef872cb27d2d5053f55a5a70b00068c5789dc27682",
            "registry.example.com/repo@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "localhost:5000/my-repo@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "docker.io/library/alpine@sha256:1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd",
        ];

        for ref_str in valid_digest_refs {
            let result = validate_oci_reference(ref_str);
            assert!(
                result.is_ok(),
                "Digest reference '{}' should be valid",
                ref_str
            );
            let (reference, ref_type) = result.unwrap();
            assert_eq!(reference, ref_str);
            assert_eq!(ref_type, OciReferenceType::Digest);
        }

        // Test valid tag references
        // Using full OCI reference format: registry/repo:tag
        let valid_tag_refs = vec![
            "artifact.api.apollographql.com/my-graph:latest",
            "registry.example.com/repo:v1.0.0",
            "localhost:5000/my-repo:tag_name",
            "docker.io/library/alpine:tag-name",
            "registry.example.com/repo:tag.name",
            "registry.example.com/repo:v1_2_3",
            "registry.example.com/repo:a",
            // Leading underscore is allowed
            "registry.example.com/repo:_a",
            "registry.example.com/repo:22.04",
            "registry.example.com/repo:v1.2.3",
            "registry.example.com/repo:prod-build.1",
            "registry.example.com/repo:dev",
            "registry.example.com/repo:v0.0.0-alpha",
            "registry.example.com/repo:release-2025",
            "registry.example.com/repo:z",
            "registry.example.com/repo:LATEST",
            "registry.example.com/repo:ProdBuild",
            "registry.example.com/repo:RC_1",
            // Tags that look like digests (64 hex chars) are legal
            "registry.example.com/repo:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
            "registry.example.com/repo:test-9f86d081884c7d65",
        ];

        for ref_str in valid_tag_refs {
            let result = validate_oci_reference(ref_str);
            assert!(
                result.is_ok(),
                "Tag reference '{}' should be valid",
                ref_str
            );
            let (reference, ref_type) = result.unwrap();
            assert_eq!(reference, ref_str);
            assert_eq!(ref_type, OciReferenceType::Tag);
        }
    }

    #[test]
    fn test_validate_oci_reference_invalid_cases() {
        let invalid_references = vec![
            // Invalid reference, no registry (not covered by parse())
            "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdeg",
            // Invalid OCI reference formats (covered by parse())
            "",
            // Invalid digest formats - invalid hex characters
            "registry.example.com/repo@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdeg",
            "registry.example.com/repo@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcde!",
            // Invalid digest formats - too long
            "registry.example.com/repo@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1",
            // Invalid digest formats - invalid characters (spaces, dashes, colons)
            "registry.example.com/repo@sha256: 1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
            "registry.example.com/repo@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef ",
            "registry.example.com/repo@sha256:12345678-90abcdef-12345678-90abcdef-12345678-90abcdef-12345678-90abcdef",
            "registry.example.com/repo@sha256:12345678:90abcdef:12345678:90abcdef:12345678:90abcdef:12345678:90abcdef",
            // Invalid tag formats - starts with invalid character
            "registry.example.com/repo:-latest",
            "registry.example.com/repo:.123",
            "registry.example.com/repo:!boom",
            "registry.example.com/repo: latest",
            // Invalid tag formats - contains invalid chars
            "registry.example.com/repo:my tag",      // spaces
            "registry.example.com/repo:ver#1",       // # not allowed
            "registry.example.com/repo:hello/world", // / not allowed
            "registry.example.com/repo:alpha@beta",  // @ not allowed
            "registry.example.com/repo:tag?test",    // ? not allowed
            // Invalid tag formats - missing tag after colon
            "registry.example.com/repo:",
            "registry.example.com/repo::",
            // Invalid tag formats - tag exceeds max length (129 chars)
            "registry.example.com/repo:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ];

        for reference in invalid_references {
            let result = validate_oci_reference(reference);
            assert!(
                result.is_err(),
                "Reference '{}' should be invalid",
                reference
            );
        }
    }

    #[rstest::rstest]
    #[case::with_launch_id(
        Some(generate_manifest_annotations(Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"))),
        Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string())
    )]
    #[case::no_manifest_annotations(None, None)]
    #[case::manifest_without_launch_id(Some(generate_manifest_annotations(None)), None)]
    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_oci_launch_id_cases(
        #[case] manifest_annotations: Option<BTreeMap<String, String>>,
        #[case] expected_launch_id: Option<String>,
    ) {
        let mock_server = &MockServer::start().await;
        let image_reference = setup_mocks(
            mock_server,
            vec![schema_layer("test schema")],
            manifest_annotations,
        )
        .await;
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let results = stream_from_oci(oci_config)
            .take(1)
            .collect::<Vec<_>>()
            .await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "test schema");
                assert_eq!(schema_state.launch_id, expected_launch_id);
            }
            Err(e) => panic!("expected success, got error: {e}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_oci_digest_unchanged_no_fetch() {
        let mock_server = &MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";
        let manifest_info = create_manifest_from_schema_layer("test schema", None);
        let blob_url = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{}",
            mock_server.uri(),
            manifest_info.blob_digest
        ))
        .expect("url must be valid");

        // Track blob requests - should only be called once (on first poll)
        let blob_request_count = Arc::new(AtomicUsize::new(0));
        let blob_count = blob_request_count.clone();
        let schema_data = manifest_info.schema_data;
        Mock::given(method("GET"))
            .and(path(blob_url.path()))
            .respond_with(move |_request: &wiremock::Request| {
                blob_count.fetch_add(1, Ordering::Relaxed);
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(schema_data.clone())
            })
            .mount(mock_server)
            .await;

        let manifest_url = Url::parse(&format!(
            "{}/v2/{}/manifests/{}",
            mock_server.uri(),
            graph_id,
            reference
        ))
        .expect("url must be valid");

        // HEAD requests always return the same digest (unchanged)
        let _ = Mock::given(method("HEAD"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE),
            )
            .mount(mock_server)
            .await;

        // GET requests for manifest
        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                    .set_body_bytes(serde_json::to_vec(&manifest_info.oci_manifest).unwrap()),
            )
            .mount(mock_server)
            .await;

        let image_reference = format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid");
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let mut stream = stream_from_oci(oci_config);

        // first poll: digest is new, so schema should be fetched
        let first_result = stream.next().await;
        assert!(first_result.is_some());
        match first_result.unwrap() {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "test schema");
            }
            Err(e) => panic!("expected success, got error: {e}"),
        }
        assert_eq!(
            blob_request_count.load(Ordering::Relaxed),
            1,
            "Blob should be fetched once on first poll"
        );

        // second poll: digest is unchanged, so schema should not be fetched, wait for interval
        tokio::time::sleep(Duration::from_millis(50)).await;

        let timeout_result = timeout(Duration::from_millis(100), stream.next()).await;
        // should time out, it means no new result was produced since digest is unchanged
        assert!(
            timeout_result.is_err(),
            "Expected no new result when digest is unchanged"
        );
        assert_eq!(
            blob_request_count.load(Ordering::Relaxed),
            1,
            "Blob should not be fetched again when digest is unchanged"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_create_oci_schema_stream_tag_with_hot_reload() {
        let mock_server = &MockServer::start().await;
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![schema_layer], None).await;

        // Create OciConfig with tag reference and hot-reload enabled
        let oci_config = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: image_reference.to_string(),
            hot_reload: true,
            poll_interval: Duration::from_millis(10),
            use_ssl: false,
        };

        let result = create_oci_schema_stream(oci_config);
        assert!(result.is_ok(), "Tag with hot-reload should succeed");

        let mut stream = result.unwrap();
        let first_result = stream.next().await;
        assert!(
            first_result.is_some(),
            "Stream should yield at least one result"
        );
        match first_result.unwrap() {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "test schema");
            }
            Err(e) => panic!("Expected success, got error: {e}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_create_oci_schema_stream_tag_without_hot_reload() {
        let mock_server = &MockServer::start().await;
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![schema_layer], None).await;

        // Create OciConfig with tag reference and hot-reload disabled
        let oci_config = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: image_reference.to_string(),
            hot_reload: false,
            poll_interval: Duration::from_millis(10),
            use_ssl: false,
        };

        let result = create_oci_schema_stream(oci_config);
        assert!(result.is_err(), "Tag without hot-reload should fail");
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(
                error_msg.contains("Tag references without --hot-reload are not yet supported."),
                "Error message should mention hot-reload requirement, got: {}",
                error_msg
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_create_oci_schema_stream_digest_with_hot_reload() {
        // Create a digest reference
        let digest_reference = "registry.example.com/repo@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

        // Create OciConfig with digest reference and hot-reload enabled
        let oci_config = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: digest_reference.to_string(),
            hot_reload: true,
            poll_interval: Duration::from_millis(10),
            use_ssl: true,
        };

        let result = create_oci_schema_stream(oci_config);
        assert!(result.is_err(), "Digest with hot-reload should fail");
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(
                error_msg.contains(
                    "Digest references are immutable so --hot-reload flag is not allowed."
                ),
                "Error message should mention that hot-reload cannot be enabled for digests, got: {}",
                error_msg
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_create_oci_schema_stream_digest_without_hot_reload() {
        let mock_server = &MockServer::start().await;
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };

        // Create manifest first to get the digest
        let oci_manifest = create_manifest_from_schema_layer("test schema", None);
        let manifest_digest = oci_manifest.manifest_digest.clone();

        // Set up mocks manually for digest reference
        let graph_id = "test-graph-id";
        let blob_digest = schema_layer.sha256_digest();
        let blob_url = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{blob_digest}",
            mock_server.uri()
        ))
        .expect("url must be valid");

        Mock::given(method("GET"))
            .and(path(blob_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(schema_layer.data.clone()),
            )
            .mount(mock_server)
            .await;

        let manifest_digest_url = Url::parse(&format!(
            "{}/v2/{graph_id}/manifests/{}",
            mock_server.uri(),
            manifest_digest
        ))
        .expect("url must be valid");

        // Set up HEAD request for manifest digest
        Mock::given(method("HEAD"))
            .and(path(manifest_digest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE),
            )
            .mount(mock_server)
            .await;

        // Set up GET request for manifest digest
        Mock::given(method("GET"))
            .and(path(manifest_digest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                    .set_body_bytes(serde_json::to_vec(&oci_manifest.oci_manifest).unwrap()),
            )
            .mount(mock_server)
            .await;

        // Create digest reference
        let digest_ref = format!("{}/{graph_id}@{}", mock_server.address(), manifest_digest);

        // Create OciConfig with digest reference and hot-reload disabled
        let oci_config_digest = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: digest_ref,
            hot_reload: false,
            poll_interval: Duration::from_millis(10),
            use_ssl: false,
        };

        let result = create_oci_schema_stream(oci_config_digest);
        assert!(result.is_ok(), "Digest without hot-reload should succeed");

        let mut stream = result.unwrap();
        let first_result = stream.next().await;
        assert!(first_result.is_some(), "Stream should yield one result");
        match first_result.unwrap() {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "test schema");
            }
            Err(e) => panic!("Expected success, got error: {e}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_oci_digest_changed_fetches_schema() {
        let mock_server = &MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";
        let blob_request_count = Arc::new(AtomicUsize::new(0));

        let manifest_info1 = create_manifest_from_schema_layer("schema 1", None);
        let blob_url1 = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{}",
            mock_server.uri(),
            manifest_info1.blob_digest
        ))
        .expect("url must be valid");

        let blob_count1 = blob_request_count.clone();
        Mock::given(method("GET"))
            .and(path(blob_url1.path()))
            .respond_with(move |_request: &Request| {
                blob_count1.fetch_add(1, Ordering::Relaxed);
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(manifest_info1.schema_data.clone())
            })
            .mount(mock_server)
            .await;

        let manifest_info2 = create_manifest_from_schema_layer("schema 2", None);
        let blob_url2 = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{}",
            mock_server.uri(),
            manifest_info2.blob_digest
        ))
        .expect("url must be valid");
        let blob_count2 = blob_request_count.clone();
        Mock::given(method("GET"))
            .and(path(blob_url2.path()))
            .respond_with(move |_request: &Request| {
                blob_count2.fetch_add(1, Ordering::Relaxed);
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(manifest_info2.schema_data.clone())
            })
            .mount(mock_server)
            .await;

        let manifest_url = Url::parse(&format!(
            "{}/v2/{}/manifests/{}",
            mock_server.uri(),
            graph_id,
            reference
        ))
        .expect("url must be valid");

        // mock returns digest1, then digest2 sequentially
        // Stream loop: 2 HEAD requests (one per poll to check if digest changed)
        let _ = Mock::given(method("HEAD"))
            .and(path(manifest_url.path()))
            .respond_with(SequentialManifestDigests {
                digests: Mutex::new(VecDeque::from([
                    manifest_info1.manifest_digest.clone(),
                    manifest_info2.manifest_digest.clone(),
                ])),
            })
            .expect(2..=3)
            .mount(mock_server)
            .await;

        // mock requests for manifest1 then manifest2
        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(SequentialManifests {
                manifests: Mutex::new(VecDeque::from([
                    (
                        manifest_info1.manifest_digest,
                        serde_json::to_vec(&manifest_info1.oci_manifest).unwrap(),
                    ),
                    (
                        manifest_info2.manifest_digest,
                        serde_json::to_vec(&manifest_info2.oci_manifest).unwrap(),
                    ),
                ])),
            })
            .expect(2..=3)
            .mount(mock_server)
            .await;

        let image_reference = format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid");
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let mut stream = stream_from_oci(oci_config);

        // first poll: digest1 is new, so schema1 should be fetched
        let first_result = stream.next().await;
        assert!(first_result.is_some());
        match first_result.unwrap() {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "schema 1");
            }
            Err(e) => panic!("expected success, got error: {e}"),
        }

        // second poll: digest2 is different, so schema2 should be fetched
        let second_result = stream.next().await;
        assert!(second_result.is_some());
        match second_result.unwrap() {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "schema 2");
            }
            Err(e) => panic!("expected success, got error: {e}"),
        }
        assert_eq!(
            blob_request_count.load(Ordering::Relaxed),
            2,
            "Both blobs should be fetched when digest changes"
        );
    }

    struct SequentialBackoffResponse {
        responses: Mutex<VecDeque<ResponseTemplate>>,
    }

    impl Respond for SequentialBackoffResponse {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            self.responses
                .lock()
                .pop_front()
                .expect("should have enough responses")
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_oci_backoff_error_retry() {
        let mock_server = &MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";

        let manifest_info = create_manifest_from_schema_layer("test schema", None);
        let blob_url = Url::parse(&format!(
            "{}/v2/{graph_id}/blobs/{}",
            mock_server.uri(),
            manifest_info.blob_digest
        ))
        .expect("url must be valid");

        Mock::given(method("GET"))
            .and(path(blob_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(manifest_info.schema_data.clone()),
            )
            .mount(mock_server)
            .await;

        let manifest_url = Url::parse(&format!(
            "{}/v2/{}/manifests/{}",
            mock_server.uri(),
            graph_id,
            reference
        ))
        .expect("url must be valid");

        // First GET request returns 429 with Retry-After header and OCI error envelope, second returns 200
        let oci_error_body = serde_json::json!({
            "errors": [{
                "code": "TOOMANYREQUESTS",
                "message": "pull request limit exceeded",
                "detail": {
                    "retryAfter": 10
                }
            }]
        });
        let _ = Mock::given(method("HEAD"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE),
            )
            .expect(2)
            .mount(mock_server)
            .await;

        // GET request for manifest
        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(SequentialBackoffResponse {
                responses: Mutex::new(VecDeque::from([
                    // First response 429 to rate limit
                    ResponseTemplate::new(429)
                        .append_header("Retry-After", "10")
                        .append_header(http::header::CONTENT_TYPE, "application/json")
                        .set_body_json(&oci_error_body),
                    // Second response 200 to return the manifest
                    ResponseTemplate::new(200)
                        .append_header("Docker-Content-Digest", &manifest_info.manifest_digest)
                        .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                        .set_body_bytes(serde_json::to_vec(&manifest_info.oci_manifest).unwrap()),
                ])),
            })
            .mount(mock_server)
            .await;

        let image_reference = format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid");
        let oci_config = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: image_reference.to_string(),
            hot_reload: true,
            poll_interval: Duration::from_millis(10),
            use_ssl: false,
        };

        let start_time = tokio::time::Instant::now();
        let mut stream = stream_from_oci(oci_config);

        // The stream should eventually succeed after the backoff period
        // Use a timeout to ensure the test completes
        let result = timeout(Duration::from_secs(20), stream.next()).await;
        assert!(
            result.is_ok(),
            "Stream should produce an error first within timeout"
        );
        let first_result = result.unwrap();
        assert!(
            first_result.is_some() && first_result.as_ref().unwrap().is_err(),
            "First result should be an error"
        );

        let result = timeout(Duration::from_secs(20), stream.next()).await;
        assert!(
            result.is_ok(),
            "Stream should produce a result after the backoff period second within timeout"
        );

        let elapsed = start_time.elapsed();

        match result.unwrap() {
            Some(Ok(schema_state)) => {
                assert_eq!(schema_state.sdl, "test schema");
            }
            Some(Err(e)) => panic!("expected success after backoff retry, got error: {e}"),
            None => panic!("expected stream to yield a result"),
        }

        // Verify that at least 10 seconds elapsed (the retry_after_secs from Retry-After header)
        assert!(
            elapsed >= Duration::from_secs(10),
            "Should have slept for at least 10 seconds due to backoff, but elapsed time was {:?}",
            elapsed
        );
    }
}
