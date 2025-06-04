use std::time::Duration;

use async_compression::tokio::write::GzipEncoder;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use url::Url;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;

pub use crate::services::layers::persisted_queries::FullPersistedQueryOperationId;
pub use crate::services::layers::persisted_queries::ManifestOperation;
pub use crate::services::layers::persisted_queries::PersistedQueryManifest;
use crate::uplink::Endpoints;
use crate::uplink::UplinkConfig;

/// Get a query ID, body, and a PQ manifest with that ID and body.
pub fn fake_manifest() -> (String, String, PersistedQueryManifest) {
    let id = "1234".to_string();
    let body = r#"query { typename }"#.to_string();

    let manifest = PersistedQueryManifest::from(vec![ManifestOperation {
        id: id.clone(),
        body: body.clone(),
        client_name: None,
    }]);

    (id, body, manifest)
}

/// Mocks an uplink server with a persisted query list containing no operations.
pub async fn mock_empty_pq_uplink() -> (UplinkMockGuard, UplinkConfig) {
    mock_pq_uplink(&PersistedQueryManifest::default()).await
}

/// Mocks an uplink server with a persisted query list with a delay.
pub async fn mock_pq_uplink_with_delay(
    manifest: &PersistedQueryManifest,
    delay: Duration,
) -> (UplinkMockGuard, UplinkConfig) {
    let (guard, url) = mock_pq_uplink_one_endpoint(manifest, Some(delay)).await;
    (
        guard,
        UplinkConfig::for_tests(Endpoints::fallback(vec![url])),
    )
}

/// Mocks an uplink server with a persisted query list containing operations passed to this function.
pub async fn mock_pq_uplink(manifest: &PersistedQueryManifest) -> (UplinkMockGuard, UplinkConfig) {
    let (guard, url) = mock_pq_uplink_one_endpoint(manifest, None).await;
    (
        guard,
        UplinkConfig::for_tests(Endpoints::fallback(vec![url])),
    )
}

/// Guards for the uplink and GCS mock servers, dropping these structs shuts down the server.
pub struct UplinkMockGuard {
    _uplink_mock_guard: MockServer,
    _gcs_mock_guard: MockServer,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Operation {
    id: String,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    client_name: Option<String>,
}

/// Mocks an uplink server; returns a single Url rather than a full UplinkConfig, so you
/// can combine it with another one to test failover.
pub async fn mock_pq_uplink_one_endpoint(
    manifest: &PersistedQueryManifest,
    delay: Option<Duration>,
) -> (UplinkMockGuard, Url) {
    let operations: Vec<Operation> = manifest
        // clone the manifest so the caller can still make assertions about it
        .clone()
        .drain()
        .map(|(full_id, body)| Operation {
            id: full_id.operation_id,
            body,
            client_name: full_id.client_name,
        })
        .collect();

    let mock_gcs_server = MockServer::start().await;

    let body_json = serde_json::to_vec(&json!({
      "format": "apollo-persisted-query-manifest",
      "version": 1,
      "operations": operations
    }))
    .expect("Failed to convert into body.");
    let mut encoder = GzipEncoder::new(Vec::new());
    encoder.write_all(&body_json).await.unwrap();
    encoder.shutdown().await.unwrap();
    let compressed_body = encoder.into_inner();

    let gcs_response = ResponseTemplate::new(200)
        .set_body_raw(compressed_body, "application/octet-stream")
        .append_header("Content-Encoding", "gzip");

    Mock::given(method("GET"))
        .and(header("Accept-Encoding", "gzip"))
        .respond_with(gcs_response)
        .mount(&mock_gcs_server)
        .await;

    let mock_gcs_server_uri: Url = mock_gcs_server.uri().parse().unwrap();

    let mock_uplink_server = MockServer::start().await;

    let mut uplink_response = ResponseTemplate::new(200).set_body_json(json!({
          "data": {
            "persistedQueries": {
              "__typename": "PersistedQueriesResult",
              "id": "889406d7-b4f8-44df-a499-6c1e3c1bea09:1",
              "minDelaySeconds": 60,
              "chunks": [
                {
                  "id": "graph-id/889406a1-b4f8-44df-a499-6c1e3c1bea09/ec8ae3ae3eb00c738031dbe81603489b5d24fbf58f15bdeec1587282ee4e6eea",
                  "urls": [
                    "https://a.broken.gcs.url.that.will.get.fetched.and.skipped.unknown/",
                    mock_gcs_server_uri,
                  ]
                }
              ]
            }
          }
        }));

    if let Some(delay) = delay {
        uplink_response = uplink_response.set_delay(delay);
    }

    Mock::given(method("POST"))
        .respond_with(uplink_response)
        .mount(&mock_uplink_server)
        .await;

    let url = mock_uplink_server.uri().parse().unwrap();
    (
        UplinkMockGuard {
            _uplink_mock_guard: mock_uplink_server,
            _gcs_mock_guard: mock_gcs_server,
        },
        url,
    )
}

/// Mocks an uplink server which returns bad GCS URLs.
pub async fn mock_pq_uplink_bad_gcs() -> (MockServer, Url) {
    let mock_uplink_server = MockServer::start().await;

    let  uplink_response = ResponseTemplate::new(200).set_body_json(json!({
          "data": {
            "persistedQueries": {
              "__typename": "PersistedQueriesResult",
              "id": "889406d7-b4f8-44df-a499-6c1e3c1bea09:1",
              "minDelaySeconds": 60,
              "chunks": [
                {
                  "id": "graph-id/889406a1-b4f8-44df-a499-6c1e3c1bea09/ec8ae3ae3eb00c738031dbe81603489b5d24fbf58f15bdeec1587282ee4e6eea",
                  "urls": [
                    "https://definitely.not.gcs.unknown"
                  ]
                }
              ]
            }
          }
        }));

    Mock::given(method("POST"))
        .respond_with(uplink_response)
        .mount(&mock_uplink_server)
        .await;

    let url = mock_uplink_server.uri().parse().unwrap();
    (mock_uplink_server, url)
}
