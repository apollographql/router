use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use oci_client::client::ImageLayer;
use oci_client::manifest::IMAGE_MANIFEST_MEDIA_TYPE;
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use oci_client::manifest::OciDescriptor;
use oci_client::manifest::OciImageManifest;
use oci_client::manifest::OciManifest;
use regex::Regex;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use crate::integration::IntegrationTest;
use crate::integration::common::LICENSE_SIX_MONTHS_SECS;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;
use crate::integration::common::mint_license_jwt;
use crate::integration::common::mint_version_incompatible_license_jwt;

/// Helper function to create a query for the count field
fn query_count_field() -> Query {
    Query::new(
        None,
        None,
        Some(serde_json::json!({"query": "{ count }", "variables": {}})),
        None,
        HashMap::new(),
    )
}

const APOLLO_SCHEMA_MEDIA_TYPE: &str = "application/apollo.schema";
const ENTITLEMENT_MEDIA_TYPE: &str = "application/vnd.apollographql.entitlement.v1+jwt";
const APOLLO_MANIFEST_ENTITLEMENT_ID_ANNOTATION: &str = "com.apollograph.graph.entitlement.id";
const TEST_ENTITLEMENT_ID: &str = "test-entitlement-id";
const ARTIFACT_REFERENCE_404: &str =
    "localhost/testrepo@sha256:0000000000000000000000000000000000000000000000000000000000000000";
const MIN_CONFIG: &str = include_str!("fixtures/minimal-oci.router.yaml");
const MIN_CONFIG_WITH_METRICS: &str = include_str!("fixtures/minimal-oci-with-metrics.router.yaml");
const LOCAL_SCHEMA: &str = include_str!("../../../examples/graphql/local.graphql");

fn calculate_manifest_digest(manifest: &OciManifest) -> String {
    let manifest_bytes = serde_json::to_vec(manifest).unwrap();
    let hash = Sha256::digest(&manifest_bytes);
    format!("sha256:{:x}", hash)
}

/// Build an OCI image manifest with a single layer of the given media type —
/// the shape of both the graph manifest's schema layer and the entitlement
/// manifest's license layer. A media type other than `ENTITLEMENT_MEDIA_TYPE`
/// models a manifest with no entitlement layer (e.g. `OciError::LayerNotFound`).
fn build_single_layer_manifest(media_type: &str, digest: &str, size: usize) -> OciManifest {
    OciManifest::Image(OciImageManifest {
        schema_version: 2,
        media_type: Some(IMAGE_MANIFEST_MEDIA_TYPE.to_string()),
        config: Default::default(),
        layers: vec![OciDescriptor {
            media_type: media_type.to_string(),
            digest: digest.to_string(),
            size: size.try_into().unwrap(),
            urls: None,
            annotations: None,
            artifact_type: None,
        }],
        subject: None,
        artifact_type: None,
        annotations: None,
    })
}

/// Mount the graph@variant manifest (schema layer + entitlement id annotation)
/// on `mock_server`, along with its blob and the registry healthcheck endpoint.
/// Shared by every helper that needs a normal, successfully-resolvable graph
/// manifest — only the entitlement repository's behavior varies by caller.
/// Returns the digest-addressed artifact reference pointing at it.
async fn mount_graph_manifest(mock_server: &MockServer, schema_content: &str) -> String {
    let graph_id = "test-graph-id";

    let schema_layer = ImageLayer {
        data: schema_content.to_string().into(),
        media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
        annotations: None,
    };
    let blob_digest = schema_layer.sha256_digest();

    let mut manifest_annotations = BTreeMap::new();
    manifest_annotations.insert(
        APOLLO_MANIFEST_ENTITLEMENT_ID_ANNOTATION.to_string(),
        TEST_ENTITLEMENT_ID.to_string(),
    );
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
        annotations: Some(manifest_annotations),
    });
    let manifest_digest: String = calculate_manifest_digest(&oci_manifest);

    Mock::given(method("GET"))
        .and(path("/v2/"))
        .respond_with(ResponseTemplate::new(200).append_header("content-type", "application/json"))
        .mount(mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/v2/{}/blobs/{}", graph_id, blob_digest)))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(schema_layer.data.clone()),
        )
        .mount(mock_server)
        .await;

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
        .mount(mock_server)
        .await;

    format!("{}/{}@{}", mock_server.address(), graph_id, manifest_digest)
}

/// Helper function to set up mock subgraph servers
async fn setup_mock_subgraphs() -> (MockServer, HashMap<String, String>) {
    // Use port 0 to let the OS assign an available port
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
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

/// Helper function to set up a mock OCI registry server. The router now
/// resolves the license in two round trips: it reads the entitlement id
/// annotation off the graph@variant manifest, then fetches the entitlement's
/// own manifest (and license layer) from a separate `entitlements/{id}`
/// repository on the same registry. `license_jwt` is served verbatim as the
/// entitlement's license layer, so callers can pass a valid, expired, or
/// otherwise crafted JWT.
async fn setup_mock_oci_server_with_license(
    schema_content: &str,
    license_jwt: &str,
) -> (MockServer, String) {
    let mock_server = MockServer::start().await;
    let artifact_reference = mount_graph_manifest(&mock_server, schema_content).await;
    mount_entitlement_mocks(&mock_server, TEST_ENTITLEMENT_ID, license_jwt).await;
    (mock_server, artifact_reference)
}

/// Mount the entitlement's own manifest + license blob under the
/// `entitlements/{entitlement_id}` repository, tagged `latest` — the second
/// round trip `stream_license_from_oci` makes once it has read the entitlement id
/// off the graph manifest. `license_jwt` is served verbatim as the license
/// layer's content, so callers can pass a valid, expired, or malformed JWT.
async fn mount_entitlement_mocks(
    mock_server: &MockServer,
    entitlement_id: &str,
    license_jwt: &str,
) {
    let license_layer = ImageLayer {
        data: license_jwt.to_string().into(),
        media_type: ENTITLEMENT_MEDIA_TYPE.to_string(),
        annotations: None,
    };
    let license_blob_digest = license_layer.sha256_digest();

    let entitlement_manifest = build_single_layer_manifest(
        ENTITLEMENT_MEDIA_TYPE,
        &license_blob_digest,
        license_layer.data.len(),
    );

    let entitlement_repository = format!("entitlements/{entitlement_id}");

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/blobs/{}",
            entitlement_repository, license_blob_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(license_layer.data.clone()),
        )
        .mount(mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/manifests/latest",
            entitlement_repository
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                .set_body_bytes(serde_json::to_vec(&entitlement_manifest).unwrap()),
        )
        .mount(mock_server)
        .await;
}

