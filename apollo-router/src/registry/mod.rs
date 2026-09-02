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
    #[error("oci layer does not have a title")]
    LayerMissingTitle,
    #[error("oci manifest has no entitlement layer")]
    EntitlementLayerMissing,
    #[error("oci distribution error: {0}")]
    Distribution(OciDistributionError),
    #[error("oci parsing error: {0}")]
    Parse(oci_client::ParseError),
    #[error("unable to parse layer: {0}")]
    LayerParse(FromUtf8Error),
}

const APOLLO_REGISTRY_ENDING: &str = "apollographql.com";
const APOLLO_REGISTRY_USERNAME: &str = "apollo-registry";
const APOLLO_SCHEMA_MEDIA_TYPE: &str = "application/apollo.schema";
const APOLLO_MANIFEST_LAUNCH_ID_ANNOTATION: &str = "com.apollograph.launch.id";
/// Annotation on the graph artifact manifest carrying the account's opaque entitlement
/// identifier. Absent on manifests built before entitlement-over-OCI shipped; the router then
/// runs unlicensed until the graph is republished.
const APOLLO_MANIFEST_ENTITLEMENT_ID_ANNOTATION: &str = "com.apollograph.graph.entitlement.id";
/// Media type of the layer carrying the signed entitlement JWT inside the entitlement artifact.
const APOLLO_ENTITLEMENT_MEDIA_TYPE: &str = "application/vnd.apollographql.entitlement.v1+jwt";
/// Repository namespace for entitlement artifacts: `<registry>/entitlements/<identifier>`.
const ENTITLEMENTS_REPO_PATH: &str = "entitlements";
/// Entitlement artifacts are always fetched via the moving `latest` tag.
const ENTITLEMENT_TAG: &str = "latest";

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
        .ok_or(OciError::LayerMissingTitle)?
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
    fetch_reference_manifest_digest(oci_config, &reference).await
}

/// Fetch the manifest digest for an arbitrary reference in the same registry, authenticated with
/// the graph API key from the config. Lets the entitlement poll reuse the schema path's probe.
async fn fetch_reference_manifest_digest(
    oci_config: &OciConfig,
    reference: &Reference,
) -> Result<String, OciError> {
    let auth = build_auth(reference, &oci_config.apollo_key);
    let protocol = oci_config.client_protocol();

    let client = Client::new(ClientConfig {
        protocol,
        ..Default::default()
    });
    let before_request = Instant::now();
    let registry = reference.registry().to_string();
    let result = client.fetch_manifest_digest(reference, &auth).await;
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

/// The entitlement JWT fetched for the graph, or `None` when the graph artifact manifest carries
/// no entitlement identifier annotation (a manifest built before entitlement-over-OCI shipped);
/// the router then runs unlicensed until the graph is republished.
type OciLicenseJwt = Option<String>;

/// Type alias for OCI license stream
type OciLicenseStream = Pin<Box<dyn Stream<Item = Result<OciLicenseJwt, OciError>> + Send>>;

/// Build the reference for the account's entitlement artifact: the same registry the graph
/// artifact came from, repository `entitlements/<identifier>`, tag `latest`.
fn entitlement_reference(
    graph_reference: &Reference,
    entitlement_id: &str,
) -> Result<Reference, OciError> {
    format!(
        "{}/{}/{}:{}",
        graph_reference.registry(),
        ENTITLEMENTS_REPO_PATH,
        entitlement_id,
        ENTITLEMENT_TAG
    )
    .parse::<Reference>()
    .map_err(OciError::from)
}

/// Read the entitlement identifier off the graph artifact manifest annotations. `Ok(None)` means
/// the manifest predates the annotation.
async fn fetch_entitlement_id(
    client: &mut Client,
    auth: &RegistryAuth,
    graph_reference: &Reference,
) -> Result<Option<String>, OciError> {
    let (manifest, _) = fetch_oci_manifest(client, auth, graph_reference, None).await?;
    Ok(manifest
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(APOLLO_MANIFEST_ENTITLEMENT_ID_ANNOTATION))
        .cloned())
}

