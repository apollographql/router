use std::time::Duration;

use insta::assert_yaml_snapshot;
use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::common::graph_os_enabled;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::IntegrationTest;

const PROMETHEUS_CONFIG: &str = r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
"#;

#[tokio::test(flavor = "multi_thread")]
async fn test_jwks_timeout() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(format!(
            r#"
            {PROMETHEUS_CONFIG}
            authentication:
                router:
                    jwt:
                        jwks:
                            - url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
                            timeout: 1ns
            "#
        ))
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_not_started().await;

    router.graceful_shutdown().await;
    Ok(())
}