/// Mount the entitlement's own manifest + blob(s) under
/// `entitlements/{TEST_ENTITLEMENT_ID}`, tagged `latest`, serving `initial`
/// until the caller arms `switch`, then serving `after`. Each state is a
/// `(media_type, bytes)` pair, so callers can model not just a bad JWT
/// recovering into a good one, but also a manifest with no entitlement layer
/// at all (an unrelated media type) transitioning to one that has it, or vice
/// versa.
async fn mount_entitlement_mocks_switching(
    mock_server: &MockServer,
    initial: (&str, Vec<u8>),
    after: (&str, Vec<u8>),
    switch: &Arc<AtomicBool>,
) {
    let (initial_media_type, initial_bytes) = initial;
    let (after_media_type, after_bytes) = after;

    let initial_layer = ImageLayer {
        data: initial_bytes.into(),
        media_type: initial_media_type.to_string(),
        annotations: None,
    };
    let after_layer = ImageLayer {
        data: after_bytes.into(),
        media_type: after_media_type.to_string(),
        annotations: None,
    };
    let initial_blob_digest = initial_layer.sha256_digest();
    let after_blob_digest = after_layer.sha256_digest();

    let entitlement_repository = format!("entitlements/{TEST_ENTITLEMENT_ID}");

    let initial_manifest = build_single_layer_manifest(
        initial_media_type,
        &initial_blob_digest,
        initial_layer.data.len(),
    );
    let after_manifest =
        build_single_layer_manifest(after_media_type, &after_blob_digest, after_layer.data.len());

    // Blobs are content-addressed, so both are mounted statically; only the
    // manifest needs to switch which digest (and media type) it points at.
    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/blobs/{}",
            entitlement_repository, initial_blob_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(initial_layer.data.clone()),
        )
        .mount(mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/blobs/{}",
            entitlement_repository, after_blob_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(after_layer.data.clone()),
        )
        .mount(mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/manifests/latest",
            entitlement_repository
        )))
        .respond_with({
            let switch = switch.clone();
            move |_req: &wiremock::Request| {
                let manifest = if switch.load(Ordering::SeqCst) {
                    &after_manifest
                } else {
                    &initial_manifest
                };
                ResponseTemplate::new(200)
                    .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                    .set_body_bytes(serde_json::to_vec(manifest).unwrap())
            }
        })
        .mount(mock_server)
        .await;
}

/// Helper function to set up a mock OCI registry server whose manifest carries
/// only a schema layer — no entitlement layer at all. This is the artifact
/// shape that a self-hosted, non-Apollo registry is expected to serve when it
/// hasn't opted into Graph Artifacts-delivered licensing: the router should
/// boot unlicensed rather than hang retrying a fetch that can never succeed.
async fn setup_mock_oci_server_no_entitlement(schema_content: &str) -> (MockServer, String) {
    let mock_server = MockServer::start().await;
    let graph_id = "test-graph-id";

    // Create schema layer
    let schema_layer = ImageLayer {
        data: schema_content.to_string().into(),
        media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
        annotations: None,
    };

    // Mock blob
    let blob_digest = schema_layer.sha256_digest();

    // Mock manifest — deliberately no entitlement layer
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

/// Helper function to set up a mock OCI registry server whose graph manifest
/// carries the entitlement id annotation (unlike `setup_mock_oci_server_no_entitlement`),
/// but whose entitlement repository's manifest exists with only an unrelated
/// layer — no `ENTITLEMENT_MEDIA_TYPE` layer at all. This is `OciError::LayerNotFound`
/// / `is_missing_entitlement_layer()`: a malformed publish that, unlike a 404,
/// won't fix itself on the next poll, so the router must boot unlicensed with
/// an explicit error log rather than hanging or silently retrying.
async fn setup_mock_oci_server_with_missing_entitlement_layer(
    schema_content: &str,
) -> (MockServer, String) {
    let mock_server = MockServer::start().await;
    let artifact_reference = mount_graph_manifest(&mock_server, schema_content).await;

    let unrelated_layer = ImageLayer {
        data: b"not an entitlement layer".to_vec().into(),
        media_type: "foo_bar".to_string(),
        annotations: None,
    };
    let blob_digest = unrelated_layer.sha256_digest();
    let entitlement_repository = format!("entitlements/{TEST_ENTITLEMENT_ID}");
    let entitlement_manifest =
        build_single_layer_manifest("foo_bar", &blob_digest, unrelated_layer.data.len());

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/blobs/{}",
            entitlement_repository, blob_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(unrelated_layer.data.clone()),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/manifests/latest",
            entitlement_repository
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                .set_body_bytes(serde_json::to_vec(&entitlement_manifest).unwrap()),
        )
        .mount(&mock_server)
        .await;

    (mock_server, artifact_reference)
}