/// Fetch the signed entitlement JWT: the entitlement artifact's manifest, then its JWT layer.
async fn fetch_entitlement_jwt(
    client: &mut Client,
    auth: &RegistryAuth,
    entitlement_ref: &Reference,
) -> Result<String, OciError> {
    let (manifest, _) = fetch_oci_manifest(client, auth, entitlement_ref, None).await?;
    let jwt_layer = manifest
        .layers
        .iter()
        .find(|layer| layer.media_type == APOLLO_ENTITLEMENT_MEDIA_TYPE)
        .ok_or(OciError::EntitlementLayerMissing)?
        .clone();
    let jwt = fetch_oci_blob(client, entitlement_ref, &jwt_layer).await?;
    Ok(String::from_utf8(jwt)?)
}

/// One-shot license fetch: discover the identifier from the graph artifact manifest, then fetch
/// the entitlement JWT with the same graph API key.
pub(crate) async fn fetch_oci_license(oci_config: &OciConfig) -> Result<OciLicenseJwt, OciError> {
    let graph_reference: Reference = oci_config.reference.as_str().parse()?;
    let auth = build_auth(&graph_reference, &oci_config.apollo_key);
    let mut client = Client::new(ClientConfig {
        protocol: oci_config.client_protocol(),
        ..Default::default()
    });

    let Some(entitlement_id) = fetch_entitlement_id(&mut client, &auth, &graph_reference).await?
    else {
        tracing::info!(
            "graph artifact manifest has no entitlement identifier annotation; the router runs unlicensed until the graph is republished"
        );
        return Ok(None);
    };
    let entitlement_ref = entitlement_reference(&graph_reference, &entitlement_id)?;
    let jwt = fetch_entitlement_jwt(&mut client, &auth, &entitlement_ref).await?;
    Ok(Some(jwt))
}

/// A 404-shaped registry error. For entitlements this is expected during rollout (the account is
/// not backfilled yet, or the key lacks access; the proxy deliberately serves both as 404), so it
/// is a quiet retry, never a revocation signal: revocation always arrives as a newer JWT.
fn is_not_found(error: &OciError) -> bool {
    match error {
        OciError::Distribution(OciDistributionError::ImageManifestNotFoundError(_)) => true,
        OciError::Distribution(OciDistributionError::ServerError { code, .. }) => *code == 404,
        OciError::Distribution(OciDistributionError::RegistryError { envelope, .. }) => {
            envelope.errors.iter().any(|error| {
                matches!(
                    error.code,
                    OciErrorCode::ManifestUnknown | OciErrorCode::NotFound
                )
            })
        }
        _ => false,
    }
}

/// Create a license stream from OCI config, gated the same way the schema stream is.
pub(crate) fn create_oci_license_stream(
    oci_config: OciConfig,
) -> Result<OciLicenseStream, anyhow::Error> {
    let (_, ref_type) = validate_oci_reference(&oci_config.reference)?;

    match (ref_type, oci_config.hot_reload) {
        (OciReferenceType::Tag, true) => Ok(Box::pin(stream_license_from_oci(oci_config))),
        (OciReferenceType::Tag, false) => Err(anyhow::anyhow!(
            "Tag references without --hot-reload are not yet supported."
        )),
        (OciReferenceType::Digest, true) => Err(anyhow::anyhow!(
            "Digest references are immutable so --hot-reload flag is not allowed."
        )),
        (OciReferenceType::Digest, false) => {
            let oci_config_clone = oci_config.clone();
            let stream = stream::once(async move { fetch_oci_license(&oci_config_clone).await });
            Ok(Box::pin(stream))
        }
    }
}

