use crate::plugin::{PluginInit, PluginPrivate};
use crate::services::router;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::{ServiceBuilder, ServiceExt};

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
    use crate::plugin::{PluginInit, PluginPrivate};
    use super::{RequestDecompressionConfig, RequestDecompressionPlugin};

    #[tokio::test]
    async fn test_config() {
        let config = serde_yaml::from_str::<RequestDecompressionConfig>(
            r#"
        request_decompression:
          br: disable
          gzip: disable
          deflate: disable
        "#,
        )
            .unwrap();

        let plugin = RequestDecompressionPlugin::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(plugin.br, false);
        assert_eq!(plugin.gzip, false);
        assert_eq!(plugin.deflate, false);
    }
}