/// Helper function to set up a mock OCI registry server whose graph manifest is
/// exactly like `setup_mock_oci_server`'s (static, with the entitlement id
/// annotation), but whose entitlement repository serves `initial_license_bytes`
/// (e.g. malformed or version-incompatible JWT content) at the `latest` tag
/// until the caller arms the returned switch, after which it serves a freshly
/// minted valid six-month license. Used to prove the router treats a bad
/// entitlement artifact as retryable rather than collapsing to unlicensed, and
/// recovers on its own once the artifact is corrected — without a restart.
async fn setup_mock_oci_server_with_recovering_entitlement(
    schema_content: &str,
    initial_license_bytes: Vec<u8>,
) -> (MockServer, String, Arc<AtomicBool>) {
    let mock_server = MockServer::start().await;
    let recovered = Arc::new(AtomicBool::new(false));
    let artifact_reference = mount_graph_manifest(&mock_server, schema_content).await;

    let recovered_jwt = mint_license_jwt(None, LICENSE_SIX_MONTHS_SECS, LICENSE_SIX_MONTHS_SECS);
    mount_entitlement_mocks_switching(
        &mock_server,
        (ENTITLEMENT_MEDIA_TYPE, initial_license_bytes),
        (ENTITLEMENT_MEDIA_TYPE, recovered_jwt.into_bytes()),
        &recovered,
    )
    .await;

    (mock_server, artifact_reference, recovered)
}

/// Mount the entitlement manifest fetch under `entitlements/{TEST_ENTITLEMENT_ID}`
/// so it returns `unavailable_status` until the caller arms the returned
/// switch, after which it serves a valid six-month license. Used for status
/// codes where the router must not collapse to unlicensed: a 403 is a
/// transient access error (`OciError::is_transient_not_found() == false`,
/// still retried), and a 404 is the "not yet backfilled" quiet-retry path
/// (`is_transient_not_found() == true`, and must not even log at warn level
/// or increment the failure metric — see ROUTER-2085).
///
/// Also returns a request counter for the unavailable response, so callers
/// can wait for a specific number of poll cycles (a condition, not a fixed
/// sleep) before asserting on behavior during the unavailable phase.
async fn mount_entitlement_mocks_unavailable_then_recovers(
    mock_server: &MockServer,
    unavailable_status: u16,
) -> (Arc<AtomicBool>, Arc<AtomicUsize>) {
    let authorized = Arc::new(AtomicBool::new(false));
    let unavailable_request_count = Arc::new(AtomicUsize::new(0));

    let license_layer = ImageLayer {
        data: mint_license_jwt(None, LICENSE_SIX_MONTHS_SECS, LICENSE_SIX_MONTHS_SECS).into(),
        media_type: ENTITLEMENT_MEDIA_TYPE.to_string(),
        annotations: None,
    };
    let license_blob_digest = license_layer.sha256_digest();
    let entitlement_repository = format!("entitlements/{TEST_ENTITLEMENT_ID}");
    let entitlement_manifest = build_single_layer_manifest(
        ENTITLEMENT_MEDIA_TYPE,
        &license_blob_digest,
        license_layer.data.len(),
    );

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/blobs/{}",
            entitlement_repository, license_blob_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(license_layer.data.clone()),
        )
        .mount(mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/manifests/latest",
            entitlement_repository
        )))
        .respond_with({
            let authorized = authorized.clone();
            let unavailable_request_count = unavailable_request_count.clone();
            move |_req: &wiremock::Request| {
                if authorized.load(Ordering::SeqCst) {
                    ResponseTemplate::new(200)
                        .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                        .set_body_bytes(serde_json::to_vec(&entitlement_manifest).unwrap())
                } else {
                    unavailable_request_count.fetch_add(1, Ordering::SeqCst);
                    ResponseTemplate::new(unavailable_status)
                }
            }
        })
        .mount(mock_server)
        .await;

    (authorized, unavailable_request_count)
}

/// Helper function to set up a mock OCI registry server whose graph manifest is
/// exactly like `setup_mock_oci_server`'s, but whose entitlement manifest fetch
/// returns 403 Forbidden until the caller arms the returned switch, after which
/// it serves a valid license. Used to prove a 403 is treated as a transient
/// error (see `OciError::is_transient_not_found`) — retried without collapsing
/// the router to unlicensed — and that the router recovers on its own once
/// access is restored.
async fn setup_mock_oci_server_with_unauthorized_entitlement(
    schema_content: &str,
) -> (MockServer, String, Arc<AtomicBool>) {
    let mock_server = MockServer::start().await;
    let artifact_reference = mount_graph_manifest(&mock_server, schema_content).await;
    let (authorized, _unavailable_request_count) =
        mount_entitlement_mocks_unavailable_then_recovers(&mock_server, 403).await;
    (mock_server, artifact_reference, authorized)
}

/// Helper function to set up a mock OCI registry server whose graph manifest is
/// exactly like `setup_mock_oci_server`'s, but whose entitlement manifest fetch
/// returns 404 until the caller arms the returned switch, after which it
/// serves a valid license. Used to prove the "not yet backfilled" path
/// (`OciError::is_transient_not_found() == true`) never logs above `debug` and
/// never touches the failure metric — unlike every other entitlement-fetch
/// failure — and that the router still boots once the entitlement appears.
async fn setup_mock_oci_server_with_not_yet_found_entitlement(
    schema_content: &str,
) -> (MockServer, String, Arc<AtomicBool>, Arc<AtomicUsize>) {
    let mock_server = MockServer::start().await;
    let artifact_reference = mount_graph_manifest(&mock_server, schema_content).await;
    let (found, not_found_request_count) =
        mount_entitlement_mocks_unavailable_then_recovers(&mock_server, 404).await;
    (
        mock_server,
        artifact_reference,
        found,
        not_found_request_count,
    )
}

/// Mount the entitlement manifest + blob under `entitlements/{TEST_ENTITLEMENT_ID}`
/// serving a valid six-month license until the caller arms the returned
/// switch, after which the manifest carries only an unrelated layer — the
/// same "missing entitlement layer" shape `setup_mock_oci_server_no_entitlement`'s
/// sibling test covers at startup, but reached here from an already-licensed
/// `Running` state. Used to prove `state_machine::accumulate_inputs`'s
/// "ignoring reload because of loss of license" guard: a router already
/// serving traffic under a good license must not be knocked back to
/// unlicensed by a later entitlement regression.
async fn setup_mock_oci_server_with_degrading_entitlement(
    schema_content: &str,
) -> (MockServer, String, Arc<AtomicBool>) {
    let mock_server = MockServer::start().await;
    let degraded = Arc::new(AtomicBool::new(false));
    let artifact_reference = mount_graph_manifest(&mock_server, schema_content).await;

    let valid_jwt = mint_license_jwt(None, LICENSE_SIX_MONTHS_SECS, LICENSE_SIX_MONTHS_SECS);
    mount_entitlement_mocks_switching(
        &mock_server,
        (ENTITLEMENT_MEDIA_TYPE, valid_jwt.into_bytes()),
        ("foo_bar", b"not an entitlement layer".to_vec()),
        &degraded,
    )
    .await;

    (mock_server, artifact_reference, degraded)
}

