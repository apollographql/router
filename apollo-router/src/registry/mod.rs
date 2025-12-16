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
use oci_client::secrets::RegistryAuth;
use thiserror::Error;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;
use tracing::instrument::WithSubscriber;

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
}

#[derive(Debug, Clone)]
pub(crate) struct OciContent {
    pub schema: String,
}

#[derive(Debug, Error)]
pub(crate) enum OciError {
    #[error("oci layer does not have a title")]
    LayerMissingTitle,
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

    Ok(OciContent {
        schema: String::from_utf8(schema)?,
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
        "apollo.router.oci.manifest.count",
        "Number of requests to get Graph Artifact manifest",
        "{count}",
        1u64,
        registry = registry.clone(),
        kind = "get_manifest",
        status = status
    );
    f64_histogram_with_unit!(
        "apollo.router.oci.manifest.duration.seconds",
        "Duration of request to get Graph Artifact manifest",
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
        "apollo.router.oci.blob.count",
        "Number of requests to get Graph Artifact blob",
        "{count}",
        1u64,
        registry = registry.clone(),
        kind = "get_blob",
        status = status
    );
    f64_histogram_with_unit!(
        "apollo.router.oci.blob.duration.seconds",
        "Duration of request to get Graph Artifact blob",
        "s",
        duration,
        registry = registry,
        kind = "get_blob",
        status = status
    );

    result?;
    Ok(blob_data)
}

/// The oci reference may not contain the protocol, only hostname[:port]. As a result,
/// in order to test locally without SSL, either (1) protocol needs to be exposed as an
/// env var or (2) protocol needs to be inferred from hostname. Rather than introduce a
/// largely unused configuration option, this function checks the hostname for local
/// development/testing and disables SSL accordingly.
async fn infer_oci_protocol(registry: &str) -> ClientProtocol {
    let host = registry.split(":").next().expect("host must be provided");
    if host == "localhost" || host == "127.0.0.1" || host == "dockerhost" {
        ClientProtocol::Http
    } else {
        ClientProtocol::Https
    }
}

/// Fetch the manifest digest (without fetching the full manifest) to detect changes
pub(crate) async fn fetch_oci_manifest_digest(oci_config: &OciConfig) -> Result<String, OciError> {
    let reference: Reference = oci_config.reference.as_str().parse()?;
    let auth = build_auth(&reference, &oci_config.apollo_key);
    let protocol = infer_oci_protocol(reference.resolve_registry()).await;

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
        "Number of requests to get Graph Artifact manifest",
        "{request}",
        1u64,
        registry = registry.clone(),
        kind = "head_manifest",
        status = status
    );
    f64_histogram_with_unit!(
        "apollo.router.oci.manifest.duration.seconds",
        "Duration of request to get Graph Artifact manifest",
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

/// Fetch an OCI bundle by parsing the Graph Artifact reference, building auth,
/// inferring the correct protocol, and calling the internal fetch function.
pub(crate) async fn fetch_oci(oci_config: &OciConfig) -> Result<OciContent, OciError> {
    let reference: Reference = oci_config.reference.as_str().parse()?;
    let auth = build_auth(&reference, &oci_config.apollo_key);
    let protocol = infer_oci_protocol(reference.registry()).await;

    tracing::debug!(
        "prepared to fetch schema from oci over {:?}, auth anonymous? {:?}",
        protocol,
        auth == RegistryAuth::Anonymous
    );

    u64_counter_with_unit!(
        "apollo.router.oci.fullArtifact.count.total",
        "Total number of requests to get full artifact for a Graph Artifact",
        "{count}",
        1u64
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
        loop {
            match fetch_oci_manifest_digest(&oci_config).await {
                Ok(current_digest) => {
                    if last_digest.as_deref() == Some(current_digest.as_str()) {
                        // Digest unchanged, skip fetching the full schema
                        tracing::debug!("oci manifest digest unchanged, skipping schema fetch");
                    } else {
                        // Digest changed, fetch the full schema
                        tracing::debug!("oci manifest digest changed, fetching schema");
                        last_digest = Some(current_digest);

                        match fetch_oci(&oci_config).await {
                            Ok(oci_result) => {
                                tracing::debug!("fetched schema from oci registry");
                                let schema_state = SchemaState {
                                    sdl: oci_result.schema,
                                    launch_id: None, //TODO: Add launch_id
                                };
                                if let Err(e) = sender.send(Ok(schema_state)).await {
                                    tracing::debug!(
                                        "failed to push to stream. This is likely to be because the router is shutting down: {e}"
                                    );
                                    break;
                                }
                            }
                            Err(err) => {
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
                    if let Err(e) = sender.send(Err(err)).await {
                        tracing::debug!(
                            "failed to send error to oci stream. This is likely to be because the router is shutting down: {e}"
                        );
                        break;
                    }
                }
            }

            tokio::time::sleep(oci_config.poll_interval).await;
        }
    };
    drop(tokio::task::spawn(task.with_current_subscriber()));

    ReceiverStream::new(receiver).boxed()
}

#[cfg(test)]
mod tests {
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
        }
    }

    struct SchemaLayerManifest {
        oci_manifest: OciManifest,
        manifest_digest: String,
        blob_digest: String,
        schema_data: Vec<u8>,
    }

    fn create_manifest_from_schema_layer(schema_data: &str) -> SchemaLayerManifest {
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
            annotations: None,
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

    async fn setup_mocks(mock_server: MockServer, layers: Vec<ImageLayer>) -> Reference {
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
                .mount(&mock_server)
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
            annotations: None,
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
            .mount(&mock_server)
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
            .mount(&mock_server)
            .await;

        format!("{}/{graph_id}:{reference}", mock_server.address())
            .parse::<Reference>()
            .expect("url must be valid")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_blob() {
        let mock_server = MockServer::start().await;
        let mut client = Client::new(ClientConfig {
            protocol: ClientProtocol::Http,
            ..Default::default()
        });
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![schema_layer]).await;
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
        let mock_server = MockServer::start().await;
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
        let image_reference = setup_mocks(mock_server, vec![schema_layer, random_layer]).await;
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
        let mock_server = MockServer::start().await;
        let mut client = Client::new(ClientConfig {
            protocol: ClientProtocol::Http,
            ..Default::default()
        });
        let random_layer = ImageLayer {
            data: "foo_bar".to_string().into_bytes(),
            media_type: "foo_bar".to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![random_layer]).await;
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

    #[tokio::test]
    async fn test_infer_oci_protocol_localhost() {
        let result = infer_oci_protocol("localhost").await;
        assert_eq!(result, ClientProtocol::Http);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_localhost_with_port() {
        let result = infer_oci_protocol("localhost:5000").await;
        assert_eq!(result, ClientProtocol::Http);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_127_0_0_1() {
        let result = infer_oci_protocol("127.0.0.1").await;
        assert_eq!(result, ClientProtocol::Http);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_127_0_0_1_with_port() {
        let result = infer_oci_protocol("127.0.0.1:5000").await;
        assert_eq!(result, ClientProtocol::Http);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_docker_io() {
        let result = infer_oci_protocol("docker.io").await;
        assert_eq!(result, ClientProtocol::Https);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_docker_io_with_port() {
        let result = infer_oci_protocol("docker.io:443").await;
        assert_eq!(result, ClientProtocol::Https);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_apollo_registry() {
        let result = infer_oci_protocol("registry.apollographql.com").await;
        assert_eq!(result, ClientProtocol::Https);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_apollo_registry_with_port() {
        let result = infer_oci_protocol("registry.apollographql.com:443").await;
        assert_eq!(result, ClientProtocol::Https);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_custom_registry() {
        let result = infer_oci_protocol("localhost.example.com").await;
        assert_eq!(result, ClientProtocol::Https);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_port_only() {
        // This case will never pass the initial reference validation, but is
        // included here as a second layer of security.
        let result = infer_oci_protocol(":8080").await;
        assert_eq!(result, ClientProtocol::Https);
    }

    #[tokio::test]
    async fn test_infer_oci_protocol_empty_string() {
        // This case will never pass the initial reference validation, but is
        // included here as a second layer of security.
        let result = infer_oci_protocol("").await;
        assert_eq!(result, ClientProtocol::Https);
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
        let mock_server = MockServer::start().await;
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![schema_layer]).await;
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
        let mock_server = MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";
        let manifest_info = create_manifest_from_schema_layer("test schema");
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
            .mount(&mock_server)
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
            .mount(&mock_server)
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
            .mount(&mock_server)
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
        let mock_server = MockServer::start().await;
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![schema_layer]).await;

        // Create OciConfig with tag reference and hot-reload enabled
        let oci_config = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: image_reference.to_string(),
            hot_reload: true,
            poll_interval: Duration::from_millis(10),
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
        let mock_server = MockServer::start().await;
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };
        let image_reference = setup_mocks(mock_server, vec![schema_layer]).await;

        // Create OciConfig with tag reference and hot-reload disabled
        let oci_config = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: image_reference.to_string(),
            hot_reload: false,
            poll_interval: Duration::from_millis(10),
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
        let mock_server = MockServer::start().await;
        let schema_layer = ImageLayer {
            data: "test schema".to_string().into_bytes(),
            media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
            annotations: None,
        };

        // Create manifest first to get the digest
        let oci_manifest = create_manifest_from_schema_layer("test schema");
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
            .mount(&mock_server)
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
            .mount(&mock_server)
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
            .mount(&mock_server)
            .await;

        // Create digest reference
        let digest_ref = format!("{}/{graph_id}@{}", mock_server.address(), manifest_digest);

        // Create OciConfig with digest reference and hot-reload disabled
        let oci_config_digest = OciConfig {
            apollo_key: "test-api-key".to_string(),
            reference: digest_ref,
            hot_reload: false,
            poll_interval: Duration::from_millis(10),
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
        let mock_server = MockServer::start().await;
        let graph_id = "test-graph-id";
        let reference = "latest";
        let blob_request_count = Arc::new(AtomicUsize::new(0));

        let manifest_info1 = create_manifest_from_schema_layer("schema 1");
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
            .mount(&mock_server)
            .await;

        let manifest_info2 = create_manifest_from_schema_layer("schema 2");
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
            .mount(&mock_server)
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
            .mount(&mock_server)
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
            .mount(&mock_server)
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
}
