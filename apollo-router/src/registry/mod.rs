use std::string::FromUtf8Error;
use docker_credential::CredentialRetrievalError;
use docker_credential::DockerCredential;
use oci_client::Client as ociClient;
use oci_client::Reference;
use oci_client::errors::OciDistributionError;
use oci_client::secrets::RegistryAuth;
use thiserror::Error;

/// Configuration for fetching an OCI Bundle
/// This struct does not change on router reloads - they are all sourced from CLI options.
#[derive(Debug, Clone)]
pub struct OCIConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: String,

    // Should this be a reference instead?
    pub url: String,
}

pub(crate) struct OCIResult {
    pub schema: String
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("OCI layer has not title")]
    OCILayerMissingTitle,
    #[error("Oci Distribution error: {0}")]
    OCIDistributionError(OciDistributionError),
    #[error("Oci Parsing error: {0}")]
    OCIParseError(oci_client::ParseError),
    #[error("Unable to parse layer: {0}")]
    OCILayerParseError(FromUtf8Error),
}

impl From<oci_client::ParseError> for Error {
    fn from(value: oci_client::ParseError) -> Self {
        Error::OCIParseError(value)
    }
}

impl From<OciDistributionError> for Error {
    fn from(value: OciDistributionError) -> Self {
        Error::OCIDistributionError(value)
    }
}

impl From<FromUtf8Error> for Error {
    fn from(value: FromUtf8Error) -> Self {
        Error::OCILayerParseError(value)
    }
}

fn build_auth(reference: &Reference, apollo_key: &String) -> RegistryAuth {
    let server = reference
        .resolve_registry()
        .strip_suffix('/')
        .unwrap_or_else(|| reference.resolve_registry());

    // Check if the server registry ends with apollographql.com
    if server.ends_with("apollographql.com") {
        tracing::debug!("Using Apollo registry authentication");
        return RegistryAuth::Basic("apollo_registry".to_string(), apollo_key.clone());
    }

    match docker_credential::get_credential(server) {
        Err(CredentialRetrievalError::ConfigNotFound) => RegistryAuth::Anonymous,
        Err(CredentialRetrievalError::NoCredentialConfigured) => RegistryAuth::Anonymous,
        Err(e) => {
            tracing::warn!("Error handling docker configuration file: {}", e);
            RegistryAuth::Anonymous
        }
        Ok(DockerCredential::UsernamePassword(username, password)) => {
            tracing::debug!("Found docker credentials");
            RegistryAuth::Basic(username, password)
        }
        Ok(DockerCredential::IdentityToken(_)) => {
            tracing::warn!(
                "Cannot use contents of docker config, identity token not supported. Using anonymous auth"
            );
            RegistryAuth::Anonymous
        }
    }
}

async fn pull_oci(
    client: &mut ociClient,
    auth: &RegistryAuth,
    reference: &Reference,
) -> Result<OCIResult, Error> {
    tracing::info!(?reference, "pulling oci bundle");
    let supported_types = vec![
        "application/json",
        "text/plain",
        "application/vnd.oci.empty.v1+json"
    ];
    let image = client
        .pull(reference, auth, supported_types.clone())
        .await?;

    let schema = image
        .layers
        .iter()
        .find(|layer| layer.media_type == "application/apollo.schema")
        .ok_or(Error::OCILayerMissingTitle)?
        .clone();

    Ok(OCIResult {
        schema: String::from_utf8(schema.data)?,
    })
}

/// Fetch an OCI bundle
pub(crate) async fn fetch_oci(oci_config: OCIConfig) -> Result<OCIResult, Error> {
    let reference: Reference = oci_config.url.as_str().parse()?;
    let client_config = oci_client::client::ClientConfig {
        protocol: oci_client::client::ClientProtocol::Https,
        ..Default::default()
    };

    let mut client = ociClient::new(client_config);

    let auth = build_auth(&reference, &oci_config.apollo_key);

    // let output = cli.output.unwrap_or("/tmp".to_string());
    pull_oci(&mut client, &auth, &reference).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_auth_apollo_registry() {
        // Create a reference for an Apollo registry
        let reference: Reference = "registry.apollographql.com/my-graph:latest".parse().unwrap();
        let apollo_key = "test-api-key".to_string();

        // Call build_auth
        let auth = build_auth(&reference, &apollo_key);

        // Check that it returns the correct RegistryAuth
        match auth {
            RegistryAuth::Basic(username, password) => {
                assert_eq!(username, "apollo_registry");
                assert_eq!(password, apollo_key);
            }
            _ => panic!("Expected RegistryAuth::Basic, got something else"),
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
        match auth {
            RegistryAuth::Basic(username, _) => {
                assert_ne!(username, "apollo_registry");
            }
            _ => {} // Any other type is fine for this test
        }
    }
}