/// Mount the entitlement manifest + blob under `entitlements/{TEST_ENTITLEMENT_ID}`
/// serving a valid six-month license until the caller arms the returned
/// switch, after which the manifest fetch returns 403 Forbidden forever.
/// Unlike `setup_mock_oci_server_with_unauthorized_entitlement` (403 from the
/// very first poll, before the router has ever started and so before any
/// metrics pipeline exists to observe), this reaches the 403 only after the
/// router is already `Running` — needed to prove
/// `apollo.router.license.fetch.failure.total` actually increments, since
/// `/metrics` isn't reachable until the router's HTTP listener exists.
async fn setup_mock_oci_server_with_entitlement_then_unauthorized(
    schema_content: &str,
) -> (MockServer, String, Arc<AtomicBool>, Arc<AtomicUsize>) {
    let mock_server = MockServer::start().await;
    let degraded = Arc::new(AtomicBool::new(false));
    let unauthorized_request_count = Arc::new(AtomicUsize::new(0));
    let artifact_reference = mount_graph_manifest(&mock_server, schema_content).await;

    let valid_jwt = mint_license_jwt(None, LICENSE_SIX_MONTHS_SECS, LICENSE_SIX_MONTHS_SECS);
    let license_layer = ImageLayer {
        data: valid_jwt.into_bytes().into(),
        media_type: ENTITLEMENT_MEDIA_TYPE.to_string(),
        annotations: None,
    };
    let license_blob_digest = license_layer.sha256_digest();
    let entitlement_repository = format!("entitlements/{TEST_ENTITLEMENT_ID}");
    let entitlement_manifest = build_single_layer_manifest(
        ENTITLEMENT_MEDIA_TYPE,
        &license_blob_digest,
        license_layer.data.len(),
    );

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/blobs/{}",
            entitlement_repository, license_blob_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(license_layer.data.clone()),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/manifests/latest",
            entitlement_repository
        )))
        .respond_with({
            let degraded = degraded.clone();
            let unauthorized_request_count = unauthorized_request_count.clone();
            move |_req: &wiremock::Request| {
                if degraded.load(Ordering::SeqCst) {
                    unauthorized_request_count.fetch_add(1, Ordering::SeqCst);
                    ResponseTemplate::new(403)
                } else {
                    ResponseTemplate::new(200)
                        .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                        .set_body_bytes(serde_json::to_vec(&entitlement_manifest).unwrap())
                }
            }
        })
        .mount(&mock_server)
        .await;

    (
        mock_server,
        artifact_reference,
        degraded,
        unauthorized_request_count,
    )
}

