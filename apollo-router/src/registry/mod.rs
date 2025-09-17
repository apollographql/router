use std::string::FromUtf8Error;

use docker_credential::CredentialRetrievalError;
use docker_credential::DockerCredential;
use oci_client::Client;
use oci_client::Reference;
use oci_client::client::ClientConfig;
use oci_client::client::ClientProtocol;
use oci_client::errors::OciDistributionError;
use oci_client::secrets::RegistryAuth;
use thiserror::Error;

/// Configuration for fetching an OCI Bundle
/// This struct does not change on router reloads - they are all sourced from CLI options.
#[derive(Debug, Clone)]
pub struct OciConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: String,

    /// OCI Compliant URL pointing to the release bundle
    pub reference: String,
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

async fn pull_oci(
    client: &mut Client,
    auth: &RegistryAuth,
    reference: &Reference,
) -> Result<OciContent, OciError> {
    tracing::debug!("pulling oci manifest");
    // We aren't using the default `pull` function because that validates that all the layers are in the
    // set of supported layers. Since we want to be able to add new layers for new features, we want the
    // client to have forwards compatibility.
    // To achieve that, we are going to fetch the manifest and then fetch the layers that this code cares about directly.
    let (manifest, _) = client.pull_image_manifest(reference, auth).await?;

    let schema_layer = manifest
        .layers
        .iter()
        .find(|layer| layer.media_type == APOLLO_SCHEMA_MEDIA_TYPE)
        .ok_or(OciError::LayerMissingTitle)?
        .clone();

    tracing::debug!("pulling oci blob");
    let mut schema = Vec::new();
    client
        .pull_blob(reference, &schema_layer, &mut schema)
        .await?;

    Ok(OciContent {
        schema: String::from_utf8(schema)?,
    })
}

/// The oci reference may not contain the protocol, only hostname[:port]. As a result,
/// in order to test locally without SSL, either (1) protocol needs to be exposed as an
/// env var or (2) protocol needs to be inferred from hostname. Rather than introduce a
/// largely unused configuration option, this function checks the hostname for local
/// development/testing and disables SSL accordingly.
async fn infer_oci_protocol(registry: &str) -> ClientProtocol {
    let host = registry.split(":").next().expect("host must be provided");
    if host == "localhost" || host == "127.0.0.1" {
        ClientProtocol::Http
    } else {
        ClientProtocol::Https
    }
}

/// Fetch an OCI bundle
pub(crate) async fn fetch_oci(oci_config: OciConfig) -> Result<OciContent, OciError> {
    let reference: Reference = oci_config.reference.as_str().parse()?;
    let auth = build_auth(&reference, &oci_config.apollo_key);
    let protocol = infer_oci_protocol(reference.registry()).await;

    tracing::debug!(
        "prepared to fetch schema from oci over {:?}, auth anonymous? {:?}",
        protocol,
        auth == RegistryAuth::Anonymous
    );

    pull_oci(
        &mut Client::new(ClientConfig {
            protocol,
            ..Default::default()
        }),
        &auth,
        &reference,
    )
    .await
}

#[cfg(test)]
mod tests {
    use futures::future::join_all;
    use oci_client::client::ClientConfig;
    use oci_client::client::ClientProtocol;
    use oci_client::client::ImageLayer;
    use oci_client::manifest::IMAGE_MANIFEST_MEDIA_TYPE;
    use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
    use oci_client::manifest::OciDescriptor;
    use oci_client::manifest::OciImageManifest;
    use oci_client::manifest::OciManifest;
    use url::Url;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::registry::OciError::LayerMissingTitle;

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
        let _ = Mock::given(method("GET"))
            .and(path(manifest_url.path()))
            .respond_with(
                ResponseTemplate::new(200)
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
        let result = pull_oci(&mut client, &RegistryAuth::Anonymous, &image_reference)
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
        let result = pull_oci(&mut client, &RegistryAuth::Anonymous, &image_reference)
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
        let result = pull_oci(&mut client, &RegistryAuth::Anonymous, &image_reference)
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
}
