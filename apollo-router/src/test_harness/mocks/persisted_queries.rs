use std::collections::HashMap;
use std::time::Duration;

use maplit::hashmap;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use url::Url;
use wiremock::matchers::method;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

use crate::uplink::Endpoints;
use crate::uplink::UplinkConfig;

/// Get a query ID, body, and a PQ manifest with that ID and body.
pub fn fake_manifest() -> (String, String, HashMap<String, String>) {
    let id = "1234".to_string();
    let body = r#"query { typename }"#.to_string();
    let manifest = hashmap! { id.to_string() => body.to_string() };
    (id, body, manifest)
}

/// Mocks an uplink server with a persisted query list containing no operations.
pub async fn mock_empty_pq_uplink() -> (UplinkMockGuard, UplinkConfig) {
    mock_pq_uplink(&HashMap::new()).await
}

/// Mocks an uplink server with a persisted query list with a delay.
pub async fn mock_pq_uplink_with_delay(
    manifest: &HashMap<String, String>,
    delay: Duration,
) -> (UplinkMockGuard, UplinkConfig) {
    let (guard, url) = mock_pq_uplink_one_endpoint(manifest, Some(delay)).await;
    (
        guard,
        UplinkConfig::for_tests(Endpoints::fallback(vec![url])),
    )
}

/// Mocks an uplink server with a persisted query list containing operations passed to this function.
pub async fn mock_pq_uplink(manifest: &HashMap<String, String>) -> (UplinkMockGuard, UplinkConfig) {
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
struct Operation {
    id: String,
    body: String,
}

/// Mocks an uplink server; returns a single Url rather than a full UplinkConfig, so you
/// can combine it with another one to test failover.
pub async fn mock_pq_uplink_one_endpoint(
    manifest: &HashMap<String, String>,
    delay: Option<Duration>,
) -> (UplinkMockGuard, Url) {
    let operations: Vec<Operation> = manifest
        // clone the manifest so the caller can still make assertions about it
        .clone()
        .drain()
        .map(|(id, body)| Operation { id, body })
        .collect();

    let mock_gcs_server = MockServer::start().await;

    let gcs_response = ResponseTemplate::new(200).set_body_json(json!({
      "format": "apollo-persisted-query-manifest",
      "version": 1,
      "operations": operations
    }));

    Mock::given(method("GET"))
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