/// Poll the registry for license updates. Each tick re-probes the graph artifact manifest (the
/// entitlement identifier is discovered from its annotations and is stable, but a graph
/// republished onto a newer manifest scheme can gain the annotation), then probes the entitlement
/// artifact's `latest` tag and fetches the JWT when its digest moves.
pub(crate) fn stream_license_from_oci(
    oci_config: OciConfig,
) -> impl Stream<Item = Result<OciLicenseJwt, OciError>> {
    let (sender, receiver) = channel(2);

    let task = async move {
        let graph_reference: Reference = match oci_config.reference.as_str().parse() {
            Ok(reference) => reference,
            Err(err) => {
                let _ = sender.send(Err(OciError::from(err))).await;
                return;
            }
        };
        let auth = build_auth(&graph_reference, &oci_config.apollo_key);

        // None until the graph manifest has been read at least once; Some(None) when the manifest
        // was read and carries no annotation.
        let mut entitlement_id: Option<Option<String>> = None;
        let mut last_graph_digest: Option<String> = None;
        let mut last_entitlement_digest: Option<String> = None;
        let mut polling_time = oci_config.poll_interval;

        loop {
            let mut client = Client::new(ClientConfig {
                protocol: oci_config.client_protocol(),
                ..Default::default()
            });

            // Step 1: (re)discover the entitlement identifier when the graph manifest changes.
            match fetch_reference_manifest_digest(&oci_config, &graph_reference).await {
                Ok(graph_digest) => {
                    if last_graph_digest.as_deref() != Some(graph_digest.as_str()) {
                        match fetch_entitlement_id(&mut client, &auth, &graph_reference).await {
                            Ok(discovered) => {
                                if discovered.is_none() {
                                    tracing::info!(
                                        "graph artifact manifest has no entitlement identifier annotation; the router runs unlicensed until the graph is republished"
                                    );
                                    // The unlicensed state is explicit, not an error, and any
                                    // previously fetched entitlement no longer applies.
                                    if entitlement_id != Some(None)
                                        && sender.send(Ok(None)).await.is_err()
                                    {
                                        break;
                                    }
                                    last_entitlement_digest = None;
                                }
                                entitlement_id = Some(discovered);
                                last_graph_digest = Some(graph_digest);
                            }
                            Err(err) => {
                                if let Some(retry_after) = parse_rate_limit_error(&err) {
                                    polling_time = retry_after.max(Duration::from_secs(10));
                                }
                                if sender.send(Err(err)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    if let Some(retry_after) = parse_rate_limit_error(&err) {
                        polling_time = retry_after.max(Duration::from_secs(10));
                    }
                    if sender.send(Err(err)).await.is_err() {
                        break;
                    }
                }
            }

            // Step 2: poll the entitlement artifact's `latest` tag and fetch the JWT on movement.
            if let Some(Some(id)) = &entitlement_id {
                match entitlement_reference(&graph_reference, id) {
                    Ok(entitlement_ref) => {
                        match fetch_reference_manifest_digest(&oci_config, &entitlement_ref).await {
                            Ok(current_digest) => {
                                if last_entitlement_digest.as_deref()
                                    == Some(current_digest.as_str())
                                {
                                    tracing::debug!(
                                        "entitlement manifest digest unchanged, skipping license fetch"
                                    );
                                } else {
                                    match fetch_entitlement_jwt(
                                        &mut client,
                                        &auth,
                                        &entitlement_ref,
                                    )
                                    .await
                                    {
                                        Ok(jwt) => {
                                            if sender.send(Ok(Some(jwt))).await.is_err() {
                                                break;
                                            }
                                            // Only advance on success so a failed fetch retries.
                                            last_entitlement_digest = Some(current_digest);
                                        }
                                        Err(err) => {
                                            if let Some(retry_after) = parse_rate_limit_error(&err)
                                            {
                                                polling_time =
                                                    retry_after.max(Duration::from_secs(10));
                                            }
                                            if sender.send(Err(err)).await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            Err(err) if is_not_found(&err) => {
                                // Expected during rollout (not yet backfilled, or no access; the
                                // proxy serves both as 404). Never a revocation signal, so the
                                // current license state is left untouched and the poll retries.
                                tracing::debug!(
                                    "entitlement artifact not available yet (404); retrying on the next poll"
                                );
                            }
                            Err(err) => {
                                if let Some(retry_after) = parse_rate_limit_error(&err) {
                                    polling_time = retry_after.max(Duration::from_secs(10));
                                }
                                if sender.send(Err(err)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        if sender.send(Err(err)).await.is_err() {
                            break;
                        }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::VecDeque;
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
    use crate::registry::OciError::LayerMissingTitle;

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
            data: schema_data.to_string().into_bytes(),
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
            schema_data: schema_layer.data,
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

    const TEST_ENTITLEMENT_ID: &str = "0a1b2c3d4e5f";
    const TEST_JWT: &str = "header.payload.signature";

    fn entitlement_annotations() -> BTreeMap<String, String> {
        let mut annotations = generate_manifest_annotations(Some("launch-1"));
        annotations.insert(
            APOLLO_MANIFEST_ENTITLEMENT_ID_ANNOTATION.to_string(),
            TEST_ENTITLEMENT_ID.to_string(),
        );
        annotations
    }

    /// Mounts HEAD/GET manifest and GET blob mocks for `entitlements/<id>:latest`, returning the
    /// number of blob fetches via the counter.
    async fn setup_entitlement_mocks(mock_server: &MockServer, jwt: &str) -> Arc<AtomicUsize> {
        let jwt_layer = ImageLayer {
            data: jwt.as_bytes().to_vec(),
            media_type: APOLLO_ENTITLEMENT_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let blob_digest = jwt_layer.sha256_digest();
        let oci_manifest = OciManifest::Image(OciImageManifest {
            schema_version: 2,
            media_type: Some(IMAGE_MANIFEST_MEDIA_TYPE.to_string()),
            config: Default::default(),
            layers: vec![OciDescriptor {
                media_type: jwt_layer.media_type.clone(),
                digest: blob_digest.clone(),
                size: jwt_layer.data.len().try_into().unwrap(),
                urls: None,
                annotations: None,
            }],
            subject: None,
            artifact_type: None,
            annotations: None,
        });
        let manifest_digest = calculate_manifest_digest(&oci_manifest);
        let manifest_path = format!(
            "/v2/{ENTITLEMENTS_REPO_PATH}/{TEST_ENTITLEMENT_ID}/manifests/{ENTITLEMENT_TAG}"
        );

        Mock::given(method("HEAD"))
            .and(path(manifest_path.clone()))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", manifest_digest.clone())
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE),
            )
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(manifest_path))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Docker-Content-Digest", manifest_digest)
                    .append_header(http::header::CONTENT_TYPE, OCI_IMAGE_MEDIA_TYPE)
                    .set_body_bytes(serde_json::to_vec(&oci_manifest).unwrap()),
            )
            .mount(mock_server)
            .await;

        let blob_fetches = Arc::new(AtomicUsize::new(0));
        let counter = blob_fetches.clone();
        let blob_data = jwt_layer.data.clone();
        Mock::given(method("GET"))
            .and(path(format!(
                "/v2/{ENTITLEMENTS_REPO_PATH}/{TEST_ENTITLEMENT_ID}/blobs/{blob_digest}"
            )))
            .respond_with(move |_: &Request| {
                counter.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200)
                    .append_header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .set_body_bytes(blob_data.clone())
            })
            .mount(mock_server)
            .await;
        blob_fetches
    }

    #[test]
    fn test_entitlement_reference() {
        let graph_reference: Reference = "registry.apollographql.com/my-graph:current"
            .parse()
            .unwrap();
        let reference = entitlement_reference(&graph_reference, TEST_ENTITLEMENT_ID).unwrap();
        assert_eq!(reference.registry(), "registry.apollographql.com");
        assert_eq!(
            reference.repository(),
            format!("{ENTITLEMENTS_REPO_PATH}/{TEST_ENTITLEMENT_ID}")
        );
        assert_eq!(reference.tag(), Some(ENTITLEMENT_TAG));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_success() {
        let mock_server = MockServer::start().await;
        let schema_layer = ImageLayer {
            data: b"type Query { hello: String }".to_vec(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let graph_reference = setup_mocks(
            &mock_server,
            vec![schema_layer],
            Some(entitlement_annotations()),
        )
        .await;
        setup_entitlement_mocks(&mock_server, TEST_JWT).await;

        let oci_config = mock_oci_config_with_reference(graph_reference.to_string());
        let mut stream = Box::pin(stream_license_from_oci(oci_config));

        let jwt = stream
            .next()
            .await
            .expect("stream should yield an item")
            .expect("item should not be an error");
        assert_eq!(jwt, Some(TEST_JWT.to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_missing_annotation_is_unlicensed() {
        let mock_server = MockServer::start().await;
        let schema_layer = ImageLayer {
            data: b"type Query { hello: String }".to_vec(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        // Annotations carry a launch ID but no entitlement identifier.
        let graph_reference = setup_mocks(
            &mock_server,
            vec![schema_layer],
            Some(generate_manifest_annotations(Some("launch-1"))),
        )
        .await;

        let oci_config = mock_oci_config_with_reference(graph_reference.to_string());
        let mut stream = Box::pin(stream_license_from_oci(oci_config));

        let jwt = stream
            .next()
            .await
            .expect("stream should yield an item")
            .expect("item should not be an error");
        assert_eq!(
            jwt, None,
            "missing annotation must yield the unlicensed marker"
        );

        // The unlicensed marker is emitted once, not on every poll.
        let second = timeout(Duration::from_millis(200), stream.next()).await;
        assert!(second.is_err(), "no further items while nothing changes");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_entitlement_not_found_is_quiet() {
        let mock_server = MockServer::start().await;
        let schema_layer = ImageLayer {
            data: b"type Query { hello: String }".to_vec(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        // Annotation present, but no entitlement artifact mocks mounted: the registry answers 404,
        // which is expected during rollout and must produce neither a license nor an error item.
        let graph_reference = setup_mocks(
            &mock_server,
            vec![schema_layer],
            Some(entitlement_annotations()),
        )
        .await;

        let oci_config = mock_oci_config_with_reference(graph_reference.to_string());
        let mut stream = Box::pin(stream_license_from_oci(oci_config));

        let item = timeout(Duration::from_millis(300), stream.next()).await;
        assert!(
            item.is_err(),
            "a 404 for the entitlement artifact must not emit items, got {item:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_license_from_oci_digest_unchanged_no_refetch() {
        let mock_server = MockServer::start().await;
        let schema_layer = ImageLayer {
            data: b"type Query { hello: String }".to_vec(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let graph_reference = setup_mocks(
            &mock_server,
            vec![schema_layer],
            Some(entitlement_annotations()),
        )
        .await;
        let blob_fetches = setup_entitlement_mocks(&mock_server, TEST_JWT).await;

        let oci_config = mock_oci_config_with_reference(graph_reference.to_string());
        let mut stream = Box::pin(stream_license_from_oci(oci_config));

        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first, Some(TEST_JWT.to_string()));

        // Let several polls elapse; the unchanged digest must not trigger blob refetches.
        let second = timeout(Duration::from_millis(300), stream.next()).await;
        assert!(
            second.is_err(),
            "no further items while the digest is unchanged"
        );
        assert_eq!(1, blob_fetches.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_create_oci_license_stream_tag_without_hot_reload_errors() {
        let mut oci_config =
            mock_oci_config_with_reference("registry.example.com/graph:latest".to_string());
        oci_config.hot_reload = false;
        assert!(create_oci_license_stream(oci_config).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_create_oci_license_stream_digest_with_hot_reload_errors() {
        let mut oci_config = mock_oci_config_with_reference(
            "registry.example.com/graph@sha256:0123456789012345678901234567890123456789012345678901234567890123"
                .to_string(),
        );
        oci_config.hot_reload = true;
        assert!(create_oci_license_stream(oci_config).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_blob() {
        let mock_server = &MockServer::start().await;
        let mut client = Client::new(ClientConfig {
            protocol: ClientProtocol::Http,
            ..Default::default()
        });
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![schema_layer], None).await;
        let result = fetch_oci_from_reference(
            &mut client,
            &RegistryAuth::Anonymous,
            &image_reference,
            None,
        )
        .await
        .expect("failed to fetch oci bundle");
        assert_eq!(result.schema, "test schema");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handle_extra_layers() {
        let mock_server = &MockServer::start().await;
        let mut client = Client::new(ClientConfig {
            protocol: ClientProtocol::Http,
            ..Default::default()
        });
        let schema_layer = ImageLayer {
            data: "test schema".into(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let random_layer = ImageLayer {
            data: "foo_bar".into(),
            media_type: "foo_bar".to_string(),
            annotations: None,
        };
        let image_reference =
            setup_mocks(mock_server, vec![schema_layer, random_layer], None).await;
        let result = fetch_oci_from_reference(
            &mut client,
            &RegistryAuth::Anonymous,
            &image_reference,
            None,
        )
        .await
        .expect("failed to fetch oci bundle");
        assert_eq!(result.schema, "test schema");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn error_layer_not_found() {
        let mock_server = &MockServer::start().await;
        let mut client = Client::new(ClientConfig {
            protocol: ClientProtocol::Http,
            ..Default::default()
        });
        let random_layer = ImageLayer {
            data: "foo_bar".to_string().into_bytes(),
            media_type: "foo_bar".to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![random_layer], None).await;
        let result = fetch_oci_from_reference(
            &mut client,
            &RegistryAuth::Anonymous,
            &image_reference,
            None,
        )
        .await
        .expect_err("expect can't fetch oci bundle");
        if let LayerMissingTitle = result {
            // Expected error
        } else {
            panic!("expected missing title error, got {result:?}");
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

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_oci_success() {
        let mock_server = &MockServer::start().await;

        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };

        let launch_id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string();
        let manifest_annotations = generate_manifest_annotations(Some(&launch_id.clone()));

        let image_reference =
            setup_mocks(mock_server, vec![schema_layer], Some(manifest_annotations)).await;
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let results = stream_from_oci(oci_config)
            .take(1)
            .collect::<Vec<_>>()
            .await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "test schema");
                assert_eq!(schema_state.launch_id, Some(launch_id));
            }
            Err(e) => panic!("expected success, got error: {e}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_oci_missing_manifests() {
        let mock_server = &MockServer::start().await;

        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };

        let image_reference = setup_mocks(mock_server, vec![schema_layer], None).await;
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let results = stream_from_oci(oci_config)
            .take(1)
            .collect::<Vec<_>>()
            .await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "test schema");
                assert_eq!(schema_state.launch_id, None);
            }
            Err(e) => panic!("expected success, got error: {e}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_oci_missing_launch_id_manifest() {
        let mock_server = &MockServer::start().await;

        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };

        let manifest_annotations = generate_manifest_annotations(None);
        let image_reference =
            setup_mocks(mock_server, vec![schema_layer], Some(manifest_annotations)).await;
        let oci_config = mock_oci_config_with_reference(image_reference.to_string());

        let results = stream_from_oci(oci_config)
            .take(1)
            .collect::<Vec<_>>()
            .await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Ok(schema_state) => {
                assert_eq!(schema_state.sdl, "test schema");
                assert_eq!(schema_state.launch_id, None);
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
            data: "test schema".to_string().into_bytes(),
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
            data: "test schema".to_string().into_bytes(),
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
            data: "test schema".to_string().into_bytes(),
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
