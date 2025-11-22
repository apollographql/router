use std::collections::HashMap;
use std::net::SocketAddr;
use std::net::TcpListener;

use oci_client::client::ImageLayer;
use oci_client::manifest::IMAGE_MANIFEST_MEDIA_TYPE;
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use oci_client::manifest::OciDescriptor;
use oci_client::manifest::OciImageManifest;
use oci_client::manifest::OciManifest;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use crate::integration::IntegrationTest;
use crate::integration::common::graph_os_enabled;

const APOLLO_SCHEMA_MEDIA_TYPE: &str = "application/apollo.schema";
const ARTIFACT_REFERENCE_404: &str = "artifact.api.apollographql.com/test@sha256:0000000000000000000000000000000000000000000000000000000000000000";
const MIN_CONFIG: &str = include_str!("fixtures/minimal-oci.router.yaml");
const LOCAL_SCHEMA: &str = include_str!("../../../examples/graphql/local.graphql");

fn calculate_manifest_digest(manifest: &OciManifest) -> String {
    let manifest_bytes = serde_json::to_vec(manifest).unwrap();
    let hash = Sha256::digest(&manifest_bytes);
    format!("sha256:{:x}", hash)
}

/// Helper function to set up mock subgraph servers
async fn setup_mock_subgraphs() -> (MockServer, HashMap<String, String>) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 8900))).unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}/");

    let subgraphs_server = wiremock::MockServer::builder()
        .listener(listener)
        .start()
        .await;

    // Set up basic GraphQL responses for all subgraphs
    let basic_response = serde_json::json!({
        "data": {
            "__typename": "Query"
        }
    });

    // Mock GraphQL introspection and basic queries for all subgraphs
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/json")
                .set_body_json(&basic_response),
        )
        .mount(&subgraphs_server)
        .await;

    // Create subgraph overrides for all subgraphs in the local.graphql schema
    let mut subgraph_overrides = HashMap::new();
    subgraph_overrides.insert("accounts".to_string(), url.clone());
    subgraph_overrides.insert("inventory".to_string(), url.clone());
    subgraph_overrides.insert("products".to_string(), url.clone());
    subgraph_overrides.insert("reviews".to_string(), url.clone());

    (subgraphs_server, subgraph_overrides)
}

/// Helper function to set up a mock OCI registry server
async fn setup_mock_oci_server(schema_content: &str) -> (MockServer, String) {
    let mock_server = MockServer::start().await;
    let graph_id = "test-graph-id";

    // Create schema layer
    let schema_layer = ImageLayer {
        data: schema_content.to_string().into_bytes(),
        media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
        annotations: None,
    };

    // Mock blob
    let blob_digest = schema_layer.sha256_digest();

    // Mock manifest
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
    let manifest_digest: String = calculate_manifest_digest(&oci_manifest);

    // Set up check endpoint
    Mock::given(method("GET"))
        .and(path("/v2/"))
        .respond_with(ResponseTemplate::new(200).append_header("content-type", "application/json"))
        .mount(&mock_server)
        .await;

    // Set up blob endpoint
    Mock::given(method("GET"))
        .and(path(format!("/v2/{}/blobs/{}", graph_id, blob_digest)))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(schema_layer.data.clone()),
        )
        .mount(&mock_server)
        .await;

    // Set up manifest endpoint
    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/manifests/{}",
            graph_id, manifest_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                .set_body_bytes(serde_json::to_vec(&oci_manifest).unwrap()),
        )
        .mount(&mock_server)
        .await;

    let artifact_reference = format!("{}/{}@{}", mock_server.address(), graph_id, manifest_digest);
    (mock_server, artifact_reference)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_boots_with_oci_config() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference) = setup_mock_oci_server(LOCAL_SCHEMA).await;
    // Set up mock subgraph servers
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([(
            String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
            artifact_reference.into(),
        )]))
        .subgraph_overrides(subgraph_overrides)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_cannot_fetch_schema() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([(
            String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
            ARTIFACT_REFERENCE_404.into(),
        )]))
        .build()
        .await;

    router.start().await;
    router
        .wait_for_log_message("no valid schema was supplied")
        .await;
    Ok(())
}
