use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::router;

#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
struct RequestDecompressionConfig {
    /// When `true`, brotli-encoded request bodies will be automatically decompressed.
    ///
    /// Defaults to `true`.
    br: Option<bool>,
    /// When `true`, gzip-encoded request bodies will be automatically decompressed.
    ///
    /// Defaults to `true`.
    gzip: Option<bool>,
    /// When `true`, deflate-encoded request bodies will be automatically decompressed.
    ///
    /// Defaults to `true`.
    deflate: Option<bool>,
}
struct RequestDecompressionPlugin {
    br: bool,
    gzip: bool,
    deflate: bool,
}

#[async_trait::async_trait]
impl PluginPrivate for RequestDecompressionPlugin {
    type Config = RequestDecompressionConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, tower::BoxError> {
        Ok(RequestDecompressionPlugin {
            br: init.config.br.unwrap_or_default(),
            gzip: init.config.gzip.unwrap_or_default(),
            deflate: init.config.deflate.unwrap_or_default(),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        // RequestDecompressionLayer works on `http` types so we have to perform the conversions to
        // make it compatible with `BoxService`.
        ServiceBuilder::new()
            .map_request(http::Request::from)
            .map_response(router::Response::from)
            .layer(
                tower_http::decompression::RequestDecompressionLayer::new()
                    .br(self.br)
                    .gzip(self.gzip)
                    .deflate(self.deflate),
            )
            .map_request(router::Request::from)
            .map_response(http::Response::from)
            .service(service)
            .boxed()
    }
}

register_private_plugin!(
    "apollo",
    "request_decompression",
    RequestDecompressionPlugin
);

#[cfg(test)]
mod test {
    use std::io::Read;
    use std::io::Write;

    use flate2::read::DeflateEncoder;
    use flate2::read::GzEncoder;
    use http::header;
    use http_body_util::BodyExt;

    use super::RequestDecompressionConfig;
    use super::RequestDecompressionPlugin;
    use crate::plugin::PluginInit;
    use crate::plugin::PluginPrivate;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::router;

    #[tokio::test]
    async fn test_config() {
        let config = serde_yaml::from_str::<RequestDecompressionConfig>(
            r#"
        request_decompression:
          br: false
          gzip: false
          deflate: false
        "#,
        )
        .unwrap();

        let plugin =
            RequestDecompressionPlugin::new(PluginInit::fake_builder().config(config).build())
                .await
                .unwrap();

        assert!(!plugin.br);
        assert!(!plugin.gzip);
        assert!(!plugin.deflate);
    }

    const ORIGINAL_BODY: &[u8] = b"Hello, world!";

    async fn test_request_decompression_success(req: router::Request) {
        let config = r#"
        request_decompression:
          br: true
          gzip: true
          deflate: true
        "#;
        let test_harness: PluginTestHarness<RequestDecompressionPlugin> =
            PluginTestHarness::builder().config(config).build().await;

        let service = test_harness.router_service(|mut req| async move {
            let body = req
                .router_request
                .body_mut()
                .collect()
                .await
                .expect("request decompression succeeds");
            let body = body.to_bytes().to_vec();
            assert_eq!(body, ORIGINAL_BODY);

            Ok(router::Response::fake_builder().build().unwrap())
        });

        let _response = service.call(req).await.unwrap();
    }

    #[tokio::test]
    async fn test_gzip_request_decompression_success() {
        let compressed_body = {
            let mut d = GzEncoder::new(ORIGINAL_BODY, flate2::Compression::default());
            let mut s = Vec::new();
            d.read_to_end(&mut s).expect("writing to buffer succeeds");
            s
        };

        test_request_decompression_success(
            router::Request::fake_builder()
                .header(header::CONTENT_ENCODING, "gzip")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(compressed_body)
                .build()
                .unwrap(),
        )
        .await;
    }

    #[tokio::test]
    async fn test_deflate_request_decompression_success() {
        let compressed_body = {
            let mut d = DeflateEncoder::new(ORIGINAL_BODY, flate2::Compression::default());
            let mut s = Vec::new();
            d.read_to_end(&mut s).expect("writing to buffer succeeds");
            s
        };

        test_request_decompression_success(
            router::Request::fake_builder()
                .header(header::CONTENT_ENCODING, "deflate")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(compressed_body)
                .build()
                .unwrap(),
        )
        .await;
    }

    #[tokio::test]
    async fn test_brotli_request_decompression_success() {
        let compressed_body = {
            let s = Vec::new();
            let mut d = brotli::CompressorWriter::new(s, 4096, 11, 22);
            d.write_all(ORIGINAL_BODY)
                .expect("writing to buffer succeeds");
            d.flush().expect("flushing buffer succeeds");
            d.into_inner()
        };

        test_request_decompression_success(
            router::Request::fake_builder()
                .header(header::CONTENT_ENCODING, "br")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(compressed_body)
                .build()
                .unwrap(),
        )
        .await;
    }

    async fn test_request_decompression_failure(req: router::Request, expected_error: String) {
        let config = r#"
        request_decompression:
          br: true
          gzip: true
          deflate: true
        "#;
        let test_harness: PluginTestHarness<RequestDecompressionPlugin> =
            PluginTestHarness::builder().config(config).build().await;

        let service = test_harness.router_service(move |mut req| {
            let expected_error = expected_error.clone();
            async move {
                let err = req
                    .router_request
                    .body_mut()
                    .collect()
                    .await
                    .expect_err("request decompression fails");

                assert_eq!(err.to_string(), expected_error);

                Ok(router::Response::fake_builder().build().unwrap())
            }
        });

        let response = service.call(req).await.unwrap();
        assert_eq!(response.response.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn test_gzip_request_decompression_failure() {
        test_request_decompression_failure(
            router::Request::fake_builder()
                .header(header::CONTENT_ENCODING, "gzip")
                .header(header::CONTENT_TYPE, "text/plain")
                .body("not a compressed body")
                .build()
                .unwrap(),
            "Invalid gzip header".to_owned(),
        )
        .await;
    }

    #[tokio::test]
    async fn test_deflate_request_decompression_failure() {
        test_request_decompression_failure(
            router::Request::fake_builder()
                .header(header::CONTENT_ENCODING, "deflate")
                .header(header::CONTENT_TYPE, "text/plain")
                .body("not a compressed body")
                .build()
                .unwrap(),
            "deflate decompression error".to_owned(),
        )
        .await;
    }

    #[tokio::test]
    async fn test_br_request_decompression_failure() {
        test_request_decompression_failure(
            router::Request::fake_builder()
                .header(header::CONTENT_ENCODING, "br")
                .header(header::CONTENT_TYPE, "text/plain")
                .body("not a compressed body")
                .build()
                .unwrap(),
            "brotli error".to_owned(),
        )
        .await;
    }
}