/// Helper function to set up a mock OCI registry server with tag-based references.
/// The tag manifest endpoint (both HEAD and GET) serves the initial digest/manifest
/// until explicitly told otherwise by the caller.
///
/// Returns two caller-armed `Arc<AtomicBool>` switches rather than gating on a raw
/// request count: this same graph artifact reference is polled concurrently by
/// both the schema stream and the license stream, so "the Nth request" doesn't
/// deterministically correspond to either stream's Nth poll — whichever stream's
/// request lands on a given count is arbitrary. Both switches must therefore be
/// flipped by the caller only once it's confirmed (e.g. via `assert_started`) that
/// both streams have already completed an initial successful fetch:
///
/// - `serve_updated`: once armed via `.store(true, ..)`, the tag manifest switches
///   (atomically, for both HEAD and GET) from the initial digest/manifest to the
///   updated one.
/// - `enable_404`: once armed via `.store(true, ..)`, the tag manifest endpoint
///   returns 404 for every subsequent request.
async fn setup_mock_oci_server_with_tag(
    initial_schema: &str,
    updated_schema: &str,
) -> (MockServer, String, Arc<AtomicBool>, Arc<AtomicBool>) {
    let mock_server = MockServer::start().await;
    let graph_id = "test-repo";
    let tag = "latest";
    let enable_404 = Arc::new(AtomicBool::new(false));
    let serve_updated = Arc::new(AtomicBool::new(false));

    // Create initial schema layer
    let initial_schema_layer = ImageLayer {
        data: initial_schema.to_string().into(),
        media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
        annotations: None,
    };
    let initial_blob_digest = initial_schema_layer.sha256_digest();

    // Create updated schema layer
    let updated_schema_layer = ImageLayer {
        data: updated_schema.to_string().into(),
        media_type: APOLLO_SCHEMA_MEDIA_TYPE.to_string(),
        annotations: None,
    };
    let updated_blob_digest = updated_schema_layer.sha256_digest();

    // Both the initial and updated graph manifests carry the same entitlement
    // id annotation, so the router can reuse the same `entitlements/{id}`
    // mock repository (set up below) to fetch a license across a hot reload:
    // only the schema is expected to change here.
    let mut manifest_annotations = BTreeMap::new();
    manifest_annotations.insert(
        APOLLO_MANIFEST_ENTITLEMENT_ID_ANNOTATION.to_string(),
        TEST_ENTITLEMENT_ID.to_string(),
    );

    // Create initial manifest
    let initial_oci_manifest = OciManifest::Image(OciImageManifest {
        schema_version: 2,
        media_type: Some(IMAGE_MANIFEST_MEDIA_TYPE.to_string()),
        config: Default::default(),
        layers: vec![OciDescriptor {
            media_type: initial_schema_layer.media_type.clone(),
            digest: initial_blob_digest.clone(),
            size: initial_schema_layer.data.len().try_into().unwrap(),
            urls: None,
            annotations: None,
            artifact_type: None,
        }],
        subject: None,
        artifact_type: None,
        annotations: Some(manifest_annotations.clone()),
    });
    let initial_manifest_digest = calculate_manifest_digest(&initial_oci_manifest);

    // Create updated manifest
    let updated_oci_manifest = OciManifest::Image(OciImageManifest {
        schema_version: 2,
        media_type: Some(IMAGE_MANIFEST_MEDIA_TYPE.to_string()),
        config: Default::default(),
        layers: vec![OciDescriptor {
            media_type: updated_schema_layer.media_type.clone(),
            digest: updated_blob_digest.clone(),
            size: updated_schema_layer.data.len().try_into().unwrap(),
            urls: None,
            annotations: None,
            artifact_type: None,
        }],
        subject: None,
        artifact_type: None,
        annotations: Some(manifest_annotations),
    });
    let updated_manifest_digest = calculate_manifest_digest(&updated_oci_manifest);

    // Healthcheck
    Mock::given(method("GET"))
        .and(path("/v2/"))
        .respond_with(ResponseTemplate::new(200).append_header("content-type", "application/json"))
        .mount(&mock_server)
        .await;

    // Blob - initial
    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/blobs/{}",
            graph_id, initial_blob_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(initial_schema_layer.data.clone()),
        )
        .mount(&mock_server)
        .await;

    // Blob - updated
    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/blobs/{}",
            graph_id, updated_blob_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/octet-stream")
                .set_body_bytes(updated_schema_layer.data.clone()),
        )
        .mount(&mock_server)
        .await;

    // Entitlement's own manifest + license blob, shared by both the initial
    // and updated graph manifests via the entitlement id annotation.
    mount_entitlement_mocks(
        &mock_server,
        TEST_ENTITLEMENT_ID,
        &mint_license_jwt(None, LICENSE_SIX_MONTHS_SECS, LICENSE_SIX_MONTHS_SECS),
    )
    .await;

    // Manifest - initial
    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/manifests/{}",
            graph_id, initial_manifest_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                .append_header("docker-content-digest", initial_manifest_digest.clone())
                .set_body_bytes(serde_json::to_vec(&initial_oci_manifest).unwrap()),
        )
        .mount(&mock_server)
        .await;

    // Manifest - updated
    Mock::given(method("GET"))
        .and(path(format!(
            "/v2/{}/manifests/{}",
            graph_id, updated_manifest_digest
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                .append_header("docker-content-digest", updated_manifest_digest.clone())
                .set_body_bytes(serde_json::to_vec(&updated_oci_manifest).unwrap()),
        )
        .mount(&mock_server)
        .await;

    // Tag - HEAD returns the initial digest until `serve_updated` is armed by
    // the caller, then the updated digest; returns 404 once `enable_404` has
    // been armed by the caller.
    let tag_path = format!("/v2/{}/manifests/{}", graph_id, tag);
    Mock::given(method("HEAD"))
        .and(path(tag_path.clone()))
        .respond_with({
            let enable_404 = enable_404.clone();
            let serve_updated = serve_updated.clone();
            let initial_digest = initial_manifest_digest.clone();
            let updated_digest = updated_manifest_digest.clone();
            move |_req: &wiremock::Request| {
                if enable_404.load(Ordering::SeqCst) {
                    ResponseTemplate::new(404)
                } else if serve_updated.load(Ordering::SeqCst) {
                    ResponseTemplate::new(200)
                        .append_header("docker-content-digest", updated_digest.as_str())
                } else {
                    ResponseTemplate::new(200)
                        .append_header("docker-content-digest", initial_digest.as_str())
                }
            }
        })
        .mount(&mock_server)
        .await;

    // Tag - GET returns the initial manifest until `serve_updated` is armed
    // by the caller, then the updated manifest; returns 404 once
    // `enable_404` has been armed by the caller.
    Mock::given(method("GET"))
        .and(path(tag_path.clone()))
        .respond_with({
            let enable_404 = enable_404.clone();
            let serve_updated = serve_updated.clone();
            let initial_digest = initial_manifest_digest.clone();
            let updated_digest = updated_manifest_digest.clone();
            let initial_manifest_bytes =
                Arc::new(serde_json::to_vec(&initial_oci_manifest).unwrap());
            let updated_manifest_bytes =
                Arc::new(serde_json::to_vec(&updated_oci_manifest).unwrap());
            move |_req: &wiremock::Request| {
                if enable_404.load(Ordering::SeqCst) {
                    ResponseTemplate::new(404)
                } else if serve_updated.load(Ordering::SeqCst) {
                    ResponseTemplate::new(200)
                        .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                        .append_header("docker-content-digest", updated_digest.as_str())
                        .set_body_bytes(updated_manifest_bytes.as_ref().clone())
                } else {
                    ResponseTemplate::new(200)
                        .append_header("content-type", OCI_IMAGE_MEDIA_TYPE)
                        .append_header("docker-content-digest", initial_digest.as_str())
                        .set_body_bytes(initial_manifest_bytes.as_ref().clone())
                }
            }
        })
        .mount(&mock_server)
        .await;

    let artifact_reference = format!("{}/{}:{}", mock_server.address(), graph_id, tag);
    (mock_server, artifact_reference, enable_404, serve_updated)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_boots_with_oci_config() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference) = setup_mock_oci_server_with_license(
        LOCAL_SCHEMA,
        &mint_license_jwt(None, LICENSE_SIX_MONTHS_SECS, LICENSE_SIX_MONTHS_SECS),
    )
    .await;
    // Set up mock subgraph servers
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([(
            String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
            artifact_reference.into(),
        )]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// A graph artifact manifest with a schema layer but no entitlement layer
/// must not hang the router at startup. Therefore, OCI treats a missing
/// entitlement layer the same way Uplink does in its response: as
/// `License::default()` (unlicensed), not a fetch failure.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_boots_unlicensed_without_entitlement_layer() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference) =
        setup_mock_oci_server_no_entitlement(LOCAL_SCHEMA).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([(
            // A graph artifact reference in the config ensures that the
            // Router goes through OCI for its license
            String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
            artifact_reference.into(),
        )]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    // Bounded by `assert_started`'s own timeout: this must not hang
    router.assert_started().await;
    // Although the assertion above ensures that router did not hang on startup,
    // check explicitly that it is running unlicensed by confirming that the
    // router's state machine has logged this state transition
    if !router.log_contains("UpdateLicense(Unlicensed)") {
        router
            .wait_for_log_message("UpdateLicense(Unlicensed)")
            .await;
    }
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
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    router
        .wait_for_log_message("error fetching manifest digest from oci registry")
        .await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_tag_hot_reload() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let initial_schema = include_str!("fixtures/oci_initial_schema.graphql");
    let updated_schema = include_str!("fixtures/oci_updated_schema.graphql");

    let (_mock_server, artifact_reference, _enable_404, serve_updated) =
        setup_mock_oci_server_with_tag(initial_schema, updated_schema).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(true)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    // Only now arm the switch to the updated schema, i.e. once both the
    // schema stream and the license stream (which poll this same graph
    // artifact reference concurrently) have each already completed an
    // initial successful fetch. Arming this any earlier would race the two
    // streams' first polls against each other, since which stream's request
    // lands on a given HEAD/GET call is not deterministic.
    serve_updated.store(true, Ordering::SeqCst);

    // Wait for hot-reload, verify router can execute query
    router.assert_reloaded().await;

    router.execute_default_query().await;

    // Verify that the count field is no longer available after hot reload
    let (_trace_id, response) = router.execute_query(query_count_field()).await;
    let status = response.status();
    // GraphQL validation errors can return either 200 or 400
    assert!(
        status == 200 || status == 400,
        "Expected HTTP 200 or 400 for GraphQL validation error, got: {}",
        status
    );
    let graphql_response: apollo_router::graphql::Response = response
        .json()
        .await
        .expect("Failed to parse GraphQL response");
    assert!(
        !graphql_response.errors.is_empty(),
        "Expected query for count field to fail after hot reload"
    );
    assert!(
        graphql_response
            .errors
            .iter()
            .any(|e| e.message.contains("count") || e.message.contains("Cannot query field")),
        "Expected error message about count field, got: {:?}",
        graphql_response.errors
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_tag_hot_reload_no_change() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let initial_schema = include_str!("fixtures/oci_initial_schema.graphql");
    let updated_schema = include_str!("fixtures/oci_updated_schema.graphql");

    let (_mock_server, artifact_reference, _enable_404, _serve_updated) =
        setup_mock_oci_server_with_tag(initial_schema, updated_schema).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(true)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    // Wait for at least one poll cycle to complete
    // The router polls every 1 second (TEST_APOLLO_OCI_POLL_INTERVAL), so wait 2 seconds
    // to ensure at least one poll has completed. Since the tag doesn't change, no reload occurs.
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    router.execute_query(query_count_field()).await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_tag_404_after_first() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let initial_schema = include_str!("fixtures/oci_initial_schema.graphql");
    let updated_schema = include_str!("fixtures/oci_updated_schema.graphql");

    let (_mock_server, artifact_reference, enable_404, _serve_updated) =
        setup_mock_oci_server_with_tag(initial_schema, updated_schema).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(true)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    // Only now arm the 404, i.e. once both the schema stream and the license
    // stream (which poll this same graph artifact reference concurrently)
    // have each already completed an initial successful fetch. Arming this
    // any earlier would race the two streams' first polls against each
    // other over a shared request counter.
    enable_404.store(true, Ordering::SeqCst);

    // Wait for the next poll to return a 404, then verify a query against the old schema still works
    router
        .wait_for_log_message("error fetching manifest digest from oci registry")
        .await;
    router.execute_query(query_count_field()).await;
    router.graceful_shutdown().await;
    Ok(())
}

/// An entitlement whose JWT has already passed `haltAt` must still let the
/// router start: `haltAt` in the past resolves immediately to `LicensedHalt`
/// (see `license_stream::reset_checks_for_licenses`), which restricts
/// commercial features but does not prevent the router from serving traffic.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_boots_halted_with_expired_license() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let expired_jwt = mint_license_jwt(None, LICENSE_SIX_MONTHS_SECS, -60);
    let (_mock_server, artifact_reference) =
        setup_mock_oci_server_with_license(LOCAL_SCHEMA, &expired_jwt).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([(
            String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
            artifact_reference.into(),
        )]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    if !router.log_contains("UpdateLicense(LicensedHalt") {
        router
            .wait_for_log_message("UpdateLicense(LicensedHalt")
            .await;
    }
    // LicensedHalt is soft-enforced: unrestricted queries keep working.
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// A garbled (non-JWT) entitlement layer must not be treated as "no license":
/// `OciError::LicenseParse` with a decode failure that isn't version-incompatible
/// is a retryable error (see `oci_error_reason` / `record_license_fetch_failure`
/// in `router/event/license.rs`), so the router keeps polling instead of
/// collapsing to `Unlicensed`. Once the entitlement artifact is corrected, the
/// router picks up the valid license on its own, without a restart.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_recovers_after_invalid_license() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference, recovered) =
        setup_mock_oci_server_with_recovering_entitlement(
            LOCAL_SCHEMA,
            b"this-is-not-a-jwt".to_vec(),
        )
        .await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    // The router cannot reach `Running` until it has a license (see
    // `state_machine::attempt_reload`), so wait for the router to have
    // observed and retried the bad entitlement before arming the fix.
    router
        .wait_for_log_message("transient error fetching license from oci registry, will retry")
        .await;

    recovered.store(true, Ordering::SeqCst);

    router.assert_started().await;
    if !router.log_contains("UpdateLicense(Licensed") {
        router.wait_for_log_message("UpdateLicense(Licensed").await;
    }
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// A JWT missing a required claim (`haltAt`) is classified as
/// version-incompatible rather than generically invalid, but takes the same
/// retryable path as a malformed JWT: the router keeps polling instead of
/// treating the graph as unlicensed, and recovers once given a JWT this
/// router version understands.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_recovers_after_version_incompatible_license() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference, recovered) =
        setup_mock_oci_server_with_recovering_entitlement(
            LOCAL_SCHEMA,
            mint_version_incompatible_license_jwt().into_bytes(),
        )
        .await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    router
        .wait_for_log_message("transient error fetching license from oci registry, will retry")
        .await;

    recovered.store(true, Ordering::SeqCst);

    router.assert_started().await;
    if !router.log_contains("UpdateLicense(Licensed") {
        router.wait_for_log_message("UpdateLicense(Licensed").await;
    }
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// A 403 on the entitlement manifest fetch must be treated as transient (an
/// access problem or a blip), never as a signal that the graph has no license:
/// only a 404 means "unlicensed" (see `OciError::is_transient_not_found` and
/// ROUTER-2085). The router keeps polling and recovers on its own once access
/// is restored.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_recovers_after_unauthorized_entitlement_fetch() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference, authorized) =
        setup_mock_oci_server_with_unauthorized_entitlement(LOCAL_SCHEMA).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    router
        .wait_for_log_message("transient error fetching license from oci registry, will retry")
        .await;

    authorized.store(true, Ordering::SeqCst);

    router.assert_started().await;
    if !router.log_contains("UpdateLicense(Licensed") {
        router.wait_for_log_message("UpdateLicense(Licensed").await;
    }
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// A 404 on the entitlement manifest fetch — the "not yet backfilled" shape
/// expected during rollout, per `OciError::is_transient_not_found()` — must
/// behave differently from every *other* entitlement-fetch failure: it must
/// never log above `debug`, and it must never touch
/// `apollo.router.license.fetch.failure.total` (see ROUTER-2085 and
/// `router/event/license.rs`'s `Err(e)` arm in `LicenseSource::OCI`'s
/// `into_stream`). This is the "stay quiet" half of ROUTER-2145 item 2;
/// `test_router_oci_recovers_after_unauthorized_entitlement_fetch` above
/// proves the opposite (403 recorded, warned on) for comparison.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_entitlement_not_found_stays_quiet_and_recovers() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference, found, not_found_request_count) =
        setup_mock_oci_server_with_not_yet_found_entitlement(LOCAL_SCHEMA).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG_WITH_METRICS)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;

    // Wait for several 404 polls (a condition, not a fixed sleep) before
    // asserting on the quiet phase, so the assertions below cover more than
    // just the very first poll.
    let polled_enough = tokio::time::timeout(Duration::from_secs(10), async {
        while not_found_request_count.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        polled_enough.is_ok(),
        "expected at least 3 polls of the not-yet-found entitlement within timeout"
    );

    router.read_logs();
    assert!(
        !router.log_contains("transient error fetching license from oci registry, will retry"),
        "a 404 (not yet backfilled) must never be logged as a transient error"
    );
    assert!(
        !router.log_contains("UpdateLicense(Unlicensed)"),
        "a 404 (not yet backfilled) must never be treated as a revocation signal"
    );

    found.store(true, Ordering::SeqCst);

    // The router has no HTTP listener yet during the 404 phase (it can't
    // reach `Running` without a license — see `state_machine::attempt_reload`),
    // so `/metrics` isn't reachable until after it starts. The counter is
    // cumulative and neither the quiet 404s above nor a successful recovery
    // fetch increment it, so checking it here still proves the quiet phase
    // never touched it.
    router.assert_started().await;
    let metrics = router
        .get_metrics_response()
        .await
        .expect("metrics request should succeed")
        .text()
        .await
        .expect("metrics response should have a body");
    let oci_failure_counter =
        Regex::new(r#"(?m)^apollo_router_license_fetch_failure_total\{[^}]*source="oci"[^}]*\}"#)
            .expect("regex must be valid");
    assert!(
        !oci_failure_counter.is_match(&metrics),
        "a 404 (not yet backfilled) must never increment the oci license fetch failure counter, got:\n{metrics}"
    );

    if !router.log_contains("UpdateLicense(Licensed") {
        router.wait_for_log_message("UpdateLicense(Licensed").await;
    }
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// A manifest that exists under `entitlements/{id}` but carries no entitlement
/// JWT layer (`OciError::LayerNotFound` / `is_missing_entitlement_layer()`) is
/// malformed, not merely absent: unlike a 404, it won't fix itself on the next
/// poll. The router must still boot (unlicensed), logging an explicit error —
/// distinct from both the "missing annotation" case
/// (`test_router_oci_boots_unlicensed_without_entitlement_layer`) and the
/// quiet 404 case above.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_boots_unlicensed_with_missing_entitlement_layer() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference) =
        setup_mock_oci_server_with_missing_entitlement_layer(LOCAL_SCHEMA).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([(
            String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
            artifact_reference.into(),
        )]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    // Bounded by `assert_started`'s own timeout: this must not hang.
    router.assert_started().await;
    if !router.log_contains("UpdateLicense(Unlicensed)") {
        router
            .wait_for_log_message("UpdateLicense(Unlicensed)")
            .await;
    }
    if !router.log_contains("APOLLO_ROUTER_LICENSE_INVALID") {
        router
            .wait_for_log_message("APOLLO_ROUTER_LICENSE_INVALID")
            .await;
    }
    assert!(
        router.log_contains("the router will run unlicensed"),
        "expected the missing-entitlement-layer error to explain the router will run unlicensed"
    );
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// A router that is already `Running` under a good license must not be
/// knocked back to unlicensed by a later entitlement regression —
/// `state_machine::accumulate_inputs` has an explicit guard for exactly this:
///
/// ```ignore
/// if self.is_licensed() && new_license.as_deref().is_some_and(|l| l.is_unlicensed()) {
///     tracing::info!(event = STATE_CHANGE, "ignoring reload because of loss of license");
///     return self;
/// }
/// ```
///
/// This is the resiliency half of ROUTER-2145 item 1: every other test in
/// this module proves "start broken, then recover"; this one proves the
/// reverse direction, "start good, then degrade," is ignored rather than
/// applied.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_ignores_license_loss_while_running() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference, degraded) =
        setup_mock_oci_server_with_degrading_entitlement(LOCAL_SCHEMA).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    if !router.log_contains("UpdateLicense(Licensed") {
        router.wait_for_log_message("UpdateLicense(Licensed").await;
    }
    router.execute_default_query().await;

    // Only now arm the degrade, i.e. once the router has already committed a
    // good license. Arming any earlier would race the license stream's first
    // poll.
    degraded.store(true, Ordering::SeqCst);

    router
        .wait_for_log_message("ignoring reload because of loss of license")
        .await;

    // The router must still be serving under the original, good license:
    // no second "GraphQL endpoint exposed" (a fresh Running transition) and
    // queries keep working.
    let started_count = router
        .logs()
        .iter()
        .filter(|line| line.contains("GraphQL endpoint exposed"))
        .count();
    assert_eq!(
        started_count, 1,
        "the router must not have re-entered Running after the ignored degrade"
    );
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// `record_license_fetch_failure` emits both a `tracing::warn!` and an
/// `apollo.router.license.fetch.failure.total` increment from the same call
/// site, but every other test in this module only verifies the log side.
/// This asserts on the counter directly (ROUTER-2145 item 4), scraping
/// `/metrics` after the router is already `Running` under a good license and
/// degrades to repeated 403s — `/metrics` isn't reachable during `Startup`
/// (the router has no HTTP listener until `attempt_reload` succeeds), so a
/// failure that only ever happens pre-license (as in
/// `test_router_oci_recovers_after_unauthorized_entitlement_fetch`) can't be
/// observed this way; recording it from an already-`Running` state sidesteps
/// that.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_oci_records_metric_for_license_fetch_failures() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let (_mock_server, artifact_reference, degraded, unauthorized_request_count) =
        setup_mock_oci_server_with_entitlement_then_unauthorized(LOCAL_SCHEMA).await;
    let (_subgraphs_server, subgraph_overrides) = setup_mock_subgraphs().await;

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG_WITH_METRICS)
        .env(HashMap::from([
            (
                String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
                OsString::from(artifact_reference),
            ),
            (
                String::from("TEST_APOLLO_OCI_POLL_INTERVAL"),
                OsString::from("1"),
            ),
        ]))
        .subgraph_overrides(subgraph_overrides)
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    if !router.log_contains("UpdateLicense(Licensed") {
        router.wait_for_log_message("UpdateLicense(Licensed").await;
    }
    router.execute_default_query().await;

    // Only now arm the degrade, i.e. once the router has already committed a
    // good license and its metrics pipeline is live.
    degraded.store(true, Ordering::SeqCst);

    router
        .wait_for_log_message("transient error fetching license from oci registry, will retry")
        .await;

    // Wait for a second 403 poll (a condition, not a fixed sleep) so the
    // counter reflects more than just the first failure.
    let polled_enough = tokio::time::timeout(Duration::from_secs(10), async {
        while unauthorized_request_count.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        polled_enough.is_ok(),
        "expected at least 2 polls of the degraded entitlement within timeout"
    );

    let metrics = router
        .get_metrics_response()
        .await
        .expect("metrics request should succeed")
        .text()
        .await
        .expect("metrics response should have a body");
    // The Rust `regex` crate doesn't support look-around, so match both label
    // orderings explicitly rather than asserting each is present independently.
    let failure_counter = Regex::new(
        r#"(?m)^apollo_router_license_fetch_failure_total\{(?:reason="http_error",source="oci"|source="oci",reason="http_error")[^}]*\}\s+(\d+)"#,
    )
    .expect("regex must be valid");
    let captures = failure_counter.captures(&metrics).unwrap_or_else(|| {
        panic!(
            "expected apollo_router_license_fetch_failure_total{{reason=\"http_error\",source=\"oci\"}} in /metrics, got:\n{metrics}"
        )
    });
    let failure_count: u64 = captures[1].parse().expect("counter value must be a number");
    assert!(
        failure_count >= 2,
        "expected at least two recorded oci/http_error license fetch failures, got {failure_count}"
    );

    // The router must still be serving under the original, good license.
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

/// An Apollo-hosted graph artifact reference and an explicit license
/// (`APOLLO_ROUTER_LICENSE`) are both fully-specified license sources, and an
/// Apollo-hosted artifact is expected to carry its own entitlement layer —
/// so configuring both is a contradiction, not a precedence question
/// (`Opt::license_source` in `executable.rs`, the ROUTER-2087 fail-fast
/// check). `executable.rs`'s `license_source_tests` module already proves
/// this at the `Opt` level; this proves it through the real CLI/env surface
/// a router process actually sees (ROUTER-2145 item 3), mirroring
/// `lifecycle::test_invalid_config`'s pattern for a different class of bad
/// config.
///
/// No OCI mock server is needed: `license_source()` is called with `?`
/// before `RouterHttpServer` is ever built, so the router exits before any
/// network I/O — schema and license sources are both just plain values built
/// synchronously at this point, not yet-polling streams.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_rejects_apollo_hosted_graph_artifact_reference_with_explicit_license()
-> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .config(MIN_CONFIG)
        .jwt("not-a-real-license")
        .env(HashMap::from([(
            String::from("APOLLO_GRAPH_ARTIFACT_REFERENCE"),
            OsString::from(
                "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
            ),
        )]))
        .hot_reload(false)
        .build()
        .await;

    router.start().await;
    // `license_source()`'s error is a top-level `anyhow::Error` returned from
    // `main()`, printed via `eprintln!` in `main.rs` rather than through the
    // tracing logger — so it lands on stderr, not stdout.
    router
        .wait_for_stderr_message(
            "APOLLO_ROUTER_LICENSE and --graph-artifact-reference cannot be used together",
        )
        .await;
    router.assert_shutdown().await;
    Ok(())
}
